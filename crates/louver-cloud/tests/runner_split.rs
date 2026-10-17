//! The API/runner split, tested through two managers over a real socket.
//!
//! One manager owns the workers, as the runner process does. The other is in
//! `api` mode and reaches it over a Unix socket, as the API container does. The
//! socket, the protocol and the worker ownership are the real ones; only the
//! FFmpeg process is faked, by the same `Fleet` the other manager tests use.
//!
//! That is the right line to draw. What the split has to get right is *which
//! process holds the handle* and *what happens when the other one goes away*,
//! and neither of those involves a real encoder. The by-hand runs recorded in
//! `docs/FFMPEG_LIFECYCLE.md` cover the real FFmpeg case.

// `runner::Server` is a Unix socket, so there is nothing here to compile
// anywhere else. The split itself is a Linux deployment shape.
#![cfg(unix)]

use louver_cloud::manager::{BroadcastManager, LauncherFactory};
use louver_cloud::runner::{Client, Command, Reply, Role, Server};
use louver_cloud::storage::{LocalStorage, Storage};
use louver_cloud::CloudDb;
use louver_core::error::Result as CoreResult;
use louver_core::runtime::StreamLauncher;
use louver_core::security::SecretStore;
use louver_core::streaming::ffmpeg::FfmpegTools;
use louver_core::streaming::supervisor::{ProcessHandle, StreamSupervisor};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

// --- a process that does nothing, so a broadcast can be "running" ----------

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
        Arc::new(One { fleet: Arc::clone(&self.fleet), id: broadcast_id.to_string() })
    }
}

struct One {
    fleet: Arc<Fleet>,
    id: String,
}

impl StreamLauncher for One {
    fn launch(&self, _s: &mut StreamSupervisor, _args: &[String]) -> CoreResult<Box<dyn ProcessHandle>> {
        *self.fleet.launches.lock().unwrap().entry(self.id.clone()).or_insert(0) += 1;
        let state = Arc::new(FakeState::default());
        self.fleet.live.lock().unwrap().insert(self.id.clone(), Arc::clone(&state));
        let pid = self.fleet.next_pid.fetch_add(1, Ordering::SeqCst) + 2000;
        Ok(Box::new(FakeProcess { state, pid }))
    }
}

// --- the two sides ---------------------------------------------------------

struct Split {
    _dir: tempfile::TempDir,
    data: std::path::PathBuf,
    db: CloudDb,
    /// The manager the runner process would hold.
    runner: BroadcastManager,
    fleet: Arc<Fleet>,
    user: String,
    broadcasts: Vec<String>,
}

fn manager(db: &CloudDb, root: &std::path::Path, fleet: &Arc<Fleet>) -> BroadcastManager {
    let keys: Arc<dyn SecretStore> = Arc::new(louver_core::security::MemorySecretStore::new());
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

fn split(broadcasts: usize) -> Split {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().to_path_buf();
    let db = CloudDb::open(&data.join("cloud.db")).unwrap();
    let fleet = Arc::new(Fleet::default());
    let runner = manager(&db, &data, &fleet);

    let user = db.create_user("live@x.com", "hash", "business").unwrap().id;
    let store = LocalStorage::new(data.join("media"));
    let dest = db.create_destination(&user, "채널", "rtmps://a/live2", "••••").unwrap();
    runner
        .secret_store()
        .set(&louver_cloud::credentials::destination_account(&dest.id), "aaaa-aaaa-aaaa-aaaa")
        .unwrap();

    let mut ids = Vec::new();
    for i in 0..broadcasts {
        let src = data.join(format!("in{i}.mp4"));
        std::fs::write(&src, b"prepared video bytes").unwrap();
        let key = store.put_file(&user, &format!("clip{i}.mp4"), &src).unwrap();
        let m = db.create_media(&user, &format!("clip{i}.mp4"), 20, &key).unwrap();
        db.record_media_prepared(&m.id, &key, 60.0, 0, 20).unwrap();
        let b = db.create_broadcast(&user, &format!("LIVE {i}"), &m.id, &dest.id, true).unwrap();
        ids.push(b.id);
    }
    Split { _dir: dir, data, db, runner, fleet, user, broadcasts: ids }
}

impl Split {
    /// Serve the runner's socket on a background thread, as the runner process
    /// does, and hand back the client an API process would hold.
    fn serve(&self) -> Client {
        let server = Server::bind(&self.data).unwrap();
        let sock = server.socket().to_path_buf();
        let mgr = self.runner.clone();
        std::thread::spawn(move || {
            server.serve(move |cmd| match cmd {
                Command::Ping => Reply::Ok,
                Command::Running => Reply::Ids(mgr.running_ids()),
                Command::Start(id) => match mgr.start_worker(&id) {
                    Ok(()) => Reply::Ok,
                    Err(e) => Reply::Err(e.to_string()),
                },
                Command::Halt(id) => {
                    mgr.stop_worker(&id);
                    Reply::Ok
                }
            });
        });
        for _ in 0..400 {
            if sock.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        Client::new(&self.data)
    }

    /// A fresh API-side manager, as a new API container would build one.
    fn api(&self, client: &Client) -> BroadcastManager {
        manager(&self.db, &self.data, &self.fleet).with_runner(client.clone())
    }
}

fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..400 {
        if f() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!("timed out waiting for: {what}");
}

// --- the cases --------------------------------------------------------------

#[test]
fn a1_an_api_manager_spawns_nothing_itself() {
    let s = split(1);
    let client = s.serve();
    let api = s.api(&client);
    assert!(api.is_remote());

    api.start(&s.user, &s.broadcasts[0]).unwrap();
    eventually("the runner launched it", || s.fleet.launches(&s.broadcasts[0]) == 1);
    // The handle lives in the runner's manager, which is the whole point.
    assert_eq!(s.runner.running_ids(), vec![s.broadcasts[0].clone()]);
}

#[test]
fn a2_the_worker_survives_the_api_being_replaced() {
    // The deployment this exists for: the API process goes away and a new one
    // takes its place, and the broadcast does not notice.
    let s = split(1);
    let client = s.serve();
    let id = s.broadcasts[0].clone();

    let api = s.api(&client);
    api.start(&s.user, &id).unwrap();
    eventually("launched", || s.fleet.launches(&id) == 1);

    // The API container is stopped. `shutdown` is what `main` calls on the way
    // out, and for an API process it must do nothing at all.
    api.shutdown();
    drop(api);
    assert!(s.fleet.is_alive(&id), "stopping the API stopped the broadcast");
    assert_eq!(s.runner.running_ids(), vec![id.clone()]);

    // A new API container comes up, recovers nothing, and sees the truth.
    let fresh = s.api(&client);
    assert_eq!(fresh.recover_all().unwrap(), 0, "an API process must not recover");
    assert_eq!(fresh.running_ids(), vec![id.clone()]);
    assert_eq!(s.fleet.launches(&id), 1, "the broadcast was relaunched by a deployment");
}

#[test]
fn a3_an_api_restart_does_not_start_a_second_encoder() {
    // Recovery is the runner's. If both halves recovered, every deployment
    // would double every broadcast.
    let s = split(2);
    let client = s.serve();
    for id in &s.broadcasts {
        s.api(&client).start(&s.user, id).unwrap();
    }
    eventually("both launched", || s.broadcasts.iter().all(|i| s.fleet.launches(i) == 1));

    for _ in 0..3 {
        let api = s.api(&client);
        assert_eq!(api.recover_all().unwrap(), 0);
        api.shutdown();
    }
    for id in &s.broadcasts {
        assert_eq!(s.fleet.launches(id), 1, "{id} was launched more than once");
    }
}

#[test]
fn a4_a_duplicate_start_through_the_socket_launches_once() {
    let s = split(1);
    let client = s.serve();
    let id = s.broadcasts[0].clone();
    let api = s.api(&client);

    api.start(&s.user, &id).unwrap();
    eventually("launched", || s.fleet.launches(&id) == 1);
    // A double click, a retried request, two tabs.
    for _ in 0..5 {
        let _ = api.start(&s.user, &id);
    }
    assert_eq!(s.fleet.launches(&id), 1, "a repeated start made a second encoder");
}

#[test]
fn a5_a_stop_from_the_api_really_stops_the_worker_before_it_returns() {
    // Synchronous, because the caller goes on to end the YouTube broadcast.
    let s = split(1);
    let client = s.serve();
    let id = s.broadcasts[0].clone();
    let api = s.api(&client);

    api.start(&s.user, &id).unwrap();
    eventually("launched", || s.fleet.launches(&id) == 1);

    api.stop(&s.user, &id).unwrap();
    assert!(!s.fleet.is_alive(&id), "stop returned while the encoder was still sending");
    assert!(s.runner.running_ids().is_empty());
}

#[test]
fn a6_a_restart_through_the_socket_is_a_stop_and_a_start() {
    let s = split(1);
    let client = s.serve();
    let id = s.broadcasts[0].clone();
    let api = s.api(&client);

    api.start(&s.user, &id).unwrap();
    eventually("launched", || s.fleet.launches(&id) == 1);
    api.restart(&s.user, &id).unwrap();
    eventually("relaunched", || s.fleet.launches(&id) == 2);
    assert!(s.fleet.is_alive(&id));
    // The intent is kept, so a tick cannot read a restart as a stop.
    assert_eq!(s.db.desired_state(&id).unwrap(), louver_cloud::DesiredState::Running);
}

#[test]
fn a7_an_unreachable_runner_is_an_error_and_never_a_silent_success() {
    let s = split(1);
    // No `serve`, so nothing is listening.
    let client = Client::new(&s.data);
    let api = s.api(&client);

    let e = api.start(&s.user, &s.broadcasts[0]).unwrap_err().to_string();
    assert!(e.contains("연결할 수 없습니다"), "got: {e}");
    assert_eq!(s.fleet.launches(&s.broadcasts[0]), 0, "something was launched anyway");
}

#[test]
fn a8_an_unreachable_runner_does_not_stop_a_broadcast() {
    // §5-2's rule, one level down: not being able to ask is not a reason to
    // tear anything down.
    let s = split(1);
    let client = s.serve();
    let id = s.broadcasts[0].clone();
    s.api(&client).start(&s.user, &id).unwrap();
    eventually("launched", || s.fleet.launches(&id) == 1);

    // A client pointed at a socket that is not there, as an API would be while
    // the runner is briefly away.
    let elsewhere = tempfile::tempdir().unwrap();
    let blind = s.api(&Client::new(elsewhere.path()));
    // Asking it anything, and then stopping it, is what a deployment does.
    let _ = blind.running_ids();
    blind.shutdown();
    assert!(s.fleet.is_alive(&id), "a blind API stopped a live broadcast");
}

#[test]
fn a9_an_unreachable_runner_falls_back_to_the_database_rather_than_claiming_nothing_runs() {
    let s = split(1);
    let client = s.serve();
    let id = s.broadcasts[0].clone();
    s.api(&client).start(&s.user, &id).unwrap();
    eventually("launched", || s.fleet.launches(&id) == 1);
    // The runtime row the engine writes is what the fallback reads. A fake
    // process never pushes bytes, so it settles short of `Running` — which is
    // exactly why the fallback counts the states on the way up too.
    eventually("the runtime row left CREATED", || {
        s.db.broadcast(&id).map(|b| b.runtime_state != louver_cloud::RuntimeState::Created).unwrap_or(false)
    });

    let elsewhere = tempfile::tempdir().unwrap();
    let blind = s.api(&Client::new(elsewhere.path()));
    assert_eq!(
        blind.running_ids(),
        vec![id.clone()],
        "a dashboard must not say nothing is running just because the runner is away"
    );
}

#[test]
fn a10_the_runner_refuses_a_start_for_an_account_with_no_plan() {
    // Defence in depth: this path does not go through `start`, so the gate is
    // repeated, exactly as `recover_all` repeats it.
    let s = split(1);
    let client = s.serve();
    let id = s.broadcasts[0].clone();
    s.db.cancel_subscription(&s.user).unwrap();

    let e = client.call(&Command::Start(id.clone())).unwrap_err().to_string();
    assert!(!e.is_empty());
    assert_eq!(s.fleet.launches(&id), 0, "a broadcast ran for an account with no plan");
}

#[test]
fn a11_one_broadcasts_trouble_does_not_touch_another() {
    let s = split(3);
    let client = s.serve();
    let api = s.api(&client);
    for id in &s.broadcasts {
        api.start(&s.user, id).unwrap();
    }
    eventually("all three", || s.broadcasts.iter().all(|i| s.fleet.launches(i) == 1));

    api.stop(&s.user, &s.broadcasts[1]).unwrap();
    assert!(!s.fleet.is_alive(&s.broadcasts[1]));
    assert!(s.fleet.is_alive(&s.broadcasts[0]) && s.fleet.is_alive(&s.broadcasts[2]));
    assert_eq!(s.fleet.launches(&s.broadcasts[0]), 1, "a neighbour was restarted");
    assert_eq!(s.fleet.launches(&s.broadcasts[2]), 1, "a neighbour was restarted");
}

#[test]
fn a12_the_old_single_process_shape_still_works_exactly_as_before() {
    // The rollback path: no role, no socket, no runner. Everything in one
    // process, which is what every deployment does today.
    let s = split(1);
    let id = s.broadcasts[0].clone();
    assert!(!s.runner.is_remote());

    s.runner.start(&s.user, &id).unwrap();
    eventually("launched", || s.fleet.launches(&id) == 1);
    assert_eq!(s.runner.running_ids(), vec![id.clone()]);
    s.runner.shutdown();
    assert!(!s.fleet.is_alive(&id), "an all-in-one shutdown must stop the broadcast");
}

#[test]
fn a13_a_broadcast_keeps_its_id_and_its_destination_across_the_split() {
    // Nothing about the split may rewrite what the customer is watching.
    let s = split(1);
    let client = s.serve();
    let id = s.broadcasts[0].clone();
    let before = s.db.broadcast(&id).unwrap();

    let api = s.api(&client);
    api.start(&s.user, &id).unwrap();
    eventually("launched", || s.fleet.launches(&id) == 1);
    api.shutdown();
    let fresh = s.api(&client);
    let _ = fresh.running_ids();

    let after = s.db.broadcast(&id).unwrap();
    assert_eq!(after.id, before.id);
    assert_eq!(after.destination_id, before.destination_id);
    assert_eq!(after.youtube.broadcast_id, before.youtube.broadcast_id);
    assert_eq!(after.youtube.stream_id, before.youtube.stream_id);
}

#[test]
fn a14_the_roles_divide_the_work_without_overlapping() {
    assert!(Role::All.owns_workers() && Role::All.serves_http());
    assert!(Role::Api.serves_http() && !Role::Api.owns_workers());
    assert!(Role::Runner.owns_workers() && !Role::Runner.serves_http());
    // Exactly one side owns the data directory, and it is the side with the
    // processes.
    assert!(Role::Runner.takes_the_lease() && !Role::Api.takes_the_lease());
}
