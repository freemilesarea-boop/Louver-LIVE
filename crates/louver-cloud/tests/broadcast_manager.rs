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
    assert!(
        matches!(second, Err(louver_cloud::CloudError::LimitReached { .. })),
        "a Basic account started a second stream: {second:?}",
    );
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
