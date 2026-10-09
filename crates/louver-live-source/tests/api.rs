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
//!
//! Every request carries the gate header, because every request has to: the
//! gate is checked before anything else, so a test that forgot it would be
//! testing the gate and nothing more. [`req`] adds it, and [`req_full`] is the
//! way to leave it out or get it wrong on purpose.

mod common;

use common::*;
use louver_core::streaming::ffmpeg::FfmpegTools;
use louver_core::OutputProfile;
use louver_live_source::{
    api::Api,
    auth::{Identity, IdentitySource},
    destinations::Destinations,
    gate::Gate,
    jobs::{Registry, Settings},
    limits::Limits,
    media::MediaRoot,
    origin::AllowedOrigins,
    resolver::{LiveSourceResolver, ResolvedSource},
    token::{Claims, Signer},
    Result,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const SECRET: &str = "a-test-secret-long-enough-to-pass-32";
/// The shared secret between the proxy and the worker.
const GATE: &str = "a-test-gate-secret-also-32-bytes-long";
/// The one origin a session may be started from in these tests.
const ORIGIN: &str = "https://beta.test";
/// Looks like a real stream key, so a leak is unmistakable.
const STREAM_KEY: &str = "abcd-efgh-ijkl-mnop-qrst";
/// Alice's, and only Alice's.
const ALICE_KEY: &str = "alice-only-key-zzzz-yyyy";

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
    media_dir: PathBuf,
    signer: Signer,
    jobs: Arc<Registry>,
}

/// The destinations these tests run against.
///
/// Port 1 refuses instantly, so a job starts and then fails, which is what
/// keeps these tests fast without pretending FFmpeg is not involved.
///
/// Alice has a second one nobody else has, so "another user's destination by
/// name" is a thing a test can actually try.
fn test_destinations() -> Destinations {
    let sink = |key: &str| format!("rtmp://127.0.0.1:1/live/{key}");
    let mut by_user: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    by_user.insert(
        "user-alice".into(),
        BTreeMap::from([
            ("test-sink".to_string(), sink(STREAM_KEY)),
            ("alice-only".to_string(), sink(ALICE_KEY)),
        ]),
    );
    by_user.insert("user-bob".into(), BTreeMap::from([("test-sink".to_string(), sink(STREAM_KEY))]));
    Destinations::from_map(by_user).unwrap()
}

fn api_for(jobs: &Arc<Registry>) -> Api {
    Api {
        jobs: Arc::clone(jobs),
        signer: Arc::new(Signer::new(SECRET).unwrap()),
        identity: Arc::new(Cookies),
        gate: Arc::new(Gate::new(GATE).unwrap()),
        origins: Arc::new(AllowedOrigins::parse(ORIGIN).unwrap()),
    }
}

fn rig(max_concurrent: usize) -> Rig {
    rig_limits(Limits { max_concurrent, max_per_user: max_concurrent })
}

fn rig_limits(limits: Limits) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("media");
    std::fs::create_dir_all(&media).unwrap();
    std::fs::write(dir.path().join("secret.txt"), b"outside the root").unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let media_root = MediaRoot::new(&media, FfmpegTools::new(ffmpeg(), ffprobe())).unwrap();
    // Each user's own directory, and each user's own copies. Planted directly
    // rather than uploaded: these tests are about the API, and `media.rs`'s own
    // tests cover the upload checks.
    for user in ["user-alice", "user-bob"] {
        let d = media_root.dir_for(user).unwrap();
        for n in ["song-a.mp4", "song-b.mp4"] {
            std::fs::write(d.join(n), b"x").unwrap();
        }
    }
    // One file only Alice has, for the cross-user media test.
    std::fs::write(media_root.dir_for("user-alice").unwrap().join("alice-secret.mp4"), b"x").unwrap();

    let settings = Settings {
        state_dir: state_dir.clone(),
        media: media_root,
        destinations: test_destinations(),
        tools: FfmpegTools::new(ffmpeg(), ffprobe()),
        profile: OutputProfile::P1080p30,
        limits,
        // High, so a job stays "meant to be running" for the whole of a test
        // rather than giving up underneath an assertion.
        max_restarts: 50,
        stall_after: Duration::from_secs(60),
        grace: Duration::from_secs(60),
    };
    let jobs = Arc::new(Registry::new(settings, Arc::new(Never)));
    let signer = Signer::new(SECRET).unwrap();
    let api = api_for(&jobs);

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
    // Wait for the listener rather than sleeping a guess. The health route is
    // gated like everything else, so the probe carries the header.
    assert!(
        wait_until(Duration::from_secs(10), || ureq::get(&format!(
            "{base}/api/live-source/health"
        ))
        .header("X-Louver-Gate", GATE)
        .call()
        .is_ok()),
        "api did not come up"
    );
    Rig { _dir: dir, base, state_dir, media_dir: media, signer, jobs }
}

/// One request, as the proxy would send it: correct gate header, and the
/// allowed `Origin` whenever a cookie rides along.
///
/// Returns `(status, body)` — the body is read either way, because an error
/// body is part of the contract.
fn req(
    method: &str,
    url: &str,
    token: Option<&str>,
    cookie: Option<&str>,
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    let origin = cookie.map(|_| ORIGIN);
    req_full(method, url, token, cookie, origin, Some(GATE), body)
}

/// One request with every header under the test's control, for the cases that
/// are *about* a missing or wrong header.
#[allow(clippy::too_many_arguments)]
fn req_full(
    method: &str,
    url: &str,
    token: Option<&str>,
    cookie: Option<&str>,
    origin: Option<&str>,
    gate: Option<&str>,
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
        if let Some(o) = origin {
            b = b.header("Origin", o);
        }
        if let Some(g) = gate {
            b = b.header("X-Louver-Gate", g);
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
        if let Some(o) = origin {
            b = b.header("Origin", o);
        }
        if let Some(g) = gate {
            b = b.header("X-Louver-Gate", g);
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
    // Health needs no user, but it still needs the gate, and it says nothing
    // about any user.
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
    //
    // It used to be a 401 (no usable credential). It is now a 403: the cookie
    // is refused outright on this route, before the absence of a token is even
    // considered, so a proxy that started forwarding it fails loudly. Both are
    // a refusal; this one is the stricter of the two.
    let r = rig(1);
    let (s, got) = req("GET", &r.url("/jobs"), None, Some("louver_session=alice"), None);
    assert_eq!(s, 403, "{got}");
    assert_eq!(got["error"], "forbidden");
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
        .header("X-Louver-Gate", GATE)
        .header("Origin", "https://evil.example")
        .header("Cookie", "louver_session=alice")
        .send_empty()
        .unwrap();
    // The wrong origin is also refused outright, which is the other half of
    // the CSRF answer and is tested on its own below.
    assert_eq!(resp.status().as_u16(), 401);
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
    // The session body names destinations, but by name only — and only this
    // user's, which is what `a_session_lists_only_this_users_destinations`
    // covers in full.
    assert_eq!(sess["destinations"], serde_json::json!(["alice-only", "test-sink"]));

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
    let api = api_for(&r.jobs);
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
    let get = |path: &str| ureq::get(&format!("{base}{path}")).header("X-Louver-Gate", GATE).call();
    assert!(
        wait_until(Duration::from_secs(10), || get("/beta/").is_ok()),
        "beta page did not come up"
    );
    let mut resp = get("/beta/").unwrap();
    let html = resp.body_mut().read_to_string().unwrap();
    assert!(html.contains("YouTube Live 영상 소스"), "the page should be the beta UI");
    // It must not ship a token or a destination.
    assert!(!html.contains("rtmp"), "no destination in the page source");
    assert!(!html.contains(SECRET), "no secret in the page source");
    // No inline script and no inline style, which is what lets the content
    // policy be `script-src 'self'` rather than `'unsafe-inline'`. This is the
    // assertion that would catch somebody moving the code back inline and
    // silently breaking the page under its own CSP.
    assert!(!html.contains("<script>"), "an inline script would need 'unsafe-inline': {html}");
    assert!(!html.contains("<style>"), "an inline style would need 'unsafe-inline'");
    assert!(html.contains("src=\"app.js\""), "the script is a separate file");
    assert!(html.contains("href=\"app.css\""), "the style is a separate file");

    // The script it loads talks to the same origin, with no absolute URL to
    // another host.
    let mut js = get("/beta/app.js").unwrap();
    let js = js.body_mut().read_to_string().unwrap();
    assert!(js.contains("\"/api/live-source\""), "same-origin fetch base expected");
    assert!(!js.contains("https://247streams.kr/api"), "no cross-origin call");
    // The token is never written anywhere that outlives the tab. Matched with
    // the dot, so the file may still explain in a comment why it does not use
    // these.
    assert!(!js.contains("localStorage."), "a stored token survives an XSS at leisure");
    assert!(!js.contains("sessionStorage."), "likewise");
    assert!(!js.contains("document.cookie"), "this page sets no cookie of its own");

    // Served under the gate, like everything else: without the header the page
    // is a 403 too.
    let (s, _) = req_full("GET", &format!("{base}/beta/"), None, None, None, None, None);
    assert_eq!(s, 403, "the static page is behind the gate as well");

    // The API is still reachable alongside it.
    let (s, _) = req("GET", &format!("{base}/api/live-source/health"), None, None, None);
    assert_eq!(s, 200);
}

/* =========================================================== PHASE 4 ======
 *
 * The checks added before this API is reachable from a customer's browser:
 * the gate, the origin, the cookie refusal, the response headers, per-user
 * destinations and media, the per-user ceiling, and the token's five minutes.
 */

/* ------------------------------------------------------------- the gate */

#[test]
fn every_route_refuses_a_request_without_the_gate_header() {
    // The worker's port is reachable by anything that can route to it until a
    // firewall is in place. This is what makes it useless to everything but
    // 247streams' own proxy.
    let r = rig(1);
    let t = r.signer.mint(&Claims::new("user-alice", "business", chrono::Utc::now().timestamp()));
    for (m, p) in [
        ("GET", "/health"),
        ("POST", "/session"),
        ("GET", "/jobs"),
        ("POST", "/jobs"),
        ("GET", "/jobs/x"),
        ("DELETE", "/jobs/x"),
        ("POST", "/check"),
        ("GET", "/media"),
        ("DELETE", "/media/x.mp4"),
    ] {
        let body = (m == "POST").then(|| serde_json::json!({"source_url":"https://youtu.be/dQw4w9WgXcQ"}));
        let (s, got) =
            req_full(m, &r.url(p), Some(&t), None, Some(ORIGIN), None, body.clone());
        assert_eq!(s, 403, "{m} {p} with no gate → {got}");
        assert_eq!(got["error"], "forbidden");

        // A wrong one is the same answer: a different reply would say whether
        // the header name was right.
        let (s2, got2) = req_full(
            m,
            &r.url(p),
            Some(&t),
            None,
            Some(ORIGIN),
            Some("the-wrong-gate-secret-32-bytes-long!"),
            body,
        );
        assert_eq!(s2, 403, "{m} {p} with a wrong gate → {got2}");
        assert_eq!(got2["message"], got["message"]);
    }
}

#[test]
fn the_gate_is_not_authentication() {
    // The whole point of the double check: a leaked gate secret gets an
    // attacker as far as an unauthenticated 401 and no further. If this test
    // ever reads 200, the gate has become the only thing standing between a
    // stranger and a customer's broadcasts.
    let r = rig(1);
    for (m, p) in [("GET", "/jobs"), ("GET", "/jobs/x"), ("DELETE", "/jobs/x"), ("GET", "/media")] {
        let (s, got) = req(m, &r.url(p), None, None, None);
        assert_eq!(s, 401, "{m} {p} with the gate but no token → {got}");
    }
    // And the handshake still needs the cookie production vouches for.
    let (s, _) = req_full("POST", &r.url("/session"), None, None, Some(ORIGIN), Some(GATE), None);
    assert_eq!(s, 401);
}

/* ----------------------------------------------------------- the origin */

#[test]
fn a_session_is_refused_without_the_right_origin() {
    // CSRF: this is the one route that uses a credential the browser attaches
    // on its own, so it is the one route where a cross-site request could
    // achieve anything.
    let r = rig(1);
    let cookie = Some("louver_session=alice");
    for origin in [
        None,
        Some("null"),
        Some("https://evil.test"),
        Some("https://beta.test.evil.test"),
        Some("https://evil-beta.test"),
        Some("http://beta.test"),
        Some("https://beta.test:8443"),
        Some("https://beta.test/"),
    ] {
        let (s, got) = req_full("POST", &r.url("/session"), None, cookie, origin, Some(GATE), None);
        assert_eq!(s, 401, "{origin:?} was accepted → {got}");
        assert!(
            got["message"].as_str().unwrap_or_default().contains("Origin"),
            "{origin:?} → {got}"
        );
        // The rejected origin is attacker-controlled and is not echoed back.
        // `null` is excepted: it is a fixed value this code names on purpose,
        // not a string an attacker chose, and saying which of the three
        // refusals fired is useful to whoever is debugging a browser.
        if let Some(o) = origin {
            if o != "null" {
                assert!(!got.to_string().contains(o), "{o} echoed: {got}");
            }
        }
    }
    // And the right one works, which is what makes the refusals meaningful.
    let (s, ok) = req_full("POST", &r.url("/session"), None, cookie, Some(ORIGIN), Some(GATE), None);
    assert_eq!(s, 200, "{ok}");
}

#[test]
fn the_origin_check_is_only_on_the_handshake() {
    // The bearer routes do not need it: they carry a credential a browser does
    // not attach on its own, so a cross-site request cannot forge one. Adding
    // the check there would break a non-browser client for no security gain.
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let r = rig(1);
    let t = r.token_for("alice");
    let (s, got) = req_full(
        "GET",
        &r.url("/jobs"),
        Some(&t),
        None,
        Some("https://evil.test"),
        Some(GATE),
        None,
    );
    assert_eq!(s, 200, "{got}");
}

/* ----------------------------------------------------------- the cookie */

#[test]
fn a_cookie_is_refused_on_every_route_but_the_handshake() {
    // Caddy strips it. This refuses it as well, so a proxy that started
    // forwarding the session cookie here would fail loudly on the next request
    // instead of quietly handing this worker a credential it has no use for.
    let r = rig(1);
    let t = r.signer.mint(&Claims::new("user-alice", "business", chrono::Utc::now().timestamp()));
    for (m, p) in [
        ("GET", "/health"),
        ("GET", "/jobs"),
        ("POST", "/jobs"),
        ("GET", "/jobs/x"),
        ("DELETE", "/jobs/x"),
        ("POST", "/check"),
        ("GET", "/media"),
        ("DELETE", "/media/x.mp4"),
    ] {
        let body = (m == "POST").then(|| serde_json::json!({"source_url":"https://youtu.be/dQw4w9WgXcQ"}));
        let (s, got) = req(m, &r.url(p), Some(&t), Some("louver_session=alice"), body);
        assert_eq!(s, 403, "{m} {p} accepted a cookie → {got}");
        assert_eq!(got["error"], "forbidden");
        // The refusal must not quote the cookie back.
        assert!(!got.to_string().contains("louver_session"), "{m} {p} echoed the cookie: {got}");
        assert!(!got.to_string().contains("alice"), "{m} {p} echoed the cookie: {got}");
    }
    // The handshake is the exception, and it is the only one.
    let (s, ok) = req("POST", &r.url("/session"), None, Some("louver_session=alice"), None);
    assert_eq!(s, 200, "{ok}");
}

/* -------------------------------------------------- the response headers */

#[test]
fn every_response_carries_the_security_headers() {
    let r = rig(1);
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    // One that succeeds, one that is refused by the gate: both must carry them,
    // because the 403 is a response an attacker gets and a page could frame.
    for gate in [Some(GATE), None] {
        let mut b = agent.get(&r.url("/health"));
        if let Some(g) = gate {
            b = b.header("X-Louver-Gate", g);
        }
        let mut resp = b.call().unwrap();
        let h = resp.headers();
        let csp = h.get("content-security-policy").and_then(|v| v.to_str().ok()).unwrap_or("");
        assert!(csp.contains("default-src 'none'"), "{gate:?} → {csp:?}");
        assert!(csp.contains("script-src 'self'"), "{gate:?} → {csp:?}");
        assert!(csp.contains("frame-ancestors 'none'"), "{gate:?} → {csp:?}");
        // The relaxation that would give most of the policy's value back.
        assert!(!csp.contains("unsafe-inline"), "{gate:?} → {csp:?}");
        assert!(!csp.contains("unsafe-eval"), "{gate:?} → {csp:?}");
        for (name, want) in [
            ("x-frame-options", "DENY"),
            ("x-content-type-options", "nosniff"),
            ("referrer-policy", "no-referrer"),
            ("cross-origin-opener-policy", "same-origin"),
            ("cache-control", "no-store"),
        ] {
            let got = h.get(name).and_then(|v| v.to_str().ok()).unwrap_or("");
            assert_eq!(got, want, "{gate:?} {name}");
        }
        let _ = resp.body_mut().read_to_string();
    }
}

#[test]
fn a_session_reply_is_never_cached() {
    // It contains a bearer token.
    let r = rig(1);
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let mut resp = agent
        .post(&r.url("/session"))
        .header("X-Louver-Gate", GATE)
        .header("Origin", ORIGIN)
        .header("Cookie", "louver_session=alice")
        .send_empty()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(resp.headers().get("cache-control").unwrap().to_str().unwrap(), "no-store");
    let _ = resp.body_mut().read_to_string();
}

/* --------------------------------------------------- the five-minute token */

#[test]
fn the_token_lives_five_minutes_and_the_page_is_told_so() {
    let r = rig(1);
    assert_eq!(louver_live_source::token::TOKEN_TTL_SECS, 300);
    let (s, body) =
        req("POST", &r.url("/session"), None, Some("louver_session=alice"), None);
    assert_eq!(s, 200, "{body}");
    assert_eq!(body["expires_in"], 300, "the page renews on this number");

    // One second past it, the same token is refused. Minted directly, because
    // a test that waited five minutes would not be run.
    let t = r.signer.mint(&Claims::new("user-alice", "business", 1_000_000));
    let (s, got) = req("GET", &r.url("/jobs"), Some(&t), None, None);
    assert_eq!(s, 401, "a long-expired token → {got}");
}

#[test]
fn a_fresh_handshake_reissues_a_working_token() {
    // The re-authentication the page depends on: the cookie is still good, so a
    // second handshake produces a token that works. If this failed, every beta
    // session would die after five minutes.
    let r = rig(1);
    let first = r.token_for("alice");
    let second = r.token_for("alice");
    for t in [&first, &second] {
        let (s, got) = req("GET", &r.url("/jobs"), Some(t), None, None);
        assert_eq!(s, 200, "{got}");
    }
    // And the earlier one is not invalidated by the later one, so a request in
    // flight across a renewal does not fail.
    let (s, _) = req("GET", &r.url("/jobs"), Some(&first), None, None);
    assert_eq!(s, 200);
}

/* ------------------------------------------- per-user destinations */

#[test]
fn one_user_cannot_send_to_another_users_destination() {
    // The defect this closes: one flat destination map meant every beta user
    // could aim at every other user's ingest by naming it.
    let r = rig(2);
    let bob = r.token_for("bob");
    let body = serde_json::json!({
        "broadcast_id": "steal-1",
        "source_url": "https://93.184.216.34/live/stream.m3u8",
        "playlist": ["song-a.mp4"],
        "destination": "alice-only"
    });
    let (s, got) = req("POST", &r.url("/jobs"), Some(&bob), None, Some(body));
    assert_eq!(s, 400, "{got}");
    // The same words as a name nobody registered, so the refusal is not an
    // oracle for whether Alice's destination exists.
    assert_eq!(got["message"], "등록되지 않은 송출 대상입니다.");
    assert!(!got.to_string().contains(ALICE_KEY), "{got}");
    assert_eq!(louver_live_source::state::scan(&r.state_dir).len(), 0, "nothing was created");
}

#[test]
fn a_session_lists_only_this_users_destinations() {
    // A name is itself a leak: it says another customer exists and is enough to
    // try sending to.
    let r = rig(1);
    let (_, alice) = req("POST", &r.url("/session"), None, Some("louver_session=alice"), None);
    let (_, bob) = req("POST", &r.url("/session"), None, Some("louver_session=bob"), None);
    assert_eq!(alice["destinations"], serde_json::json!(["alice-only", "test-sink"]));
    assert_eq!(bob["destinations"], serde_json::json!(["test-sink"]));
    assert!(!bob.to_string().contains("alice-only"), "{bob}");
    for v in [&alice, &bob] {
        let s = v.to_string();
        for needle in [STREAM_KEY, ALICE_KEY, "rtmp"] {
            assert!(!s.contains(needle), "leaked {needle}: {s}");
        }
    }
}

/* -------------------------------------------------- per-user media */

#[test]
fn one_user_cannot_see_use_or_delete_another_users_media() {
    let r = rig(1);
    let (alice, bob) = (r.token_for("alice"), r.token_for("bob"));

    // Alice's listing has her own file; Bob's does not.
    let (s, mine) = req("GET", &r.url("/media"), Some(&alice), None, None);
    assert_eq!(s, 200, "{mine}");
    let names: Vec<String> = mine["media"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"alice-secret.mp4".to_string()), "{names:?}");

    let (_, theirs) = req("GET", &r.url("/media"), Some(&bob), None, None);
    assert!(!theirs.to_string().contains("alice-secret"), "{theirs}");

    // Bob cannot name it in a playlist…
    let body = serde_json::json!({
        "broadcast_id": "cross-1",
        "source_url": "https://93.184.216.34/live/stream.m3u8",
        "playlist": ["alice-secret.mp4"],
        "destination": "test-sink"
    });
    let (s, got) = req("POST", &r.url("/jobs"), Some(&bob), None, Some(body));
    assert_eq!(s, 404, "{got}");

    // …nor delete it.
    let (s, got) = req("DELETE", &r.url("/media/alice-secret.mp4"), Some(&bob), None, None);
    assert_eq!(s, 404, "{got}");
    assert!(
        r.media_dir.join("user-alice").join("alice-secret.mp4").is_file(),
        "Bob's request must not have removed Alice's file"
    );
}

#[test]
fn a_media_name_cannot_be_a_path() {
    let r = rig(1);
    let t = r.token_for("alice");
    // A name with a separator does not even reach the handler — axum's own
    // routing will not match it — and the rest are refused by the name check.
    for bad in ["..", "...", ".hidden", "x:y.mp4", "%2e%2e%2fsecret.txt", "%00.mp4"] {
        let (s, got) = req("DELETE", &r.url(&format!("/media/{bad}")), Some(&t), None, None);
        assert!(s == 400 || s == 404, "{bad:?} → {s} {got}");
        let msg = got["message"].as_str().unwrap_or_default();
        assert!(!msg.contains("/tmp"), "path leaked: {msg}");
    }
    // Nothing outside the user's own directory was touched.
    assert!(r.media_dir.parent().unwrap().join("secret.txt").is_file());
}

/* ------------------------------------------- the per-user ceiling */

#[test]
fn one_user_cannot_take_every_slot() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    // Three slots on the machine, one per user: without the second ceiling,
    // Alice alone could fill all three and Bob's request would be refused by
    // Alice's enthusiasm.
    let r = rig_limits(Limits { max_concurrent: 3, max_per_user: 1 });
    let (alice, bob) = (r.token_for("alice"), r.token_for("bob"));

    let (s, got) = req("POST", &r.url("/jobs"), Some(&alice), None, Some(r.new_job("mine-1")));
    assert_eq!(s, 201, "{got}");
    let (s, got) = req("POST", &r.url("/jobs"), Some(&alice), None, Some(r.new_job("mine-2")));
    assert_eq!(s, 429, "{got}");
    assert_eq!(got["error"], "limit");
    // It says it is *their* ceiling, because the remedy is different: wait for
    // your own broadcast to end, not for somebody else's.
    assert!(got["message"].as_str().unwrap().contains("한 계정"), "{got}");

    // And the machine still has room for Bob.
    let (s, got) = req("POST", &r.url("/jobs"), Some(&bob), None, Some(r.new_job("theirs-1")));
    assert_eq!(s, 201, "{got}");

    let (_, list) = req("GET", &r.url("/jobs"), Some(&alice), None, None);
    assert_eq!(list["running_mine"], 1);
    assert_eq!(list["running"], 2);
    assert_eq!(list["max_per_user"], 1);
    r.jobs.shutdown();
}

/* ------------------------------- the same id, from two requests at once */

#[test]
fn two_simultaneous_requests_for_one_broadcast_id_start_one_job() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    // Sequentially this is just a state-file read, and the idempotency test
    // above covers it. Concurrently it is the two-senders-on-one-ingest
    // failure: both requests miss the file, both pass the ceiling, both spawn.
    let r = rig_limits(Limits { max_concurrent: 4, max_per_user: 4 });
    let t = r.token_for("alice");
    let url = r.url("/jobs");
    let body = r.new_job("race-1");

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let (url, t, body) = (url.clone(), t.clone(), body.clone());
            std::thread::spawn(move || req("POST", &url, Some(&t), None, Some(body)))
        })
        .collect();
    let results: Vec<(u16, serde_json::Value)> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    // Every reply is either the job or a refusal — never a second job, and
    // never a 500.
    for (s, got) in &results {
        assert!(*s == 201 || *s == 409, "{s} {got}");
    }
    assert!(results.iter().any(|(s, _)| *s == 201), "one of them must have won: {results:?}");

    let (_, list) = req("GET", &url, Some(&t), None, None);
    assert_eq!(list["jobs"].as_array().unwrap().len(), 1, "one job: {list}");
    assert_eq!(list["running"], 1, "one worker: {list}");
    r.jobs.shutdown();
}

/* ------------------------------------------------- the upload route */

/// `PUT …/media/{name}` with raw bytes. Returns `(status, body)`.
fn put_media(r: &Rig, token: &str, name: &str, bytes: Vec<u8>) -> (u16, serde_json::Value) {
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let mut resp = agent
        .put(&r.url(&format!("/media/{name}")))
        .header("X-Louver-Gate", GATE)
        .header("Authorization", &format!("Bearer {token}"))
        .header("Content-Type", "application/octet-stream")
        .send(&bytes[..])
        .unwrap();
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    (status, serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text)))
}

/// A real, tiny media file. The upload check looks at the bytes, so a test of
/// it needs bytes that actually are media.
fn tiny_media() -> Option<Vec<u8>> {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("t.m4a");
    let ok = std::process::Command::new(ffmpeg())
        .args([
            "-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "sine=frequency=440:duration=1",
            "-c:a", "aac",
        ])
        .arg(&out)
        .status()
        .ok()?
        .success();
    ok.then(|| std::fs::read(&out).ok()).flatten()
}

#[test]
fn an_upload_is_stored_under_the_callers_own_directory() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let Some(bytes) = tiny_media() else {
        eprintln!("SKIP: could not make a test file");
        return;
    };
    let r = rig(1);
    let (alice, bob) = (r.token_for("alice"), r.token_for("bob"));

    let (s, got) = put_media(&r, &alice, "mine.m4a", bytes.clone());
    assert_eq!(s, 201, "{got}");
    assert_eq!(got["name"], "mine.m4a");
    assert_eq!(got["bytes"], bytes.len());
    assert_eq!(got["kind"], "audio");

    // In Alice's directory, and nowhere else.
    assert!(r.media_dir.join("user-alice").join("mine.m4a").is_file());
    assert!(!r.media_dir.join("user-bob").join("mine.m4a").exists());
    assert!(!r.media_dir.join("mine.m4a").exists(), "not in the shared root");

    // Bob's listing does not show it and Bob cannot delete it.
    let (_, theirs) = req("GET", &r.url("/media"), Some(&bob), None, None);
    assert!(!theirs.to_string().contains("mine.m4a"), "{theirs}");
    let (s, _) = req("DELETE", &r.url("/media/mine.m4a"), Some(&bob), None, None);
    assert_eq!(s, 404);
    assert!(r.media_dir.join("user-alice").join("mine.m4a").is_file(), "still Alice's");

    // Alice can, and then it is gone.
    let (s, left) = req("DELETE", &r.url("/media/mine.m4a"), Some(&alice), None, None);
    assert_eq!(s, 200, "{left}");
    assert!(!r.media_dir.join("user-alice").join("mine.m4a").exists());
}

#[test]
fn an_upload_that_is_not_media_is_refused_and_leaves_nothing_behind() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let r = rig(1);
    let t = r.token_for("alice");
    let dir = r.media_dir.join("user-alice");
    let before: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();

    // An extension is a claim. These are not media whatever they are called.
    for (name, body) in [
        ("fake.mp4", b"#!/bin/sh\nrm -rf /\n".to_vec()),
        ("fake.m4a", vec![0u8; 4096]),
        ("empty.mp4", Vec::new()),
        // A real media file under a name this will not store.
        ("script.sh", b"echo hi".to_vec()),
        ("note.txt", b"hello".to_vec()),
        ("no-extension", b"hello".to_vec()),
    ] {
        let (s, got) = put_media(&r, &t, name, body);
        assert_eq!(s, 400, "{name} was accepted → {got}");
        assert!(!got["message"].as_str().unwrap_or_default().contains("/tmp"), "path leaked: {got}");
    }

    // Nothing was left behind — not the files, and not a half-written
    // temporary either.
    let after: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(before.len(), after.len(), "a refused upload left something: {after:?}");
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        assert!(!n.starts_with(".tmp-"), "a temporary survived: {n}");
    }
}

#[test]
fn a_multi_megabyte_body_is_handled_and_a_refusal_stores_nothing() {
    // Deliberately *not* a test of the 512MB cap: two megabytes is under it,
    // so what this shows is that a body far past axum's 2MB default reaches
    // the handler at all (the route raises the limit) and that a refusal
    // leaves nothing behind. Sending 512MB to prove the cap would be a slow
    // test for an answer `media.rs`'s own tests already give, and a test named
    // after a cap it does not reach is worse than no test.
    let r = rig(1);
    let t = r.token_for("alice");
    let (s, got) = put_media(&r, &t, "big.mp4", vec![0u8; 2 * 1024 * 1024 + 1]);
    // Refused for not being media — which means it was read, not truncated.
    assert_eq!(s, 400, "{got}");
    assert_eq!(got["error"], "invalid", "a 413 here would mean the limit is still axum's default");
    assert!(!r.media_dir.join("user-alice").join("big.mp4").exists());
    // And the number the route is configured with is the one the store
    // enforces, so the two cannot drift apart.
    assert_eq!(louver_live_source::media::MAX_UPLOAD_BYTES, 512 * 1024 * 1024);
}

#[test]
fn an_unmatched_path_is_also_behind_the_gate() {
    // `Router::layer` wraps the fallback as well as the routes — asserted
    // rather than assumed, because if it did not, every unknown path would be
    // an ungated endpoint and the gate would have a hole the size of a typo.
    let r = rig(1);
    let (s, _) = req_full("GET", &format!("{}/nope", r.base), None, None, None, None, None);
    assert_eq!(s, 403, "an unmatched path answered without the gate");
    let (s, _) = req("GET", &format!("{}/nope", r.base), None, None, None);
    assert_eq!(s, 404, "and with the gate it is an honest 404");
}

#[test]
fn the_upload_route_needs_the_gate_and_a_token_like_everything_else() {
    let r = rig(1);
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let t = r.token_for("alice");
    for (gate, token, want) in [
        (None, Some(t.as_str()), 403u16),
        (Some(GATE), None, 401),
        (None, None, 403),
    ] {
        let mut b = agent.put(&r.url("/media/x.mp4"));
        if let Some(g) = gate {
            b = b.header("X-Louver-Gate", g);
        }
        if let Some(tok) = token {
            b = b.header("Authorization", &format!("Bearer {tok}"));
        }
        let mut resp = b.send(&b"x"[..]).unwrap();
        assert_eq!(resp.status().as_u16(), want, "gate={} token={}", gate.is_some(), token.is_some());
        let _ = resp.body_mut().read_to_string();
    }
}

/* ------------------------------- logout, expiry and suspension */

#[test]
fn a_dead_session_stops_the_next_handshake_but_not_the_token_already_minted() {
    // The honest shape of the limitation, pinned so nobody can later claim
    // more than this.
    //
    // Production destroys a cookie's session row on logout
    // (`delete_auth_session`), on expiry (`expires_at` in `user_for_token`)
    // and on suspension (`set_disabled` runs
    // `DELETE FROM auth_sessions WHERE user_id=?1`). All three therefore make
    // `/api/me` answer 401, which is what `Revocable` below imitates.
    //
    // What they do *not* do is revoke a bearer token this worker already
    // minted: it is self-contained until it expires. So the window is the
    // token's remaining lifetime — five minutes at most — and that is exactly
    // what this test asserts. It must not be read as "suspension is handled":
    // a job already running keeps running, which is a separate, unclosed gap.
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Revocable(AtomicBool);
    impl IdentitySource for Revocable {
        fn whoami(&self, cookie_header: &str) -> Result<Identity> {
            if self.0.load(Ordering::SeqCst) {
                // What production answers once the session row is gone.
                return Err(louver_live_source::LiveSourceError::unauthorized("로그인이 필요합니다."));
            }
            assert!(cookie_header.contains("louver_session="));
            Ok(Identity { user_id: "user-alice".into(), plan: "business".into() })
        }
    }

    let r = rig(1);
    let revoked = Arc::new(Revocable(AtomicBool::new(false)));
    let api = Api {
        jobs: Arc::clone(&r.jobs),
        signer: Arc::new(Signer::new(SECRET).unwrap()),
        identity: revoked.clone(),
        gate: Arc::new(Gate::new(GATE).unwrap()),
        origins: Arc::new(AllowedOrigins::parse(ORIGIN).unwrap()),
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
    let url = |p: &str| format!("{base}/api/live-source{p}");
    // Probed with a client that reports a refused connection rather than
    // panicking on it: `req` unwraps, and the first few attempts land before
    // the listener exists.
    assert!(
        wait_until(Duration::from_secs(10), || ureq::get(&url("/health"))
            .header("X-Louver-Gate", GATE)
            .call()
            .is_ok()),
        "api did not come up"
    );

    // A token while the session is alive.
    let (s, body) = req("POST", &url("/session"), None, Some("louver_session=alice"), None);
    assert_eq!(s, 200, "{body}");
    let t = body["token"].as_str().unwrap().to_string();
    assert_eq!(req("GET", &url("/jobs"), Some(&t), None, None).0, 200);

    // The user logs out, or is suspended. Production's session row is gone.
    revoked.0.store(true, Ordering::SeqCst);

    // The next handshake fails, so the page tells them to sign in again…
    let (s, got) = req("POST", &url("/session"), None, Some("louver_session=alice"), None);
    assert_eq!(s, 401, "{got}");

    // …but the token already minted keeps working until it expires. This is
    // the limitation, measured rather than assumed, and bounded by the
    // five-minute lifetime.
    assert_eq!(
        req("GET", &url("/jobs"), Some(&t), None, None).0,
        200,
        "a minted token is self-contained; the bound is its lifetime"
    );
    assert_eq!(louver_live_source::token::TOKEN_TTL_SECS, 300, "which is what bounds the window");
}
