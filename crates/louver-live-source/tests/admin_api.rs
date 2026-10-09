//! The operator's revocation API, over a real socket.
//!
//! What has to be true, and is asserted here rather than designed and hoped:
//!
//!  * only the operator's own secret opens it — not the gate secret, not a
//!    user's bearer token, not a near miss;
//!  * it stops **only** the named account's jobs;
//!  * calling it twice is safe;
//!  * every call is in the audit log, and the log carries no secret;
//!  * a revoked account cannot start a new job even while its token is still
//!    cryptographically valid;
//!  * a restart does not put a revoked account back on air;
//!  * it is not reachable through the paths Caddy forwards.
//!
//! Test accounts and test jobs only. No production anything: the destinations
//! point at `127.0.0.1:1`, which refuses instantly.

mod common;

use common::*;
use louver_core::streaming::ffmpeg::FfmpegTools;
use louver_core::OutputProfile;
use louver_live_source::state::Desired;
use louver_live_source::{
    admin::{AdminSecret, AuditLog, RevokedUsers},
    api::Api,
    auth::{Identity, IdentitySource},
    destinations::Destinations,
    gate::Gate,
    jobs::{NewJob, Registry, Settings},
    limits::Limits,
    media::MediaRoot,
    origin::AllowedOrigins,
    resolver::{LiveSourceResolver, ResolvedSource},
    token::Signer,
    Result,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const SECRET: &str = "a-test-secret-long-enough-to-pass-32";
const GATE: &str = "a-test-gate-secret-also-32-bytes-long";
const ADMIN: &str = "a-test-admin-secret-32-bytes-long!!!";
const ORIGIN: &str = "https://beta.test";
const STREAM_KEY: &str = "abcd-efgh-ijkl-mnop-qrst";

struct Cookies;
impl IdentitySource for Cookies {
    fn whoami(&self, cookie_header: &str) -> Result<Identity> {
        match cookie_header.strip_prefix("louver_session=").unwrap_or_default() {
            "alice" => Ok(Identity { user_id: "user-alice".into(), plan: "business".into() }),
            "bob" => Ok(Identity { user_id: "user-bob".into(), plan: "basic".into() }),
            _ => Err(louver_live_source::LiveSourceError::unauthorized("로그인이 필요합니다.")),
        }
    }
}

struct Never;
impl LiveSourceResolver for Never {
    fn resolve(&self, _: &str) -> Result<ResolvedSource> {
        Err(louver_live_source::LiveSourceError::unavailable("해석하지 않습니다."))
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    base: String,
    state_dir: PathBuf,
    media_dir: PathBuf,
    jobs: Arc<Registry>,
    revoked: Arc<RevokedUsers>,
    audit: Arc<AuditLog>,
}

fn destinations() -> Destinations {
    let url = format!("rtmp://127.0.0.1:1/live/{STREAM_KEY}");
    let mut by_user: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for u in ["user-alice", "user-bob"] {
        by_user.insert(u.into(), BTreeMap::from([("test-sink".to_string(), url.clone())]));
    }
    Destinations::from_map(by_user).unwrap()
}

fn plant_media(root: &MediaRoot) {
    for user in ["user-alice", "user-bob"] {
        let d = root.dir_for(user).unwrap();
        std::fs::write(d.join("song-a.mp4"), b"x").unwrap();
    }
}

fn settings_over(
    state_dir: &std::path::Path,
    media_dir: &std::path::Path,
    revoked: &Arc<RevokedUsers>,
) -> Settings {
    let media = MediaRoot::new(media_dir, FfmpegTools::new(ffmpeg(), ffprobe())).unwrap();
    plant_media(&media);
    Settings {
        state_dir: state_dir.to_path_buf(),
        media,
        destinations: destinations(),
        tools: FfmpegTools::new(ffmpeg(), ffprobe()),
        profile: OutputProfile::P1080p30,
        limits: Limits { max_concurrent: 4, max_per_user: 4 },
        max_restarts: 50,
        stall_after: Duration::from_secs(60),
        grace: Duration::from_secs(60),
        revoked: Arc::clone(revoked),
    }
}

fn serve(api: Api) -> String {
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
    assert!(
        wait_until(Duration::from_secs(10), || ureq::get(&format!("{base}/api/live-source/health"))
            .header("X-Louver-Gate", GATE)
            .call()
            .is_ok()),
        "api did not come up"
    );
    base
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let media_dir = dir.path().join("media");
    std::fs::create_dir_all(&media_dir).unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let revoked = Arc::new(RevokedUsers::load(&state_dir));
    let audit = Arc::new(AuditLog::new(&state_dir));
    let jobs = Arc::new(Registry::new(settings_over(&state_dir, &media_dir, &revoked), Arc::new(Never)));
    let base = serve(Api {
        jobs: Arc::clone(&jobs),
        signer: Arc::new(Signer::new(SECRET).unwrap()),
        identity: Arc::new(Cookies),
        gate: Arc::new(Gate::new(GATE).unwrap()),
        origins: Arc::new(AllowedOrigins::parse(ORIGIN).unwrap()),
        admin: Arc::new(AdminSecret::new(ADMIN).unwrap()),
        revoked: Arc::clone(&revoked),
        audit: Arc::clone(&audit),
    });
    Rig { _dir: dir, base, state_dir, media_dir, jobs, revoked, audit }
}

impl Rig {
    fn token_for(&self, who: &str) -> String {
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        let mut r = agent
            .post(format!("{}/api/live-source/session", self.base))
            .header("X-Louver-Gate", GATE)
            .header("Origin", ORIGIN)
            .header("Cookie", &format!("louver_session={who}"))
            .send_empty()
            .unwrap();
        let text = r.body_mut().read_to_string().unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        v["token"].as_str().expect("a token").to_string()
    }

    fn new_job(&self, id: &str) -> serde_json::Value {
        serde_json::json!({
            "broadcast_id": id,
            "source_url": "https://93.184.216.34/live/stream.m3u8",
            "playlist": ["song-a.mp4"],
            "destination": "test-sink"
        })
    }

    fn create(&self, token: &str, id: &str) -> u16 {
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        let mut r = agent
            .post(format!("{}/api/live-source/jobs", self.base))
            .header("X-Louver-Gate", GATE)
            .header("Authorization", &format!("Bearer {token}"))
            .send_json(self.new_job(id))
            .unwrap();
        let s = r.status().as_u16();
        let _ = r.body_mut().read_to_string();
        s
    }

    fn jobs_of(&self, token: &str) -> serde_json::Value {
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        let mut r = agent
            .get(format!("{}/api/live-source/jobs", self.base))
            .header("X-Louver-Gate", GATE)
            .header("Authorization", &format!("Bearer {token}"))
            .call()
            .unwrap();
        serde_json::from_str(&r.body_mut().read_to_string().unwrap()).unwrap()
    }

    /// An operator call, with whatever credential the test wants to try.
    fn admin(
        &self,
        path: &str,
        secret: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> (u16, serde_json::Value) {
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        let url = format!("{}{path}", self.base);
        let mut resp = match body {
            Some(v) => {
                let mut b = agent.post(&url);
                if let Some(s) = secret {
                    b = b.header("X-Louver-Admin", s);
                }
                b.send_json(&v).unwrap()
            }
            None => {
                let mut b = agent.get(&url);
                if let Some(s) = secret {
                    b = b.header("X-Louver-Admin", s);
                }
                b.call().unwrap()
            }
        };
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        (status, serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text)))
    }
}

/* ------------------------------------------------- authorization */

#[test]
fn only_the_operators_own_secret_opens_the_admin_routes() {
    let r = rig();
    let user_token = r.token_for("alice");
    let body = Some(serde_json::json!({"user_id": "user-alice"}));

    // Everything that is not the admin secret, including the other two
    // secrets this service holds.
    for (what, cred) in [
        ("missing", None),
        ("empty", Some("")),
        ("the gate secret", Some(GATE)),
        ("the signing secret", Some(SECRET)),
        ("a user's bearer token", Some(user_token.as_str())),
        ("a near miss (truncated)", Some(&ADMIN[..ADMIN.len() - 1])),
        ("a near miss (extended)", Some("a-test-admin-secret-32-bytes-long!!!x")),
        ("wrong case", Some("A-TEST-ADMIN-SECRET-32-BYTES-LONG!!!")),
    ] {
        for path in ["/admin/revoke", "/admin/restore"] {
            let (s, got) = r.admin(path, cred, body.clone());
            assert_eq!(s, 403, "{what} opened {path} → {got}");
            assert_eq!(got["error"], "forbidden");
            // The refusal says nothing about which part was wrong.
            assert_eq!(got["message"], "관리자 인증이 필요합니다.");
        }
        let (s, _) = r.admin("/admin/revoked", cred, None);
        assert_eq!(s, 403, "{what} opened /admin/revoked");
    }

    // And the operator's own secret works, which is what makes the refusals
    // meaningful rather than a route that is simply broken.
    let (s, got) = r.admin("/admin/revoked", Some(ADMIN), None);
    assert_eq!(s, 200, "{got}");
    assert_eq!(got["users"], serde_json::json!([]));
}

#[test]
fn the_admin_routes_do_not_need_and_do_not_take_the_gate() {
    // The gate asks "did this come through Caddy?" and for an operator action
    // the honest answer is no. So the admin secret alone is enough, and the
    // gate alone is not.
    let r = rig();
    let body = Some(serde_json::json!({"user_id": "user-alice"}));

    // Admin secret, no gate header: accepted.
    let (s, got) = r.admin("/admin/revoked", Some(ADMIN), None);
    assert_eq!(s, 200, "{got}");

    // Gate header present as well: still accepted, not confused by it.
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let mut resp = agent
        .post(format!("{}/admin/revoke", r.base))
        .header("X-Louver-Admin", ADMIN)
        .header("X-Louver-Gate", GATE)
        .send_json(body.as_ref().unwrap())
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let _ = resp.body_mut().read_to_string();
}

#[test]
fn the_admin_routes_are_not_under_a_path_caddy_forwards() {
    // Caddy's matchers are `/api/live-source/*` and `/beta/*`. If an operator
    // route ever sat under one of those, it would be internet-reachable with
    // only the admin secret. This pins the paths.
    let r = rig();
    for p in ["/admin/revoke", "/admin/restore", "/admin/revoked"] {
        assert!(!p.starts_with("/api/live-source"), "{p} would be proxied by Caddy");
        assert!(!p.starts_with("/beta"), "{p} would be proxied by Caddy");
    }
    // And the forwarded prefixes have no admin route hiding under them.
    for p in ["/api/live-source/admin/revoke", "/api/live-source/revoke", "/beta/admin/revoke"] {
        let (s, _) = r.admin(p, Some(ADMIN), Some(serde_json::json!({"user_id": "user-alice"})));
        assert!(s == 403 || s == 404 || s == 405, "{p} answered {s}; it must not be an admin route");
    }
}

#[test]
fn a_bad_user_id_is_refused_before_anything_is_recorded() {
    let r = rig();
    for bad in ["", "   ", "../../etc", "a/b", "a b", "a.b", &"x".repeat(65)] {
        let (s, got) = r.admin("/admin/revoke", Some(ADMIN), Some(serde_json::json!({"user_id": bad})));
        assert_eq!(s, 400, "{bad:?} → {got}");
    }
    assert!(r.revoked.list().is_empty(), "a refused id was recorded anyway");
    assert!(r.audit.entries().is_empty(), "a refused id reached the audit log");
}

/* ------------------------------------------ stopping one account */

#[test]
fn revoking_stops_that_accounts_jobs_and_leaves_every_other_account_alone() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let r = rig();
    let (alice, bob) = (r.token_for("alice"), r.token_for("bob"));
    assert_eq!(r.create(&alice, "alice-1"), 201);
    assert_eq!(r.create(&alice, "alice-2"), 201);
    assert_eq!(r.create(&bob, "bob-1"), 201);
    assert_eq!(r.jobs.running_count(), 3);

    let (s, got) = r.admin("/admin/revoke", Some(ADMIN), Some(serde_json::json!({"user_id": "user-alice"})));
    assert_eq!(s, 200, "{got}");
    assert_eq!(got["changed"], true);
    assert_eq!(got["stopped"], 2);
    assert_eq!(got["jobs"], serde_json::json!(["alice-1", "alice-2"]));

    // Alice's are stopped…
    for id in ["alice-1", "alice-2"] {
        assert_eq!(r.jobs.get("user-alice", id).unwrap().desired, Desired::Stopped, "{id}");
    }
    // …and Bob's is untouched, still running, still his.
    let his = r.jobs.get("user-bob", "bob-1").unwrap();
    assert_eq!(his.desired, Desired::Running, "another account's broadcast was stopped");
    assert_eq!(r.jobs.running_count(), 1, "only Bob's is left");
    assert_eq!(r.jobs.running_count_for("user-bob"), 1);
    assert_eq!(r.jobs.running_count_for("user-alice"), 0);

    // Bob can still read his own, and is not revoked.
    assert_eq!(r.jobs_of(&bob)["jobs"].as_array().unwrap().len(), 1);
    assert!(!r.revoked.is_revoked("user-bob"));
    r.jobs.shutdown();
}

#[test]
fn revoking_twice_is_safe_and_both_calls_are_recorded() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let r = rig();
    let alice = r.token_for("alice");
    assert_eq!(r.create(&alice, "alice-1"), 201);

    let (s1, a) = r.admin("/admin/revoke", Some(ADMIN), Some(serde_json::json!({"user_id": "user-alice"})));
    let (s2, b) = r.admin("/admin/revoke", Some(ADMIN), Some(serde_json::json!({"user_id": "user-alice"})));
    assert_eq!(s1, 200, "{a}");
    assert_eq!(s2, 200, "a repeat must not be an error: {b}");

    assert_eq!(a["changed"], true);
    assert_eq!(a["stopped"], 1);
    assert_eq!(b["changed"], false, "the second call changed no decision");
    assert_eq!(b["stopped"], 0, "and had nothing left to stop");

    // One account in the list, not two.
    assert_eq!(r.revoked.list(), vec!["user-alice".to_string()]);

    // Both calls are in the audit log, and the repeat is marked as one.
    let log = r.audit.entries();
    assert_eq!(log.len(), 2, "{log:?}");
    assert!(log[0].changed && log[0].stopped == 1);
    assert!(!log[1].changed && log[1].stopped == 0);
    r.jobs.shutdown();
}

#[test]
fn a_revoked_account_cannot_start_a_new_job_even_with_a_still_valid_token() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    // The window the revocation closes: production's suspension kills the
    // cookie, but a bearer token already minted stays valid for up to five
    // minutes. Without the persisted decision the account could start a
    // broadcast in that window with a credential that is perfectly good.
    let r = rig();
    let alice = r.token_for("alice");
    let bob = r.token_for("bob");

    let (s, _) = r.admin("/admin/revoke", Some(ADMIN), Some(serde_json::json!({"user_id": "user-alice"})));
    assert_eq!(s, 200);

    // Same token as before the revocation — it still verifies.
    assert_eq!(r.create(&alice, "after-1"), 403, "a revoked account started a job");
    assert!(louver_live_source::state::scan(&r.state_dir).iter().all(|s| s.worker_id != "after-1"));

    // Bob is unaffected.
    assert_eq!(r.create(&bob, "bob-2"), 201);

    // And restoring lets Alice start again.
    let (s, got) = r.admin("/admin/restore", Some(ADMIN), Some(serde_json::json!({"user_id": "user-alice"})));
    assert_eq!(s, 200, "{got}");
    assert_eq!(got["changed"], true);
    assert_eq!(r.create(&alice, "after-2"), 201, "a restored account should be able to broadcast");
    r.jobs.shutdown();
}

#[test]
fn the_audit_log_names_the_jobs_and_carries_no_secret() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let r = rig();
    let alice = r.token_for("alice");
    assert_eq!(r.create(&alice, "alice-1"), 201);
    r.admin("/admin/revoke", Some(ADMIN), Some(serde_json::json!({"user_id": "user-alice"})));
    r.admin("/admin/restore", Some(ADMIN), Some(serde_json::json!({"user_id": "user-alice"})));

    let log = r.audit.entries();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].action, "revoke");
    assert_eq!(log[0].user_id, "user-alice");
    assert_eq!(log[0].jobs, vec!["alice-1".to_string()]);
    assert_eq!(log[1].action, "restore");
    assert!(!log[0].at.is_empty() && log[0].at.contains('T'), "a timestamp: {}", log[0].at);

    let raw = std::fs::read_to_string(r.audit.path()).unwrap();
    for forbidden in [ADMIN, GATE, SECRET, STREAM_KEY, "rtmp", "Bearer", "louver_session"] {
        assert!(!raw.contains(forbidden), "{forbidden} in the audit log");
    }
    r.jobs.shutdown();
}

/* -------------------------------------------- restart behaviour */

#[test]
fn a_restart_does_not_put_a_revoked_accounts_broadcast_back_on_air() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let r = rig();
    let (alice, bob) = (r.token_for("alice"), r.token_for("bob"));
    assert_eq!(r.create(&alice, "alice-1"), 201);
    assert_eq!(r.create(&bob, "bob-1"), 201);

    // Revoke Alice, then take the process down the way a kill does: stop the
    // threads without touching anybody's `desired`.
    r.admin("/admin/revoke", Some(ADMIN), Some(serde_json::json!({"user_id": "user-alice"})));
    r.jobs.shutdown();

    // A second process over the same state directory, loading the revocation
    // list from disk exactly as the binary does.
    let revoked2 = Arc::new(RevokedUsers::load(&r.state_dir));
    assert!(revoked2.is_revoked("user-alice"), "the decision must survive the process");
    let jobs2 =
        Arc::new(Registry::new(settings_over(&r.state_dir, &r.media_dir, &revoked2), Arc::new(Never)));

    let restored = jobs2.recover();
    assert_eq!(restored, 1, "exactly Bob's should come back");
    assert_eq!(jobs2.running_count_for("user-bob"), 1, "Bob's broadcast must survive a restart");
    assert_eq!(jobs2.running_count_for("user-alice"), 0, "a revoked account came back on air");

    // And it still cannot start anything in the new process.
    assert!(jobs2
        .create(
            "user-alice",
            "business",
            &NewJob {
                broadcast_id: "alice-2".into(),
                source_url: "https://93.184.216.34/live/stream.m3u8".into(),
                playlist: vec!["song-a.mp4".into()],
                destination: "test-sink".into(),
            }
        )
        .is_err());
    jobs2.shutdown();
}

#[test]
fn a_state_file_that_says_running_cannot_outvote_a_revocation() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    // The belt-and-braces case: a crash between "write desired = Stopped" and
    // the revocation being read, or a state file restored from a backup. The
    // revocation list is consulted by `recover` for exactly this.
    let r = rig();
    let alice = r.token_for("alice");
    assert_eq!(r.create(&alice, "alice-1"), 201);
    r.admin("/admin/revoke", Some(ADMIN), Some(serde_json::json!({"user_id": "user-alice"})));
    r.jobs.shutdown();

    // Forge the state back to Running, as a stale backup would.
    let store = louver_live_source::state::StateStore::new(&r.state_dir, "alice-1");
    let mut st = store.load().unwrap();
    st.desired = Desired::Running;
    store.save(&st).unwrap();
    assert_eq!(store.load().unwrap().desired, Desired::Running, "the forgery is in place");

    let revoked2 = Arc::new(RevokedUsers::load(&r.state_dir));
    let jobs2 =
        Arc::new(Registry::new(settings_over(&r.state_dir, &r.media_dir, &revoked2), Arc::new(Never)));
    assert_eq!(jobs2.recover(), 0, "a revoked account was recovered from a Running state file");
    assert_eq!(jobs2.running_count(), 0);
    jobs2.shutdown();
}
