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
        db.record_media_prepared(&m.id, &key, 60.0, 20).unwrap();
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
    user: String,
    id: String,
}

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
    db.record_media_prepared(&m.id, &key, 2.0, 20).unwrap();

    let dest = db.reserve_youtube_destination(&user, &account).unwrap();
    let b = db.create_broadcast(&user, "밤 라디오", &m.id, &dest.id, true).unwrap();
    yt.provision(&user, &b.id, &account).unwrap();

    YtHarness { dir, db, mgr, fleet, api, keys, user, id: b.id }
}

impl YtHarness {
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
