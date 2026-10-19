//! §19's isolation, crash-recovery, manual-stop and server-restart tests.
//!
//! These drive the real `BroadcastManager` and the real `BroadcastRuntime`
//! against a fake process, which is how the desktop's own runtime tests work.
//! Nothing here re-implements a restart policy: `StreamSupervisor` owns that and
//! these tests watch it behave.

use louver_cloud::manager::{BroadcastManager, LauncherFactory};
use louver_cloud::storage::{LocalStorage, Storage};
use louver_cloud::{CloudDb, RuntimeState};
use louver_core::error::Result as CoreResult;
use louver_core::runtime::StreamLauncher;
use louver_core::security::{MemorySecretStore, SecretStore};
use louver_core::streaming::ffmpeg::FfmpegTools;
use louver_core::streaming::supervisor::{ProcessHandle, StreamSupervisor};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

// --- a process that does what a test tells it -------------------------------

#[derive(Default, Debug)]
struct FakeState {
    exited: AtomicBool,
    /// Set by a test that wants every `terminate` to meet the others.
    ///
    /// A rendezvous rather than a sleep: it measures concurrency directly, so
    /// the assertion does not depend on how fast or how loaded the machine is.
    gate: Mutex<Option<Arc<Gate>>>,
}

/// A meeting point for N callers, with a deadline so a regression fails the
/// test instead of hanging it.
#[derive(Debug)]
struct Gate {
    want: usize,
    arrived: Mutex<usize>,
    bell: std::sync::Condvar,
    /// How many callers left the gate because everyone was there, rather than
    /// because they gave up waiting.
    ///
    /// This is the number that distinguishes concurrent from serial, and
    /// counting arrivals does not: a serial shutdown still sends every worker
    /// through here eventually, one timeout at a time. Only an overlapping
    /// shutdown lets anybody pass.
    passed: std::sync::atomic::AtomicUsize,
}

impl Gate {
    fn new(want: usize) -> Arc<Self> {
        Arc::new(Self {
            want,
            arrived: Mutex::new(0),
            bell: std::sync::Condvar::new(),
            passed: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Arrive, then wait for everyone else — or give up after a second.
    fn meet(&self) {
        let mut n = self.arrived.lock().unwrap();
        *n += 1;
        if *n >= self.want {
            self.passed.fetch_add(1, Ordering::SeqCst);
            self.bell.notify_all();
            return;
        }
        let deadline = std::time::Duration::from_secs(1);
        let (_n, timeout) = self.bell.wait_timeout_while(n, deadline, |n| *n < self.want).unwrap();
        if !timeout.timed_out() {
            self.passed.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn passed(&self) -> usize {
        self.passed.load(Ordering::SeqCst)
    }
}

struct FakeProcess {
    state: Arc<FakeState>,
    pid: u32,
}

impl ProcessHandle for FakeProcess {
    fn try_exited(&mut self) -> Option<bool> {
        // `false` means "exited, not by our request" — a crash.
        self.state.exited.load(Ordering::SeqCst).then_some(false)
    }
    fn terminate(&mut self) -> CoreResult<()> {
        // Taken out of the lock before waiting, so holding the gate does not
        // also hold this process's own mutex.
        let gate = self.state.gate.lock().unwrap().clone();
        if let Some(g) = gate {
            g.meet();
        }
        self.state.exited.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn pid(&self) -> Option<u32> {
        Some(self.pid)
    }
}

/// Per-broadcast launch bookkeeping, so a test can kill one and watch the rest.
#[derive(Default, Debug)]
struct Fleet {
    launches: Mutex<HashMap<String, u32>>,
    live: Mutex<HashMap<String, Arc<FakeState>>>,
    next_pid: AtomicU32,
    /// Handed to every process this fleet launches, so a test can set it once.
    gate: Mutex<Option<Arc<Gate>>>,
}

impl Fleet {
    fn launches(&self, id: &str) -> u32 {
        self.launches.lock().unwrap().get(id).copied().unwrap_or(0)
    }
    /// Make this broadcast's process look like it died on its own.
    fn crash(&self, id: &str) {
        if let Some(s) = self.live.lock().unwrap().get(id) {
            s.exited.store(true, Ordering::SeqCst);
        }
    }
    fn is_alive(&self, id: &str) -> bool {
        self.live.lock().unwrap().get(id).map(|s| !s.exited.load(Ordering::SeqCst)).unwrap_or(false)
    }
}

#[derive(Debug)]
struct FakeLaunchers {
    fleet: Arc<Fleet>,
}

impl LauncherFactory for FakeLaunchers {
    fn for_broadcast(&self, _db: &CloudDb, broadcast_id: &str) -> Arc<dyn StreamLauncher> {
        Arc::new(OneLauncher { fleet: Arc::clone(&self.fleet), id: broadcast_id.to_string() })
    }
}

struct OneLauncher {
    fleet: Arc<Fleet>,
    id: String,
}

impl StreamLauncher for OneLauncher {
    fn launch(&self, _s: &mut StreamSupervisor, _args: &[String]) -> CoreResult<Box<dyn ProcessHandle>> {
        *self.fleet.launches.lock().unwrap().entry(self.id.clone()).or_insert(0) += 1;
        let state = Arc::new(FakeState::default());
        *state.gate.lock().unwrap() = self.fleet.gate.lock().unwrap().clone();
        self.fleet.live.lock().unwrap().insert(self.id.clone(), Arc::clone(&state));
        let pid = self.fleet.next_pid.fetch_add(1, Ordering::SeqCst) + 1000;
        Ok(Box::new(FakeProcess { state, pid }))
    }
}

// --- harness ---------------------------------------------------------------

struct Harness {
    _dir: tempfile::TempDir,
    db: CloudDb,
    mgr: BroadcastManager,
    fleet: Arc<Fleet>,
    user: String,
    path: std::path::PathBuf,
}

fn harness(plan: &str, broadcasts: usize) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cloud.db");
    let db = CloudDb::open(&path).unwrap();
    let fleet = Arc::new(Fleet::default());
    let mgr = build_manager(&db, dir.path(), &fleet);

    let user = db.create_user("live@x.com", "hash", plan).unwrap().id;
    let store = LocalStorage::new(dir.path().join("media"));
    let dest = db.create_destination(&user, "채널", "rtmps://a/live2", "••••").unwrap();

    for i in 0..broadcasts {
        // A real file on disk, because the manager localises it and the engine
        // writes a manifest pointing at it.
        let src = dir.path().join(format!("in{i}.mp4"));
        std::fs::write(&src, b"prepared video bytes").unwrap();
        let key = store.put_file(&user, &format!("clip{i}.mp4"), &src).unwrap();

        let m = db.create_media(&user, &format!("clip{i}.mp4"), 20, &key).unwrap();
        db.record_media_prepared(&m.id, &key, 60.0, 0, 20).unwrap();
        db.create_broadcast(&user, &format!("LIVE {i}"), &m.id, &dest.id, true).unwrap();
    }
    Harness { _dir: dir, db, mgr, fleet, user, path }
}

fn build_manager(db: &CloudDb, root: &std::path::Path, fleet: &Arc<Fleet>) -> BroadcastManager {
    let keys: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
    keys.set("destination-placeholder", "aaaa-aaaa-aaaa-aaaa").ok();
    BroadcastManager::new(
        db.clone(),
        Arc::new(LocalStorage::new(root.join("media"))),
        root.join("work"),
        FfmpegTools::new("ffmpeg", "ffprobe"),
        "libx264".into(),
        keys,
        Arc::new(FakeLaunchers { fleet: Arc::clone(fleet) }),
    )
}

/// The key every broadcast's destination needs, under its own account name.
fn give_every_destination_a_key(h: &Harness) {
    let keys: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
    let _ = keys;
    for d in h.db.destinations_for(&h.user).unwrap() {
        // The manager reads through its own store; set it there.
        h.mgr
            .secret_store()
            .set(&louver_cloud::credentials::destination_account(&d.id), "aaaa-aaaa-aaaa-aaaa")
            .unwrap();
    }
}

fn ids(h: &Harness) -> Vec<String> {
    h.db.broadcasts_for(&h.user).unwrap().into_iter().map(|b| b.id).collect()
}

/// Wait for a condition the worker threads reach on their own.
fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..600 {
        if f() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("timed out waiting for: {what}");
}

// --- tests -----------------------------------------------------------------

/// §2: three broadcasts, three processes, three independent lifecycles.
#[test]
fn three_broadcasts_run_at_once_and_a_crash_in_one_leaves_the_others_alone() {
    let h = harness("business", 3);
    give_every_destination_a_key(&h);
    let b = ids(&h);

    for id in &b {
        h.mgr.start(&h.user, id).unwrap_or_else(|e| panic!("start {id} failed: {e}"));
    }
    eventually("all three launched", || b.iter().all(|id| h.fleet.launches(id) == 1));
    assert_eq!(h.mgr.running_ids().len(), 3);

    // Kill the middle one's process out from under it.
    h.fleet.crash(&b[1]);

    // The supervisor notices and relaunches it. The other two are not touched.
    eventually("the crashed one was relaunched", || h.fleet.launches(&b[1]) >= 2);
    assert_eq!(h.fleet.launches(&b[0]), 1, "broadcast 0 was restarted by another's crash");
    assert_eq!(h.fleet.launches(&b[2]), 1, "broadcast 2 was restarted by another's crash");
    assert!(h.fleet.is_alive(&b[0]) && h.fleet.is_alive(&b[2]));
    assert_eq!(h.mgr.running_ids().len(), 3, "a crash took a worker down with it");

    h.mgr.shutdown();
}

/// §5: a broadcast the user stopped is never brought back.
#[test]
fn a_manual_stop_is_final_and_no_watchdog_restarts_it() {
    let h = harness("business", 2);
    give_every_destination_a_key(&h);
    let b = ids(&h);

    h.mgr.start(&h.user, &b[0]).unwrap();
    h.mgr.start(&h.user, &b[1]).unwrap();
    eventually("both launched", || h.fleet.launches(&b[0]) == 1 && h.fleet.launches(&b[1]) == 1);

    h.mgr.stop(&h.user, &b[0]).unwrap();
    let after_stop = h.fleet.launches(&b[0]);

    // Give a watchdog every chance to misbehave.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert_eq!(h.fleet.launches(&b[0]), after_stop, "a stopped broadcast was restarted");
    assert!(!h.mgr.running_ids().contains(&b[0]), "its worker is still running");

    let row = h.db.broadcast_owned(&h.user, &b[0]).unwrap();
    assert_eq!(row.desired_state, louver_cloud::DesiredState::Stopped);
    assert_eq!(row.runtime_state, RuntimeState::Stopped);

    // The other one carried on, and the freed slot is usable.
    assert!(h.fleet.is_alive(&b[1]));
    assert_eq!(h.db.active_stream_count(&h.user).unwrap(), 1);

    h.mgr.shutdown();
}

/// §6: a server that restarts brings back what was meant to be running.
#[test]
fn a_restarted_server_recovers_running_broadcasts_and_not_stopped_ones() {
    let h = harness("business", 3);
    give_every_destination_a_key(&h);
    let b = ids(&h);

    h.mgr.start(&h.user, &b[0]).unwrap();
    h.mgr.start(&h.user, &b[1]).unwrap();
    eventually("two launched", || h.fleet.launches(&b[0]) == 1 && h.fleet.launches(&b[1]) == 1);
    h.mgr.stop(&h.user, &b[1]).unwrap();

    // The process goes away without anyone being told — a kill -9 of the server.
    h.mgr.shutdown();
    drop(h.mgr);

    // A new manager, a new fleet, the same database.
    let fleet2 = Arc::new(Fleet::default());
    let db2 = CloudDb::open(&h.path).unwrap();
    let mgr2 = build_manager(&db2, h._dir.path(), &fleet2);
    for d in db2.destinations_for(&h.user).unwrap() {
        mgr2.secret_store()
            .set(&louver_cloud::credentials::destination_account(&d.id), "aaaa-aaaa-aaaa-aaaa")
            .unwrap();
    }

    let started = mgr2.recover_all().unwrap();
    assert_eq!(started, 1, "recovery should start exactly the one still wanted");
    eventually("the recovered broadcast launched", || fleet2.launches(&b[0]) == 1);
    assert_eq!(fleet2.launches(&b[1]), 0, "a broadcast the user stopped was resurrected");
    assert_eq!(fleet2.launches(&b[2]), 0, "a broadcast never started was started");

    mgr2.shutdown();
}

/// §3 again, but through the manager rather than the database.
#[test]
fn the_manager_refuses_a_stream_the_plan_does_not_allow() {
    let h = harness("basic", 2);
    give_every_destination_a_key(&h);
    let b = ids(&h);

    h.mgr.start(&h.user, &b[0]).unwrap();
    eventually("first launched", || h.fleet.launches(&b[0]) == 1);

    let second = h.mgr.start(&h.user, &b[1]);
    match &second {
        Err(louver_cloud::CloudError::ConcurrencyReached { plan_label, used, allowed }) => {
            // The refusal a user reads names their plan and their number, not a
            // database key.
            assert_eq!((plan_label.as_str(), *used, *allowed), ("Basic", 1, 1));
            assert_eq!(
                second.as_ref().unwrap_err().to_string(),
                "Basic 요금제에서는 동시에 1개의 방송을 송출할 수 있습니다"
            );
        }
        other => panic!("a Basic account started a second stream: {other:?}"),
    }
    assert_eq!(h.fleet.launches(&b[1]), 0, "a refused broadcast spawned a process anyway");
    assert_eq!(h.mgr.running_ids().len(), 1);

    h.mgr.shutdown();
}

/// A start that cannot work must not leave a slot held by nothing.
#[test]
fn a_failed_start_gives_its_slot_back() {
    let h = harness("basic", 1);
    // Deliberately no key for the destination, so the engine refuses to start.
    let b = ids(&h);

    let r = h.mgr.start(&h.user, &b[0]);
    assert!(r.is_err(), "a broadcast with no stream key must not start");
    assert_eq!(
        h.db.active_stream_count(&h.user).unwrap(),
        0,
        "the slot is still held by a broadcast that never ran",
    );
    assert!(h.db.broadcast_owned(&h.user, &b[0]).unwrap().last_error.is_some());
}

// --- YouTube lifecycle through the manager ---------------------------------
//
// The rule these exist to hold: a YouTube broadcast that has been transitioned
// to `complete` can never go live again. So *which* stop completes it is not a
// detail — completing one on a restart, on a watchdog relaunch or on a server
// shutdown bricks that broadcast permanently, and the user is told to make a new
// one. `StopReason` is the answer, and this is where it is checked against the
// real manager rather than against a diagram.

/// A fake Google, small enough to live here: only the calls these paths make.
#[derive(Debug, Default)]
struct FakeGoogle {
    calls: Mutex<Vec<(String, String)>>,
    lifecycle: Mutex<String>,
    stream_status: Mutex<String>,
    bound: Mutex<bool>,
    /// Broadcast ids handed out by `liveBroadcasts.insert`, in order.
    issued: Mutex<u32>,
    id: Mutex<String>,
}

impl FakeGoogle {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            lifecycle: Mutex::new("ready".into()),
            stream_status: Mutex::new("inactive".into()),
            bound: Mutex::new(true),
            id: Mutex::new("bcast-1".into()),
            ..Default::default()
        })
    }
    fn urls(&self) -> Vec<String> {
        self.calls.lock().unwrap().iter().map(|(_, u)| u.clone()).collect()
    }
    fn completions(&self) -> usize {
        self.urls().iter().filter(|u| u.contains("broadcastStatus=complete")).count()
    }
    fn inserts(&self) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, u)| m == "POST" && u.contains("/liveBroadcasts?"))
            .count()
    }
    fn json(&self) -> serde_json::Value {
        let mut content = serde_json::json!({ "enableAutoStart": true, "enableAutoStop": true });
        if *self.bound.lock().unwrap() {
            content["boundStreamId"] = serde_json::json!("stream-1");
        }
        serde_json::json!({
            "id": *self.id.lock().unwrap(),
            "snippet": { "title": "t", "description": "d" },
            "status": { "lifeCycleStatus": *self.lifecycle.lock().unwrap(), "privacyStatus": "unlisted" },
            "contentDetails": content,
        })
    }
}

impl louver_core::youtube::HttpClient for FakeGoogle {
    fn request(
        &self,
        method: &str,
        url: &str,
        _bearer: &str,
        _body: Option<serde_json::Value>,
    ) -> CoreResult<(u16, String)> {
        self.calls.lock().unwrap().push((method.into(), url.into()));
        let answer = if url.contains("/channels?") {
            serde_json::json!({ "items": [{ "id": "UC-1", "snippet": { "title": "채널" } }] })
        } else if url.contains("/liveStreams?") && method == "POST" {
            serde_json::json!({
                "id": "stream-1",
                "snippet": { "title": "247streams" },
                "cdn": { "ingestionInfo": {
                    "ingestionAddress": "rtmp://a.rtmp.youtube.com/live2",
                    "rtmpsIngestionAddress": "rtmps://a.rtmps.youtube.com/live2",
                    "streamName": "aaaa-bbbb-cccc-dddd",
                } },
                "status": { "streamStatus": "inactive" },
            })
        } else if url.contains("/liveStreams?") {
            serde_json::json!({ "items": [{
                "id": "stream-1",
                "snippet": { "title": "247streams" },
                "cdn": { "ingestionInfo": { "streamName": "aaaa-bbbb-cccc-dddd" } },
                "status": { "streamStatus": *self.stream_status.lock().unwrap() },
            }] })
        } else if url.contains("/liveBroadcasts/bind") {
            *self.bound.lock().unwrap() = true;
            // Binding names the broadcast being bound; answer as that one.
            self.json()
        } else if url.contains("/liveBroadcasts/transition") {
            let to = url.split("broadcastStatus=").nth(1).unwrap_or("").split('&').next().unwrap_or("");
            *self.lifecycle.lock().unwrap() = to.to_string();
            self.json()
        } else if url.contains("/liveBroadcasts?") {
            match method {
                "POST" => {
                    // A new broadcast: a new id, and it is not complete.
                    let mut n = self.issued.lock().unwrap();
                    *n += 1;
                    *self.id.lock().unwrap() = format!("bcast-{}", *n + 1);
                    *self.lifecycle.lock().unwrap() = "ready".into();
                    self.json()
                }
                "PUT" => self.json(),
                _ => serde_json::json!({ "items": [self.json()] }),
            }
        } else {
            return Err(louver_core::error::LouverError::with_detail(
                louver_core::error::ErrorCode::YoutubeApiFailed,
                format!("이 fake 는 {method} {url} 을 모릅니다"),
            ));
        };
        Ok((200, answer.to_string()))
    }
}

#[derive(Debug, Default)]
struct FakeTokens;

impl louver_core::youtube::oauth::TokenEndpoint for FakeTokens {
    fn exchange_code(
        &self,
        _c: &louver_core::youtube::oauth::ClientCredentials,
        _code: &str,
        _redirect: &str,
        _verifier: &str,
    ) -> CoreResult<louver_core::youtube::oauth::TokenResponse> {
        Ok(serde_json::from_value(serde_json::json!({
            "access_token": "access-1", "refresh_token": "refresh-1", "expires_in": 3600
        }))
        .unwrap())
    }
    fn refresh(
        &self,
        _c: &louver_core::youtube::oauth::ClientCredentials,
        _r: &str,
    ) -> CoreResult<louver_core::youtube::oauth::TokenResponse> {
        Ok(serde_json::from_value(serde_json::json!({ "access_token": "access-2", "expires_in": 3600 }))
            .unwrap())
    }
}

struct YtHarness {
    dir: tempfile::TempDir,
    db: CloudDb,
    mgr: BroadcastManager,
    fleet: Arc<Fleet>,
    api: Arc<FakeGoogle>,
    keys: Arc<dyn SecretStore>,
    pay: louver_cloud::billing::Payapp,
    payapi: Arc<FakePayapp>,
    user: String,
    id: String,
    account: String,
}

/// A PayApp that agrees unless a test scripts a refusal.
#[derive(Debug, Default)]
struct FakePayapp {
    calls: Mutex<Vec<String>>,
    replies: Mutex<Vec<String>>,
}

impl FakePayapp {
    fn refuse_next(&self) {
        self.replies.lock().unwrap().push("state=0&errno=00009".into());
    }
    fn cancels(&self) -> usize {
        self.calls.lock().unwrap().iter().filter(|c| c.contains("rebillCancel")).count()
    }
}

impl louver_cloud::billing::FormPost for FakePayapp {
    fn post_form(&self, _url: &str, fields: &[(&str, &str)]) -> louver_cloud::Result<String> {
        let cmd = fields.iter().find(|(k, _)| *k == "cmd").map(|(_, v)| *v).unwrap_or("");
        self.calls.lock().unwrap().push(cmd.to_string());
        let scripted = {
            let mut r = self.replies.lock().unwrap();
            if r.is_empty() {
                None
            } else {
                Some(r.remove(0))
            }
        };
        Ok(scripted.unwrap_or_else(|| match cmd {
            "rebillRegist" => {
                "state=1&errno=00000&rebill_no=CAN1&payurl=https%3A%2F%2Fpayapp.kr%2Fp%2F1".into()
            }
            _ => "state=1&errno=00000".to_string(),
        }))
    }
}

const PAY_USERID: &str = "247streams";
const PAY_LINKKEY: &str = "cancel-test-link-key";
const PAY_LINKVAL: &str = "cancel-test-link-val";

/// A connected account with a provisioned, YouTube-backed broadcast: exactly
/// what a user who pressed "YouTube로 방송 만들기" ends up with.
fn youtube_harness() -> YtHarness {
    let dir = tempfile::tempdir().unwrap();
    let db = CloudDb::open(&dir.path().join("cloud.db")).unwrap();
    let fleet = Arc::new(Fleet::default());
    let keys: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
    let api = FakeGoogle::new();
    let yt = louver_cloud::youtube::Youtube::new(
        db.clone(),
        Arc::clone(&keys),
        Arc::clone(&api) as Arc<dyn louver_core::youtube::HttpClient>,
        Arc::new(FakeTokens) as Arc<dyn louver_core::youtube::oauth::TokenEndpoint>,
        louver_cloud::youtube::Config {
            credentials: louver_core::youtube::oauth::ClientCredentials {
                client_id: "test-client.apps.googleusercontent.com".into(),
                client_secret: "GOCSPX-test-only".into(),
            },
            redirect_uri: "https://247streams.kr/api/youtube/oauth/callback".into(),
            api_base: Some("https://fake.googleapis.test/youtube/v3".into()),
        },
    );
    let mgr = BroadcastManager::new(
        db.clone(),
        Arc::new(LocalStorage::new(dir.path().join("media"))),
        dir.path().join("work"),
        FfmpegTools::new("ffmpeg", "ffprobe"),
        "libx264".into(),
        Arc::clone(&keys),
        Arc::new(FakeLaunchers { fleet: Arc::clone(&fleet) }),
    )
    .with_youtube(yt.clone());

    let user = db.create_user("dj@example.com", "hash", "business").unwrap().id;
    let state = {
        let url = yt.consent_url(&user).unwrap();
        url.split("state=").nth(1).unwrap().split('&').next().unwrap().to_string()
    };
    let account = yt.complete_consent(&state, "auth-code-1").unwrap().id;

    let store = LocalStorage::new(dir.path().join("media"));
    let src = dir.path().join("in.mp4");
    std::fs::write(&src, b"prepared video bytes").unwrap();
    let key = store.put_file(&user, "clip.mp4", &src).unwrap();
    let m = db.create_media(&user, "clip.mp4", 20, &key).unwrap();
    // Two seconds, so a single-pass playlist finishes inside a test rather than
    // in a minute. Nothing that loops reads this.
    db.record_media_prepared(&m.id, &key, 2.0, 0, 20).unwrap();

    let dest = db.reserve_youtube_destination(&user, &account).unwrap();
    let b = db.create_broadcast(&user, "밤 라디오", &m.id, &dest.id, true).unwrap();
    yt.provision(&user, &b.id, &account).unwrap();

    let payapi = Arc::new(FakePayapp::default());
    let pay = louver_cloud::billing::Payapp::new(
        db.clone(),
        Arc::clone(&payapi) as Arc<dyn louver_cloud::billing::FormPost>,
        louver_cloud::billing::Config {
            userid: PAY_USERID.into(),
            linkkey: PAY_LINKKEY.into(),
            linkval: PAY_LINKVAL.into(),
            api_url: "https://fake.payapp.test/oapi/apiLoad.html".into(),
            public_url: "https://247streams.kr".into(),
        },
    );

    YtHarness { dir, db, mgr, fleet, api, keys, pay, payapi, user, id: b.id, account }
}

impl YtHarness {
    /// Pay for a plan the way an account does: register the recurring payment,
    /// then a verified notification. Nothing else grants an entitlement.
    fn subscribe(&self, plan: &str, price: &str) {
        let order = self.pay.checkout(&self.user, plan, "01012345678").unwrap();
        let feedback: std::collections::BTreeMap<String, String> = [
            ("userid", PAY_USERID),
            ("linkkey", PAY_LINKKEY),
            ("linkval", PAY_LINKVAL),
            ("price", price),
            ("pay_state", "4"),
            ("pay_date", "2026-09-28 12:00:05"),
            ("pay_type", "card"),
            ("mul_no", "880001"),
            ("rebill_no", "CAN1"),
            ("var1", &order.billing_id),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        assert!(self.pay.handle_feedback(&feedback).is_accepted(), "the payment was not accepted");
        assert!(self.db.subscription(&self.user).unwrap().active);
    }

    /// A second broadcast on the same channel, so "all of them" can be tested.
    fn another(&self, name: &str) -> String {
        use louver_cloud::storage::Storage;
        let store = LocalStorage::new(self.dir.path().join("media"));
        let src = self.dir.path().join(format!("{name}.mp4"));
        std::fs::write(&src, b"prepared video bytes").unwrap();
        let key = store.put_file(&self.user, &format!("{name}.mp4"), &src).unwrap();
        let m = self.db.create_media(&self.user, &format!("{name}.mp4"), 20, &key).unwrap();
        self.db.record_media_prepared(&m.id, &key, 2.0, 0, 20).unwrap();
        let dest = self.db.reserve_youtube_destination(&self.user, &self.account).unwrap();
        let b = self.db.create_broadcast(&self.user, name, &m.id, &dest.id, true).unwrap();
        self.mgr.youtube().unwrap().provision(&self.user, &b.id, &self.account).unwrap();
        b.id
    }

    fn link(&self) -> louver_cloud::youtube::YoutubeLink {
        self.db.broadcast_owned(&self.user, &self.id).unwrap().youtube
    }
    fn destination(&self) -> louver_cloud::StreamDestination {
        let b = self.db.broadcast_owned(&self.user, &self.id).unwrap();
        self.db.destination(&b.destination_id).unwrap()
    }
    /// What YouTube looks like once the stream has been accepted and the
    /// broadcast is on air. Without this, "no completion" would be trivially
    /// true: YouTube only accepts `complete` from a broadcast that is live.
    fn go_live(&self) {
        *self.api.stream_status.lock().unwrap() = "active".into();
        *self.api.lifecycle.lock().unwrap() = "live".into();
    }

    fn stream_key(&self) -> Option<String> {
        self.keys.get(&louver_cloud::credentials::destination_account(&self.destination().id)).ok().flatten()
    }
}

#[test]
fn a_restart_never_ends_the_youtube_broadcast() {
    // The blocker this enum was introduced for. `restart` used to go through the
    // ordinary stop, which completes the YouTube broadcast — and a completed
    // broadcast cannot go live again, so the start on the other side of the
    // restart was refused and the user was told to make a new broadcast.
    let h = youtube_harness();
    let before = h.link().broadcast_id;

    h.mgr.start(&h.user, &h.id).unwrap();
    eventually("launched", || h.fleet.launches(&h.id) == 1);
    h.go_live();

    h.mgr.restart(&h.user, &h.id).unwrap();
    eventually("relaunched", || h.fleet.launches(&h.id) == 2);

    assert_eq!(h.api.completions(), 0, "a restart completed the YouTube broadcast: {:?}", h.api.urls());
    assert_eq!(h.link().broadcast_id, before, "a restart made a second YouTube broadcast");
    assert_eq!(
        h.db.broadcast_owned(&h.user, &h.id).unwrap().desired_state,
        louver_cloud::DesiredState::Running
    );
    // The slot was never given up in between, so nothing could take it.
    assert_eq!(h.db.active_stream_count(&h.user).unwrap(), 1);
    h.mgr.shutdown();
}

#[test]
fn a_watchdog_relaunch_after_a_crash_never_touches_youtube() {
    let h = youtube_harness();
    h.mgr.start(&h.user, &h.id).unwrap();
    eventually("launched", || h.fleet.launches(&h.id) == 1);

    h.go_live();
    h.fleet.crash(&h.id);
    eventually("relaunched by the supervisor", || h.fleet.launches(&h.id) >= 2);

    assert_eq!(h.api.completions(), 0, "the watchdog completed the YouTube broadcast");
    assert_eq!(h.api.inserts(), 1, "the watchdog made a new YouTube broadcast");
    h.mgr.shutdown();
}

#[test]
fn a_server_shutdown_leaves_youtube_alone_and_recovery_brings_the_broadcast_back() {
    let h = youtube_harness();
    h.mgr.start(&h.user, &h.id).unwrap();
    eventually("launched", || h.fleet.launches(&h.id) == 1);

    h.go_live();
    h.mgr.shutdown();
    assert_eq!(h.api.completions(), 0, "shutting the server down ended the user's broadcast on YouTube");
    assert_eq!(
        h.db.broadcast_owned(&h.user, &h.id).unwrap().desired_state,
        louver_cloud::DesiredState::Running,
        "a shutdown must leave the intent alone or recovery has nothing to read",
    );

    assert_eq!(h.mgr.recover_all().unwrap(), 1);
    eventually("recovered", || h.fleet.launches(&h.id) >= 2);
    h.mgr.shutdown();
}

#[test]
fn the_stop_button_ends_the_youtube_broadcast() {
    let h = youtube_harness();
    h.mgr.start(&h.user, &h.id).unwrap();
    eventually("launched", || h.fleet.launches(&h.id) == 1);

    h.go_live();
    h.mgr.stop(&h.user, &h.id).unwrap();
    assert_eq!(h.api.completions(), 1, "STOP left the broadcast live on YouTube");
    assert_eq!(h.link().status.as_deref(), Some("complete"));
    h.mgr.shutdown();
}

#[test]
fn the_next_occurrence_of_a_repeating_schedule_gets_a_new_youtube_broadcast() {
    // The other half of the blocker. A daily schedule ends its window, which
    // completes that day's YouTube broadcast — correctly. Tomorrow's occurrence
    // then has to be able to start, which it can only do on a new one.
    let h = youtube_harness();
    let first = h.link().broadcast_id.clone().unwrap();
    let stream = h.link().stream_id.clone().unwrap();
    let destination = h.destination().id;
    let key = h.stream_key();

    h.mgr.start(&h.user, &h.id).unwrap();
    eventually("launched", || h.fleet.launches(&h.id) == 1);

    h.go_live();
    // Yesterday's window ends.
    h.mgr.stop_with(&h.user, &h.id, louver_cloud::manager::StopReason::ScheduledWindowEnd).unwrap();
    assert_eq!(h.api.completions(), 1, "the window ended but YouTube was not told");
    assert_eq!(h.link().status.as_deref(), Some("complete"));

    // Today's occurrence.
    h.mgr.start(&h.user, &h.id).unwrap();
    eventually("started again", || h.fleet.launches(&h.id) == 2);

    let now = h.link();
    assert_ne!(now.broadcast_id.as_deref(), Some(first.as_str()), "a completed broadcast was reused");
    assert_eq!(now.status.as_deref(), Some("waiting_for_ingest"));
    // And the parts the user owns are untouched: the same ingestion stream, the
    // same destination row, the same sealed key. Nothing they connected broke.
    assert_eq!(now.stream_id.as_deref(), Some(stream.as_str()), "the ingestion stream was replaced");
    assert_eq!(h.destination().id, destination, "the destination row was replaced");
    assert_eq!(h.stream_key(), key, "the stream key changed underneath a running setup");
    h.mgr.shutdown();
}

#[test]
fn a_single_pass_playlist_ends_on_youtube_too() {
    // "반복 재생" off: the server ends the pass itself. That is a real ending, so
    // the three states have to agree — stopped here, stopped in the row, and
    // complete on YouTube. It used to leave a channel showing a live broadcast
    // with nothing arriving on it.
    let h = youtube_harness();
    h.db.update_broadcast_owned(
        &h.user,
        &h.id,
        &louver_cloud::BroadcastPatch { loop_forever: Some(false), ..Default::default() },
    )
    .unwrap();

    h.mgr.start(&h.user, &h.id).unwrap();
    eventually("launched", || h.fleet.launches(&h.id) == 1);
    h.go_live();
    eventually("the single pass ended", || {
        h.db.broadcast_owned(&h.user, &h.id).unwrap().desired_state == louver_cloud::DesiredState::Stopped
    });
    eventually("youtube was told", || h.api.completions() == 1);

    let row = h.db.broadcast_owned(&h.user, &h.id).unwrap();
    assert_eq!(row.runtime_state, RuntimeState::Stopped);
    assert_eq!(row.youtube.status.as_deref(), Some("complete"));
    assert_eq!(h.db.active_stream_count(&h.user).unwrap(), 0, "the slot was not given back");
    let _ = &h.dir;
    h.mgr.shutdown();
}

// --- cancelling a subscription takes the account off air -------------------
//
// The policy: confirming a cancellation revokes the paid features *and* ends
// whatever is on air. The order is what makes it safe — PayApp first, so a
// refusal leaves the account paying and broadcasting rather than paying for
// nothing.

#[test]
fn cancelling_stops_the_stream_that_was_on_air() {
    // A, B, C, H together: the worker is gone, the intent is stopped, YouTube
    // is complete, and the plan is back to nothing.
    let h = youtube_harness();
    h.subscribe("pro", "39900");
    h.mgr.start(&h.user, &h.id).unwrap();
    eventually("launched", || h.fleet.launches(&h.id) == 1);
    h.go_live();

    let record = h.pay.cancel_and_take_off_air(&h.user, &h.mgr).unwrap();
    assert_eq!(record.status, louver_cloud::BillingStatus::Cancelled);
    assert_eq!(h.payapi.cancels(), 1, "PayApp was not asked, or was asked twice");

    // A: no worker left, and the process was asked to go.
    assert!(!h.mgr.running_ids().contains(&h.id), "a worker outlived the cancellation");
    assert!(!h.fleet.is_alive(&h.id), "FFmpeg is still publishing");
    // B: the stored intent.
    let row = h.db.broadcast_owned(&h.user, &h.id).unwrap();
    assert_eq!(row.desired_state, louver_cloud::DesiredState::Stopped);
    assert_eq!(row.runtime_state, RuntimeState::Stopped);
    // C: YouTube's side is finished.
    assert_eq!(h.api.completions(), 1, "YouTube was left holding a live broadcast");
    assert_eq!(h.link().status.as_deref(), Some("complete"));
    // H: nothing left to run on.
    let sub = h.db.subscription(&h.user).unwrap();
    assert!(!sub.active);
    assert_eq!(h.db.limit(&h.user, louver_cloud::entitlement::MAX_CONCURRENT_STREAMS).unwrap(), 0);
    assert_eq!(h.db.active_stream_count(&h.user).unwrap(), 0, "a slot is still held");
    // And the log says why, where the user reads it.
    let events = h.db.events_for(&h.id, 50).unwrap();
    assert!(
        events.iter().any(|e| e.message.contains("구독이 해지되어 방송을 종료했습니다")),
        "{:?}",
        events.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    h.mgr.shutdown();
}

#[test]
fn nothing_revives_a_broadcast_the_cancellation_stopped() {
    // D. Two layers have to hold: the supervisor inside the worker (which is
    // gone, and its `desired_state` says stopped anyway) and the boot recovery
    // (which reads that intent *and* asks about the subscription).
    let h = youtube_harness();
    h.subscribe("pro", "39900");
    h.mgr.start(&h.user, &h.id).unwrap();
    eventually("launched", || h.fleet.launches(&h.id) == 1);
    h.go_live();
    h.pay.cancel_and_take_off_air(&h.user, &h.mgr).unwrap();
    let launches = h.fleet.launches(&h.id);

    // Give every watchdog its chance.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert_eq!(h.fleet.launches(&h.id), launches, "something relaunched a cancelled broadcast");
    // And a boot does not either.
    assert_eq!(h.mgr.recover_all().unwrap(), 0);
    assert_eq!(h.fleet.launches(&h.id), launches);
    h.mgr.shutdown();
}

#[test]
fn cancelling_stops_every_one_of_that_accounts_streams() {
    // E. Pro allows two; both have to come off.
    let h = youtube_harness();
    h.subscribe("pro", "39900");
    let second = h.another("두 번째");
    h.mgr.start(&h.user, &h.id).unwrap();
    h.mgr.start(&h.user, &second).unwrap();
    eventually("both launched", || h.fleet.launches(&h.id) == 1 && h.fleet.launches(&second) == 1);
    h.go_live();

    h.pay.cancel_and_take_off_air(&h.user, &h.mgr).unwrap();

    for id in [&h.id, &second] {
        assert!(!h.mgr.running_ids().contains(id), "{id} is still running");
        assert_eq!(
            h.db.broadcast_owned(&h.user, id).unwrap().desired_state,
            louver_cloud::DesiredState::Stopped,
            "{id} still wants to run",
        );
    }
    assert_eq!(h.db.active_stream_count(&h.user).unwrap(), 0);
    h.mgr.shutdown();
}

#[test]
fn one_accounts_cancellation_leaves_another_accounts_stream_alone() {
    // F. The query is scoped to one user id, so there is no argument that
    // reaches somebody else's broadcast — but that is exactly the kind of claim
    // that has to be checked rather than asserted in a comment.
    let h = youtube_harness();
    h.subscribe("pro", "39900");

    let other = h.db.create_user("other@example.com", "hash", "business").unwrap().id;
    let store = LocalStorage::new(h.dir.path().join("media"));
    let src = h.dir.path().join("other.mp4");
    std::fs::write(&src, b"prepared video bytes").unwrap();
    let key = {
        use louver_cloud::storage::Storage;
        store.put_file(&other, "other.mp4", &src).unwrap()
    };
    let m = h.db.create_media(&other, "other.mp4", 20, &key).unwrap();
    h.db.record_media_prepared(&m.id, &key, 2.0, 0, 20).unwrap();
    let dest = h.db.create_destination(&other, "남의 채널", "rtmps://a/live2", "••••").unwrap();
    h.keys.set(&louver_cloud::credentials::destination_account(&dest.id), "aaaa-bbbb-cccc-dddd").unwrap();
    let theirs = h.db.create_broadcast(&other, "남의 방송", &m.id, &dest.id, true).unwrap().id;

    h.mgr.start(&h.user, &h.id).unwrap();
    h.mgr.start(&other, &theirs).unwrap();
    eventually("both launched", || h.fleet.launches(&h.id) == 1 && h.fleet.launches(&theirs) == 1);
    h.go_live();

    h.pay.cancel_and_take_off_air(&h.user, &h.mgr).unwrap();

    assert!(h.mgr.running_ids().contains(&theirs), "another account's broadcast was stopped");
    assert!(h.fleet.is_alive(&theirs), "another account's FFmpeg was killed");
    assert_eq!(
        h.db.broadcast_owned(&other, &theirs).unwrap().desired_state,
        louver_cloud::DesiredState::Running,
    );
    assert!(h.db.subscription(&other).unwrap().active, "another account lost its plan");
    h.mgr.shutdown();
}

#[test]
fn a_refused_cancellation_leaves_the_stream_running() {
    // G, and the reason for the order. If the broadcast were stopped first and
    // PayApp then refused, the account would keep being charged with nothing on
    // air — the one outcome this whole design exists to prevent.
    let h = youtube_harness();
    h.subscribe("pro", "39900");
    h.mgr.start(&h.user, &h.id).unwrap();
    eventually("launched", || h.fleet.launches(&h.id) == 1);
    h.go_live();

    h.payapi.refuse_next();
    let refused = h.pay.cancel_and_take_off_air(&h.user, &h.mgr).expect_err("a refusal reported success");
    let said = refused.to_string();
    assert!(!said.contains(PAY_LINKKEY) && !said.contains(PAY_LINKVAL), "{said}");

    assert!(h.mgr.running_ids().contains(&h.id), "a refused cancellation took the broadcast off air");
    assert!(h.fleet.is_alive(&h.id));
    assert_eq!(
        h.db.broadcast_owned(&h.user, &h.id).unwrap().desired_state,
        louver_cloud::DesiredState::Running,
    );
    assert!(h.db.subscription(&h.user).unwrap().active, "the entitlement was taken on a refusal");
    assert_eq!(h.api.completions(), 0, "YouTube was ended on a refused cancellation");

    // Retryable, and the retry does everything.
    h.pay.cancel_and_take_off_air(&h.user, &h.mgr).unwrap();
    assert!(!h.mgr.running_ids().contains(&h.id));
    assert_eq!(h.api.completions(), 1);
    h.mgr.shutdown();
}

#[test]
fn a_hand_granted_plan_keeps_both_the_plan_and_the_broadcast() {
    // The operator's Business account, which has no billing record of its own.
    // A cancellation that revokes nothing must take nothing off air — otherwise
    // cancelling a leftover Basic registration would end an operator's stream.
    let h = youtube_harness();
    h.subscribe("basic", "19900");
    // An operator lifts this account to Business by hand, outside PayApp.
    h.db.activate_subscription(&h.user, "business").unwrap();
    h.mgr.start(&h.user, &h.id).unwrap();
    eventually("launched", || h.fleet.launches(&h.id) == 1);
    h.go_live();

    h.pay.cancel_and_take_off_air(&h.user, &h.mgr).unwrap();

    assert_eq!(h.payapi.cancels(), 1, "the recurring payment must still stop");
    let sub = h.db.subscription(&h.user).unwrap();
    assert!(sub.active, "a hand-granted plan is not PayApp's to take away");
    assert_eq!(sub.plan_id, "business");
    assert!(h.mgr.running_ids().contains(&h.id), "a broadcast was stopped for a plan nobody revoked");
    assert_eq!(h.api.completions(), 0);
    h.mgr.shutdown();
}

/// Shutting down tells every worker to stop before waiting on any of them.
///
/// It used to halt them one at a time, and each halt waits for FFmpeg to flush
/// its FLV trailer — three seconds, in the real `ProcessHandle`. Five
/// broadcasts therefore took up to fifteen seconds, and Docker's grace period
/// is ten: the container was SIGKILLed partway through, losing the trailers
/// that the careful shutdown existed to write.
///
/// Asserted as concurrency rather than as elapsed time: every `terminate`
/// meets the others at a gate, which only all of them can pass if they are
/// in flight together. A serial shutdown leaves the first one waiting there
/// alone, and the gate's deadline turns that into a failure rather than a hang.
#[test]
fn shutdown_stops_every_worker_at_once_rather_than_one_after_another() {
    let h = harness("business", 3);
    give_every_destination_a_key(&h);
    let b = ids(&h);

    let gate = Gate::new(3);
    *h.fleet.gate.lock().unwrap() = Some(Arc::clone(&gate));

    for id in &b {
        h.mgr.start(&h.user, id).unwrap_or_else(|e| panic!("start {id} failed: {e}"));
    }
    eventually("all three launched", || b.iter().all(|id| h.fleet.launches(id) == 1));

    h.mgr.shutdown();

    assert_eq!(
        gate.passed(),
        3,
        "only {} of 3 terminations overlapped — the shutdown is serial again",
        gate.passed()
    );
    assert!(h.mgr.running_ids().is_empty(), "shutdown left a worker behind");
    for id in &b {
        assert!(!h.fleet.is_alive(id), "broadcast {id} was not stopped");
    }
}

/// And a shutdown with nothing running is still a shutdown.
#[test]
fn shutting_down_an_idle_manager_does_nothing_and_does_not_hang() {
    let h = harness("business", 1);
    h.mgr.shutdown();
    assert!(h.mgr.running_ids().is_empty());
}

// --- the machine's encoder ceiling across a restart -----------------------
//
// `recover_all` does not go through `claim_stream_slot`, so without a check of
// its own a restart would revive every live-source broadcast at once and put
// the box over its budget. That is worse than a click doing it: nobody is
// watching a boot, and every one of them stutters.

/// A manager as a boot builds one, with the machine's encoder ceiling pinned.
///
/// Its own secret store, so the destination keys have to be put there too —
/// `build_manager` makes a fresh `MemorySecretStore` each time, which is what
/// a real restart does to an in-memory store and exactly why this helper
/// exists rather than reusing the harness's manager.
fn booted_manager(h: &Harness, encode_budget: i64) -> BroadcastManager {
    let mgr = build_manager(&h.db, h.path.parent().unwrap(), &h.fleet).with_encoding_budget(encode_budget);
    for d in h.db.destinations_for(&h.user).unwrap() {
        mgr.secret_store()
            .set(&louver_cloud::credentials::destination_account(&d.id), "aaaa-aaaa-aaaa-aaaa")
            .unwrap();
    }
    mgr
}

/// Point a broadcast at a live video source, which is what makes it an encode.
fn make_it_a_cctv_broadcast(h: &Harness, id: &str) {
    let patch = louver_cloud::BroadcastPatch {
        cctv_url: Some("https://93.184.216.34/live/stream.m3u8".to_string()),
        ..Default::default()
    };
    h.db.update_broadcast_owned(&h.user, id, &patch).unwrap();
}

#[test]
fn a_restart_recovers_encodes_up_to_the_budget_and_no_further() {
    let h = harness("business", 3);
    give_every_destination_a_key(&h);
    let b = ids(&h);
    for id in &b {
        make_it_a_cctv_broadcast(&h, id);
    }
    // All three were meant to be running when the server went down.
    for id in &b {
        h.db.claim_stream_slot_within(&h.user, id, 3).unwrap();
    }

    // A fresh manager, as a boot builds one, with room for one encode.
    let fresh = booted_manager(&h, 1);
    let started = fresh.recover_all().unwrap();

    assert_eq!(started, 1, "only one encode fits on this machine");
    let running: Vec<&String> = b.iter().filter(|id| h.fleet.launches(id) == 1).collect();
    assert_eq!(running.len(), 1, "exactly one was launched: {running:?}");

    // The other two are not discarded — they still want to run, so they come
    // back when there is room. Rewriting the intent would lose it silently.
    for id in &b {
        assert_eq!(
            h.db.desired_state(id).unwrap(),
            louver_cloud::DesiredState::Running,
            "{id} lost its intent"
        );
    }
    fresh.shutdown();
}

#[test]
fn a_restart_recovers_every_stream_copy_whatever_the_encode_budget_is() {
    // The ceiling is about encoding. Ordinary broadcasts cost almost nothing
    // and must all come back.
    let h = harness("business", 3);
    give_every_destination_a_key(&h);
    let b = ids(&h);
    for id in &b {
        h.db.claim_stream_slot_within(&h.user, id, 3).unwrap();
    }

    let fresh = booted_manager(&h, 1);
    assert_eq!(fresh.recover_all().unwrap(), 3, "a copy is not an encode");
    eventually("all three came back", || b.iter().all(|id| h.fleet.launches(id) == 1));
    fresh.shutdown();
}

#[test]
fn a_held_back_broadcast_is_not_left_looking_like_it_is_running() {
    // The dashboard badge is read off `runtime_state` and never looks at
    // `last_heartbeat`. Recovery used to leave whatever was there before the
    // restart — `RUNNING`, for a broadcast that was on air — so a broadcast
    // with no worker at all was shown as 송출 중. That is the lie this whole
    // change exists to remove, and recovery is the worst place for it: it
    // happens at boot, with nobody watching.
    let h = harness("business", 2);
    give_every_destination_a_key(&h);
    let b = ids(&h);
    for id in &b {
        make_it_a_cctv_broadcast(&h, id);
        h.db.claim_stream_slot_within(&h.user, id, 2).unwrap();
        // What a server that went down while broadcasting leaves behind.
        h.db.record_runtime_only(id, louver_cloud::RuntimeState::Running).unwrap();
    }

    let fresh = booted_manager(&h, 1);
    fresh.recover_all().unwrap();

    let held = b.iter().find(|id| h.fleet.launches(id) == 0).expect("one was held back");
    assert_eq!(
        h.db.broadcast(held).unwrap().runtime_state,
        louver_cloud::RuntimeState::Stopped,
        "a broadcast with no worker must not still read as running"
    );
    // And the intent is still there, which is what makes it resumable.
    assert_eq!(h.db.desired_state(held).unwrap(), louver_cloud::DesiredState::Running);
    fresh.shutdown();
}

#[test]
fn a_held_back_broadcast_starts_itself_once_there_is_room() {
    // The promise the hold-back's own log makes. Nothing used to keep it: the
    // scheduler reads only `sched_enabled = 1` broadcasts, the watchdog only
    // relaunches workers it already has, and `recover_all` runs once.
    let h = harness("business", 2);
    give_every_destination_a_key(&h);
    let b = ids(&h);
    for id in &b {
        make_it_a_cctv_broadcast(&h, id);
        h.db.claim_stream_slot_within(&h.user, id, 2).unwrap();
    }

    let fresh = booted_manager(&h, 1);
    fresh.recover_all().unwrap();
    let running = b.iter().find(|id| h.fleet.launches(id) == 1).expect("one came back").clone();
    let held = b.iter().find(|id| h.fleet.launches(id) == 0).expect("one was held back").clone();

    // While the one that fits is still on air, there is no room and the pass
    // must not start a second encode — that is the ceiling it exists under.
    assert_eq!(fresh.resume_held_back(), 0, "it started one over the budget");
    assert_eq!(h.fleet.launches(&held), 0);

    // The customer stops the other one. Now there is room.
    fresh.stop(&h.user, &running).unwrap();
    assert_eq!(fresh.resume_held_back(), 1, "nothing picked up the held-back broadcast");
    eventually("the held-back broadcast is on air", || h.fleet.launches(&held) == 1);
    let said =
        h.db.events_for(&held, 50).unwrap().into_iter().map(|e| e.message).collect::<Vec<_>>().join("\n");
    assert!(said.contains("자리가 생겨"), "it does not say why it started: {said}");
    fresh.shutdown();
}

#[test]
fn the_resume_pass_leaves_alone_what_nobody_asked_to_run() {
    // Three things it must not touch: a broadcast the user stopped, a stream
    // copy (never held back in the first place), and one that already has a
    // worker. Without the first, a stop button could be undone by a tick.
    let h = harness("business", 3);
    give_every_destination_a_key(&h);
    let b = ids(&h);
    make_it_a_cctv_broadcast(&h, &b[0]);
    make_it_a_cctv_broadcast(&h, &b[1]);
    // b[0]: parked by a hold-back. b[1]: the user stopped it. b[2]: a copy.
    h.db.claim_stream_slot_within(&h.user, &b[0], 3).unwrap();
    // Parked the way recovery parks one: the recorded fact, not a runtime
    // state that happens to look like it.
    h.db.record_runtime_only(&b[0], louver_cloud::RuntimeState::Stopped).unwrap();
    h.db.hold_back(&b[0]).unwrap();
    h.db.record_runtime_only(&b[1], louver_cloud::RuntimeState::Stopped).unwrap();
    h.db.claim_stream_slot_within(&h.user, &b[2], 3).unwrap();

    let fresh = booted_manager(&h, 2);
    assert_eq!(fresh.resume_held_back(), 1, "only the parked encode is resumed");
    eventually("the parked one started", || h.fleet.launches(&b[0]) == 1);
    assert_eq!(h.fleet.launches(&b[1]), 0, "a broadcast the user stopped was restarted by a tick");
    assert_eq!(h.fleet.launches(&b[2]), 0, "a copy was never held back, so it is not the pass's to start");
    fresh.shutdown();
}

#[test]
fn an_api_role_manager_resumes_nothing_itself() {
    // Same division as recovery and the scheduler. Two processes resuming the
    // same broadcast is the double start the split exists to prevent.
    let h = harness("business", 1);
    give_every_destination_a_key(&h);
    let b = ids(&h);
    make_it_a_cctv_broadcast(&h, &b[0]);
    h.db.claim_stream_slot_within(&h.user, &b[0], 1).unwrap();
    h.db.record_runtime_only(&b[0], louver_cloud::RuntimeState::Stopped).unwrap();
    h.db.hold_back(&b[0]).unwrap();

    let api = booted_manager(&h, 1).with_runner(louver_cloud::runner::Client::new(h.path.parent().unwrap()));
    assert_eq!(api.resume_held_back(), 0, "an api process started a worker");
    assert_eq!(h.fleet.launches(&b[0]), 0);
}

#[test]
fn the_held_back_broadcast_is_told_why_in_its_own_log() {
    // An operator reading one broadcast's events has to be able to see that it
    // was the machine and not the customer's plan.
    let h = harness("business", 2);
    give_every_destination_a_key(&h);
    let b = ids(&h);
    for id in &b {
        make_it_a_cctv_broadcast(&h, id);
        h.db.claim_stream_slot_within(&h.user, id, 2).unwrap();
    }

    let fresh = booted_manager(&h, 1);
    fresh.recover_all().unwrap();

    let held = b.iter().find(|id| h.fleet.launches(id) == 0).expect("one was held back");
    let said =
        h.db.events_for(held, 50).unwrap().into_iter().map(|e| e.message).collect::<Vec<_>>().join("\n");
    assert!(said.contains("인코딩"), "the reason is not in the log: {said}");
    assert!(said.contains("CCTV"), "nor which kind of broadcast: {said}");
    fresh.shutdown();
}
