//! The HTTP surface, over a real socket.
//!
//! These drive the router that the binary serves, on a loopback port, with a
//! real HTTP client. Not a mocked service: status codes, headers and the
//! absence of headers are part of what is being tested, and a `oneshot` against
//! a `Service` would not exercise them.
//!
//! The identity source is a fake — the alternative is a running 247streams —
//! but it is the *only* fake: the router, the token, the job registry, the
//! media resolution and the state files are all the real ones.

mod common;

use common::*;
use louver_core::streaming::ffmpeg::FfmpegTools;
use louver_core::OutputProfile;
use louver_live_source::{
    api::Api,
    auth::{Identity, IdentitySource},
    jobs::{Registry, Settings},
    limits::Limits,
    media::MediaRoot,
    resolver::{LiveSourceResolver, ResolvedSource},
    token::{Claims, Signer},
    Result,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const SECRET: &str = "a-test-secret-long-enough-to-pass-32";
/// Looks like a real stream key, so a leak is unmistakable.
const STREAM_KEY: &str = "abcd-efgh-ijkl-mnop-qrst";

/// Maps a cookie to a user. Two users, so cross-tenant access is testable.
struct Cookies;
impl IdentitySource for Cookies {
    fn whoami(&self, cookie_header: &str) -> Result<Identity> {
        let v = cookie_header.strip_prefix("louver_session=").unwrap_or_default();
        match v {
            "alice" => Ok(Identity { user_id: "user-alice".into(), plan: "business".into() }),
            "bob" => Ok(Identity { user_id: "user-bob".into(), plan: "basic".into() }),
            _ => Err(louver_live_source::LiveSourceError::unauthorized("로그인이 필요합니다.")),
        }
    }
}

struct Never;
impl LiveSourceResolver for Never {
    fn resolve(&self, _: &str) -> Result<ResolvedSource> {
        Err(louver_live_source::LiveSourceError::unavailable("테스트 환경에서는 해석하지 않습니다."))
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    base: String,
    state_dir: PathBuf,
    signer: Signer,
    jobs: Arc<Registry>,
}

fn rig(max_concurrent: usize) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("media");
    std::fs::create_dir_all(&media).unwrap();
    for n in ["song-a.mp4", "song-b.mp4"] {
        std::fs::write(media.join(n), b"x").unwrap();
    }
    std::fs::write(dir.path().join("secret.txt"), b"outside the root").unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let mut dests = BTreeMap::new();
    // Port 1 refuses instantly, so a job starts and then fails, which is what
    // keeps these tests fast without pretending FFmpeg is not involved.
    dests.insert("test-sink".to_string(), format!("rtmp://127.0.0.1:1/live/{STREAM_KEY}"));

    let settings = Settings {
        state_dir: state_dir.clone(),
        media: MediaRoot::new(&media, dests).unwrap(),
        tools: FfmpegTools::new(ffmpeg(), ffprobe()),
        profile: OutputProfile::P1080p30,
        limits: Limits { max_concurrent },
        // High, so a job stays "meant to be running" for the whole of a test
        // rather than giving up underneath an assertion.
        max_restarts: 50,
        stall_after: Duration::from_secs(60),
        grace: Duration::from_secs(60),
    };
    let jobs = Arc::new(Registry::new(settings, Arc::new(Never)));
    let signer = Signer::new(SECRET).unwrap();
    let api = Api {
        jobs: Arc::clone(&jobs),
        signer: Arc::new(Signer::new(SECRET).unwrap()),
        identity: Arc::new(Cookies),
    };

    let port = free_port();
    let app = api.router();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let l = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.unwrap();
            axum::serve(l, app).await.unwrap();
        });
    });
    let base = format!("http://127.0.0.1:{port}");
    // Wait for the listener rather than sleeping a guess.
    assert!(
        wait_until(Duration::from_secs(10), || ureq::get(&format!("{base}/api/live-source/health"))
            .call()
            .is_ok()),
        "api did not come up"
    );
    Rig { _dir: dir, base, state_dir, signer, jobs }
}

/// One request. Returns `(status, body)` — the body is read either way, because
/// an error body is part of the contract.
fn req(
    method: &str,
    url: &str,
    token: Option<&str>,
    cookie: Option<&str>,
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    // ureq 3 types the builder by whether it carries a body, so POST cannot
    // share a variable with GET and DELETE.
    let mut resp = if method == "POST" {
        let mut b = agent.post(url);
        if let Some(t) = token {
            b = b.header("Authorization", &format!("Bearer {t}"));
        }
        if let Some(c) = cookie {
            b = b.header("Cookie", c);
        }
        match body {
            Some(v) => b.send_json(&v).unwrap(),
            None => b.send_empty().unwrap(),
        }
    } else {
        let mut b = match method {
            "GET" => agent.get(url),
            "DELETE" => agent.delete(url),
            m => panic!("unsupported {m}"),
        };
        if let Some(t) = token {
            b = b.header("Authorization", &format!("Bearer {t}"));
        }
        if let Some(c) = cookie {
            b = b.header("Cookie", c);
        }
        b.call().unwrap()
    };
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    let json = serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text));
    (status, json)
}

impl Rig {
    fn url(&self, path: &str) -> String {
        format!("{}/api/live-source{path}", self.base)
    }

    /// The handshake, which is the only place a cookie is used.
    fn token_for(&self, who: &str) -> String {
        let (s, body) =
            req("POST", &self.url("/session"), None, Some(&format!("louver_session={who}")), None);
        assert_eq!(s, 200, "{body}");
        body["token"].as_str().unwrap().to_string()
    }

    fn new_job(&self, id: &str) -> serde_json::Value {
        serde_json::json!({
            "broadcast_id": id,
            // A public address that will not answer: the job starts, FFmpeg
            // fails, and the registry keeps retrying — so it stays running.
            "source_url": "https://93.184.216.34/live/stream.m3u8",
            "playlist": ["song-a.mp4", "song-b.mp4"],
            "destination": "test-sink"
        })
    }
}

/* --------------------------------------------------- the happy path */

#[test]
fn a_job_can_be_created_read_listed_and_cancelled() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let r = rig(2);
    let t = r.token_for("alice");

    let (s, j) = req("POST", &r.url("/jobs"), Some(&t), None, Some(r.new_job("live-1")));
    assert_eq!(s, 201, "{j}");
    assert_eq!(j["broadcast_id"], "live-1");
    assert_eq!(j["desired"], "running");

    let (s, got) = req("GET", &r.url("/jobs/live-1"), Some(&t), None, None);
    assert_eq!(s, 200, "{got}");
    assert_eq!(got["broadcast_id"], "live-1");

    let (s, list) = req("GET", &r.url("/jobs"), Some(&t), None, None);
    assert_eq!(s, 200);
    assert_eq!(list["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(list["max_concurrent"], 2);

    let (s, off) = req("DELETE", &r.url("/jobs/live-1"), Some(&t), None, None);
    assert_eq!(s, 200, "{off}");
    assert_eq!(off["desired"], "stopped", "cancel records the instruction");
    r.jobs.shutdown();
}

/* ------------------------------------------------------ idempotency */

#[test]
fn posting_the_same_broadcast_id_twice_returns_the_same_job() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let r = rig(2);
    let t = r.token_for("alice");
    let (s1, a) = req("POST", &r.url("/jobs"), Some(&t), None, Some(r.new_job("dup-1")));
    let (s2, b) = req("POST", &r.url("/jobs"), Some(&t), None, Some(r.new_job("dup-1")));
    assert_eq!(s1, 201);
    assert_eq!(s2, 201, "a repeat is not an error: {b}");
    assert_eq!(a["broadcast_id"], b["broadcast_id"]);
    // One job, not two — which is the point: two senders on one ingest URL is
    // worse for a viewer than none.
    let (_, list) = req("GET", &r.url("/jobs"), Some(&t), None, None);
    assert_eq!(list["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(list["running"], 1, "only one worker was started");
    r.jobs.shutdown();
}

/* ------------------------------------------- one user, one set of jobs */

#[test]
fn one_user_cannot_see_read_or_cancel_another_users_job() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let r = rig(3);
    let (alice, bob) = (r.token_for("alice"), r.token_for("bob"));
    let (s, _) = req("POST", &r.url("/jobs"), Some(&alice), None, Some(r.new_job("alice-job")));
    assert_eq!(s, 201);

    // Reading it as Bob: 404, not 403. A 403 would confirm the id exists, which
    // is the whole value of guessing ids.
    let (s, body) = req("GET", &r.url("/jobs/alice-job"), Some(&bob), None, None);
    assert_eq!(s, 404, "{body}");
    assert_eq!(body["error"], "not_found");

    // Cancelling it as Bob: also 404, and Alice's job is untouched.
    let (s, _) = req("DELETE", &r.url("/jobs/alice-job"), Some(&bob), None, None);
    assert_eq!(s, 404);
    let (s, still) = req("GET", &r.url("/jobs/alice-job"), Some(&alice), None, None);
    assert_eq!(s, 200);
    assert_eq!(still["desired"], "running", "Bob's request must not have stopped it");

    // Bob's list does not mention it.
    let (_, list) = req("GET", &r.url("/jobs"), Some(&bob), None, None);
    assert_eq!(list["jobs"].as_array().unwrap().len(), 0);

    // And Bob cannot take the id over.
    let (s, body) = req("POST", &r.url("/jobs"), Some(&bob), None, Some(r.new_job("alice-job")));
    assert_eq!(s, 409, "{body}");
    r.jobs.shutdown();
}

/* ------------------------------------------------- no authentication */

#[test]
fn every_job_route_refuses_a_request_with_no_credential() {
    let r = rig(1);
    for (m, p) in
        [("GET", "/jobs"), ("POST", "/jobs"), ("GET", "/jobs/x"), ("DELETE", "/jobs/x"), ("POST", "/check")]
    {
        let body = (m == "POST").then(|| serde_json::json!({"source_url":"https://youtu.be/dQw4w9WgXcQ"}));
        let (s, got) = req(m, &r.url(p), None, None, body);
        assert_eq!(s, 401, "{m} {p} → {got}");
        assert_eq!(got["error"], "unauthorized");
    }
    // Health is open on purpose and says nothing about any user.
    let (s, h) = req("GET", &r.url("/health"), None, None, None);
    assert_eq!(s, 200);
    assert!(h.get("jobs").is_none(), "health must not list jobs: {h}");
}

#[test]
fn a_forged_or_stale_token_is_refused() {
    let r = rig(1);
    let other = Signer::new("a-completely-different-secret-key-32!").unwrap();
    let forged = other.mint(&Claims::new("user-alice", "business", chrono::Utc::now().timestamp()));
    let expired = r.signer.mint(&Claims::new("user-alice", "business", 1_000));
    for bad in [forged.as_str(), expired.as_str(), "nonsense", "a.b", ""] {
        let (s, got) = req("GET", &r.url("/jobs"), Some(bad), None, None);
        assert_eq!(s, 401, "{bad:?} → {got}");
    }
}

#[test]
fn a_cookie_alone_does_not_open_the_job_routes() {
    // The cookie is good for the handshake and for nothing else. If it worked
    // on every route, Caddy stripping it would break the API — and the design
    // is that it does not.
    let r = rig(1);
    let (s, got) = req("GET", &r.url("/jobs"), None, Some("louver_session=alice"), None);
    assert_eq!(s, 401, "{got}");
}

#[test]
fn an_unknown_cookie_cannot_get_a_token() {
    let r = rig(1);
    for c in ["louver_session=nobody", "other=alice", ""] {
        let (s, got) = req("POST", &r.url("/session"), None, Some(c), None);
        assert_eq!(s, 401, "{c:?} → {got}");
    }
}

/* ------------------------------------------------------------- CSRF */

#[test]
fn the_api_sends_no_cors_headers_so_another_origin_cannot_call_it() {
    // The session cookie is `SameSite=Strict`, and this API adds no
    // `Access-Control-Allow-*`. A page on another origin therefore cannot read
    // a response from here even if it can send a request — and the handshake
    // would not get a cookie anyway.
    let r = rig(1);
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let mut resp = agent
        .post(&r.url("/session"))
        .header("Origin", "https://evil.example")
        .header("Cookie", "louver_session=alice")
        .send_empty()
        .unwrap();
    let names: Vec<String> = resp.headers().keys().map(|k| k.as_str().to_ascii_lowercase()).collect();
    for h in &names {
        assert!(!h.starts_with("access-control-"), "CORS header present: {h} in {names:?}");
    }
    let _ = resp.body_mut().read_to_string();
}

/* --------------------------------------------------- bad requests */

#[test]
fn a_playlist_name_cannot_escape_the_media_directory() {
    let r = rig(1);
    let t = r.token_for("alice");
    for bad in
        ["../secret.txt", "../../etc/passwd", "/etc/passwd", "sub/x.mp4", "song-a.mp4\0", ".hidden", ""]
    {
        let body = serde_json::json!({
            "broadcast_id": "path-1",
            "source_url": "https://93.184.216.34/live/stream.m3u8",
            "playlist": [bad],
            "destination": "test-sink"
        });
        let (s, got) = req("POST", &r.url("/jobs"), Some(&t), None, Some(body));
        assert_eq!(s, 400, "{bad:?} was accepted → {got}");
        // The refusal must not echo the server's filesystem layout.
        let msg = got["message"].as_str().unwrap_or_default();
        assert!(!msg.contains("/tmp"), "path leaked: {msg}");
    }
    // And nothing was created.
    let (_, list) = req("GET", &r.url("/jobs"), Some(&t), None, None);
    assert_eq!(list["jobs"].as_array().unwrap().len(), 0);
}

#[test]
fn a_destination_cannot_be_supplied_as_a_url() {
    // The RTMP URL carries the stream key, so a client names a destination and
    // never sends one. A client that could send one could redirect somebody
    // else's picture and music to its own ingest.
    let r = rig(1);
    let t = r.token_for("alice");
    for bad in ["rtmp://evil.example/live/steal", "test-sink-x", "", "TEST-SINK"] {
        let body = serde_json::json!({
            "broadcast_id": "dest-1",
            "source_url": "https://93.184.216.34/live/stream.m3u8",
            "playlist": ["song-a.mp4"],
            "destination": bad
        });
        let (s, got) = req("POST", &r.url("/jobs"), Some(&t), None, Some(body));
        assert_eq!(s, 400, "{bad:?} → {got}");
        assert_eq!(got["message"], "등록되지 않은 송출 대상입니다.");
    }
}

#[test]
fn an_internal_or_malformed_source_url_is_refused() {
    // SSRF: the server opens this address, so it must not be able to be aimed
    // at the server itself or at a metadata endpoint.
    let r = rig(1);
    let t = r.token_for("alice");
    for bad in [
        "http://127.0.0.1:8080/api/me",
        "http://169.254.169.254/latest/meta-data/",
        "http://10.0.0.5/s.m3u8",
        "http://localhost/health",
        "file:///etc/passwd",
        "ytsearch:live",
        "https://www.youtube.com/@channel",
        "--exec=rm",
        "",
    ] {
        let body = serde_json::json!({
            "broadcast_id": "ssrf-1",
            "source_url": bad,
            "playlist": ["song-a.mp4"],
            "destination": "test-sink"
        });
        let (s, got) = req("POST", &r.url("/jobs"), Some(&t), None, Some(body));
        assert_eq!(s, 400, "{bad:?} was accepted → {got}");
    }
    // `check` refuses the same shapes without starting a subprocess.
    let (s, _) = req(
        "POST",
        &r.url("/check"),
        Some(&t),
        None,
        Some(serde_json::json!({"source_url":"http://127.0.0.1/x"})),
    );
    assert_eq!(s, 400);
}

#[test]
fn a_bad_broadcast_id_is_refused_before_anything_is_written() {
    let r = rig(1);
    let t = r.token_for("alice");
    for bad in ["", "   ", "../../etc", "a/b", "a b", &"x".repeat(65), "a.b"] {
        let mut body = r.new_job("placeholder");
        body["broadcast_id"] = serde_json::json!(bad);
        let (s, got) = req("POST", &r.url("/jobs"), Some(&t), None, Some(body));
        assert_eq!(s, 400, "{bad:?} → {got}");
    }
    // No stray state files from the refusals.
    assert_eq!(louver_live_source::state::scan(&r.state_dir).len(), 0);
}

/* ---------------------------------------------------- the ceiling */

#[test]
fn the_concurrency_ceiling_refuses_the_next_job_with_429() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let r = rig(2);
    let alice = r.token_for("alice");
    let bob = r.token_for("bob");
    for id in ["c-1", "c-2"] {
        let (s, got) = req("POST", &r.url("/jobs"), Some(&alice), None, Some(r.new_job(id)));
        assert_eq!(s, 201, "{id} → {got}");
    }
    // The ceiling is about the machine, so Bob is refused by Alice's load.
    let (s, got) = req("POST", &r.url("/jobs"), Some(&bob), None, Some(r.new_job("c-3")));
    assert_eq!(s, 429, "{got}");
    assert_eq!(got["error"], "limit");
    assert!(got["message"].as_str().unwrap().contains('2'), "{got}");

    // Freeing one lets the next in.
    let (s, _) = req("DELETE", &r.url("/jobs/c-1"), Some(&alice), None, None);
    assert_eq!(s, 200);
    let (s, got) = req("POST", &r.url("/jobs"), Some(&bob), None, Some(r.new_job("c-3")));
    assert_eq!(s, 201, "{got}");
    r.jobs.shutdown();
}

/* ------------------------------------------- secrets, over the wire */

#[test]
fn no_response_and_no_state_file_carries_the_destination() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let r = rig(1);
    let t = r.token_for("alice");
    let (_, created) = req("POST", &r.url("/jobs"), Some(&t), None, Some(r.new_job("sec-1")));
    let (_, got) = req("GET", &r.url("/jobs/sec-1"), Some(&t), None, None);
    let (_, list) = req("GET", &r.url("/jobs"), Some(&t), None, None);
    let (_, sess) = req("POST", &r.url("/session"), None, Some("louver_session=alice"), None);

    for (what, v) in [("create", &created), ("get", &got), ("list", &list), ("session", &sess)] {
        let s = v.to_string();
        for needle in [STREAM_KEY, "rtmp://", "rtmp", "user-alice", "/tmp/"] {
            assert!(!s.contains(needle), "{what} leaked {needle}: {s}");
        }
    }
    // The session body names destinations, but by name only.
    assert_eq!(sess["destinations"], serde_json::json!(["test-sink"]));

    // On disk too: the state file and the saved request.
    for f in std::fs::read_dir(&r.state_dir).unwrap().flatten() {
        if f.path().is_file() {
            let body = std::fs::read_to_string(f.path()).unwrap_or_default();
            assert!(!body.contains(STREAM_KEY), "{:?} leaked the key", f.path());
            assert!(!body.contains("rtmp"), "{:?} holds a URL", f.path());
        }
    }
    let saved = std::fs::read_to_string(r.state_dir.join("job-sec-1").join("request.json")).unwrap();
    assert!(saved.contains("test-sink"), "the name is kept, for recovery");
    assert!(!saved.contains(STREAM_KEY), "{saved}");
    r.jobs.shutdown();
}

/* ------------------------------------------------------- the beta page */

#[test]
fn the_beta_page_is_served_from_the_same_origin_as_the_api() {
    // The page has to be same-origin with 247streams for the cookie handshake
    // to work at all, which is why this worker serves it rather than Caddy
    // serving it from a bind mount (that would need the caddy container
    // recreated).
    let r = rig(1);
    let beta = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("beta");
    let api = Api {
        jobs: Arc::clone(&r.jobs),
        signer: Arc::new(Signer::new(SECRET).unwrap()),
        identity: Arc::new(Cookies),
    };
    let port = free_port();
    let app = api.router_with_beta(&beta);
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let l = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.unwrap();
            axum::serve(l, app).await.unwrap();
        });
    });
    let base = format!("http://127.0.0.1:{port}");
    assert!(
        wait_until(Duration::from_secs(10), || ureq::get(&format!("{base}/beta/")).call().is_ok()),
        "beta page did not come up"
    );
    let mut resp = ureq::get(&format!("{base}/beta/")).call().unwrap();
    let html = resp.body_mut().read_to_string().unwrap();
    assert!(html.contains("YouTube Live 영상 소스"), "the page should be the beta UI");
    // It must not ship a token or a destination.
    assert!(!html.contains("rtmp"), "no destination in the page source");
    assert!(!html.contains(SECRET), "no secret in the page source");
    // And it talks to the same origin, with no absolute URL to another host.
    assert!(html.contains("\"/api/live-source\""), "same-origin fetch base expected");
    assert!(!html.contains("https://247streams.kr/api"), "no cross-origin call");
    // The API is still reachable alongside it.
    let (s, _) = req("GET", &format!("{base}/api/live-source/health"), None, None, None);
    assert_eq!(s, 200);
}
