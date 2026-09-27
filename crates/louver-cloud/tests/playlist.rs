//! §13 C–L: what the playlist actually does.
//!
//! These read the concat manifest the engine hands FFmpeg, which is the only
//! place the truth lives: the order of the files in that file is the order
//! YouTube receives, and `-stream_loop -1` on the command line is what makes it
//! repeat. Asserting on the manifest rather than on our own tables is what makes
//! these tests able to fail.

use louver_cloud::db::NewItem;
use louver_cloud::manager::{BroadcastManager, LauncherFactory};
use louver_cloud::storage::{LocalStorage, Storage};
use louver_cloud::{CloudDb, DesiredState, RuntimeState};
use louver_core::error::Result as CoreResult;
use louver_core::runtime::StreamLauncher;
use louver_core::security::{MemorySecretStore, SecretStore};
use louver_core::streaming::ffmpeg::FfmpegTools;
use louver_core::streaming::supervisor::{ProcessHandle, StreamSupervisor};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

// --- a process that records what it was told to play ------------------------

#[derive(Default, Debug)]
struct Launch {
    /// The files in the concat manifest, in order.
    files: Vec<String>,
    /// The whole argument list, so a flag can be checked.
    args: Vec<String>,
}

#[derive(Default, Debug)]
struct Recorder {
    launches: Mutex<Vec<Launch>>,
    live: Mutex<Option<Arc<AtomicBool>>>,
    next_pid: AtomicU32,
}

impl Recorder {
    fn launches(&self) -> usize {
        self.launches.lock().unwrap().len()
    }
    fn last(&self) -> Launch {
        let l = self.launches.lock().unwrap();
        let x = l.last().expect("nothing was launched");
        Launch { files: x.files.clone(), args: x.args.clone() }
    }
    /// Make the running process look like it died on its own.
    fn crash(&self) {
        if let Some(flag) = self.live.lock().unwrap().as_ref() {
            flag.store(true, Ordering::SeqCst);
        }
    }
}

#[derive(Debug)]
struct RecordingLaunchers {
    rec: Arc<Recorder>,
}

impl LauncherFactory for RecordingLaunchers {
    fn for_broadcast(&self, _db: &CloudDb, _id: &str) -> Arc<dyn StreamLauncher> {
        Arc::new(One { rec: Arc::clone(&self.rec) })
    }
}

#[derive(Debug)]
struct One {
    rec: Arc<Recorder>,
}

impl StreamLauncher for One {
    fn launch(&self, _s: &mut StreamSupervisor, args: &[String]) -> CoreResult<Box<dyn ProcessHandle>> {
        // The manifest is the argument after `-i`.
        let manifest =
            args.iter().position(|a| a == "-i").and_then(|i| args.get(i + 1)).cloned().unwrap_or_default();
        let files = std::fs::read_to_string(&manifest)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.strip_prefix("file '").and_then(|r| r.strip_suffix('\'')))
            .map(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect();
        self.rec.launches.lock().unwrap().push(Launch { files, args: args.to_vec() });

        let exited = Arc::new(AtomicBool::new(false));
        *self.rec.live.lock().unwrap() = Some(Arc::clone(&exited));
        Ok(Box::new(Fake { exited, pid: self.rec.next_pid.fetch_add(1, Ordering::SeqCst) + 7000 }))
    }
}

struct Fake {
    exited: Arc<AtomicBool>,
    pid: u32,
}

impl ProcessHandle for Fake {
    fn try_exited(&mut self) -> Option<bool> {
        // `false` means "exited, and not because we asked" — a crash.
        self.exited.load(Ordering::SeqCst).then_some(false)
    }
    fn terminate(&mut self) -> CoreResult<()> {
        self.exited.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn pid(&self) -> Option<u32> {
        Some(self.pid)
    }
}

// --- harness ---------------------------------------------------------------

struct Env {
    dir: tempfile::TempDir,
    db: CloudDb,
    mgr: BroadcastManager,
    rec: Arc<Recorder>,
    user: String,
    dest: String,
}

fn env(plan: &str) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let db = CloudDb::open(&dir.path().join("cloud.db")).unwrap();
    let rec = Arc::new(Recorder::default());
    let keys: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
    let mgr = BroadcastManager::new(
        db.clone(),
        Arc::new(LocalStorage::new(dir.path().join("media"))),
        dir.path().join("work"),
        FfmpegTools::new("ffmpeg", "ffprobe"),
        "libx264".into(),
        Arc::clone(&keys),
        Arc::new(RecordingLaunchers { rec: Arc::clone(&rec) }),
    );
    let user = db.create_user("dj@example.com", "hash", plan).unwrap().id;
    let dest = db.create_destination(&user, "채널", "rtmps://a/live2", "••••").unwrap().id;
    keys.set(&louver_cloud::credentials::destination_account(&dest), "aaaa-bbbb-cccc-dddd").unwrap();
    Env { dir, db, mgr, rec, user, dest }
}

impl Env {
    /// A prepared video, ready to be put in a playlist.
    fn video(&self, name: &str, secs: f64) -> String {
        let store = LocalStorage::new(self.dir.path().join("media"));
        let src = self.dir.path().join(name);
        std::fs::write(&src, format!("bytes of {name}")).unwrap();
        let key = store.put_file(&self.user, name, &src).unwrap();
        let m = self.db.create_media(&self.user, name, 42, &key).unwrap();
        self.db.record_media_prepared(&m.id, &key, secs, 42).unwrap();
        m.id
    }

    fn broadcast(&self, name: &str, media: &[String], loop_forever: bool) -> String {
        let b = self.db.create_broadcast(&self.user, name, &media[0], &self.dest, loop_forever).unwrap();
        let items: Vec<NewItem> =
            media.iter().map(|m| NewItem { media_id: m.clone(), enabled: true, repeat_count: 1 }).collect();
        self.db.replace_items(&self.user, &b.id, &items).unwrap();
        b.id
    }

    fn wait_for_launches(&self, n: usize) {
        // The supervisor's first reconnect backoff is a few seconds, so this has
        // to be patient enough to see a relaunch rather than declare it missing.
        for _ in 0..400 {
            if self.rec.launches() >= n {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("expected {n} launches, saw {}", self.rec.launches());
    }
}

/// The prepared file's name ends with the original filename, so the manifest can
/// be read back as a list of videos.
fn played(files: &[String]) -> Vec<String> {
    files.iter().map(|f| f.rsplit('-').next().unwrap_or(f).to_string()).collect()
}

// --- C, D ------------------------------------------------------------------

#[test]
fn three_videos_play_in_order_in_one_continuous_stream() {
    let e = env("business");
    let a = e.video("one.mp4", 60.0);
    let b = e.video("two.mp4", 90.0);
    let c = e.video("three.mp4", 30.0);
    let id = e.broadcast("믹스", &[a, b, c], true);

    e.mgr.start(&e.user, &id).unwrap();
    e.wait_for_launches(1);
    let launch = e.rec.last();

    // The order the user arranged is the order in the manifest.
    assert_eq!(played(&launch.files), ["one.mp4", "two.mp4", "three.mp4"]);

    // One FFmpeg, one connection, looping for ever: this is what makes a change
    // of video not a disconnection from YouTube.
    assert_eq!(launch.args.iter().filter(|a| *a == "-i").count(), 1, "one input");
    assert!(launch.args.windows(2).any(|w| w == ["-stream_loop", "-1"]), "{:?}", launch.args);
    assert!(launch.args.windows(2).any(|w| w == ["-c", "copy"]), "stream copy, not a re-encode");
    // And only one process for the whole playlist.
    assert_eq!(e.rec.launches(), 1);

    e.mgr.shutdown();
}

#[test]
fn a_repeated_item_appears_as_often_as_it_repeats() {
    let e = env("business");
    let a = e.video("intro.mp4", 10.0);
    let b = e.video("track.mp4", 60.0);
    let id = e.broadcast("반복", &[a.clone(), b.clone()], true);
    e.db.replace_items(
        &e.user,
        &id,
        &[
            NewItem { media_id: a, enabled: true, repeat_count: 1 },
            NewItem { media_id: b, enabled: true, repeat_count: 3 },
        ],
    )
    .unwrap();

    e.mgr.start(&e.user, &id).unwrap();
    e.wait_for_launches(1);
    assert_eq!(played(&e.rec.last().files), ["intro.mp4", "track.mp4", "track.mp4", "track.mp4"]);
    // The editor still counts two videos; the runtime counts four plays.
    let b = e.db.broadcast_owned(&e.user, &id).unwrap();
    assert_eq!((b.item_count, b.play_count), (2, 4));
    e.mgr.shutdown();
}

#[test]
fn a_disabled_item_is_left_out_without_being_deleted() {
    let e = env("business");
    let a = e.video("keep.mp4", 60.0);
    let b = e.video("skip.mp4", 60.0);
    let id = e.broadcast("건너뛰기", &[a.clone(), b.clone()], true);
    e.db.replace_items(
        &e.user,
        &id,
        &[
            NewItem { media_id: a, enabled: true, repeat_count: 1 },
            NewItem { media_id: b, enabled: false, repeat_count: 1 },
        ],
    )
    .unwrap();

    e.mgr.start(&e.user, &id).unwrap();
    e.wait_for_launches(1);
    assert_eq!(played(&e.rec.last().files), ["keep.mp4"]);
    // Still two rows: unchecking is not deleting.
    assert_eq!(e.db.items_owned(&e.user, &id).unwrap().len(), 2);
    e.mgr.shutdown();
}

// --- E ---------------------------------------------------------------------

#[test]
fn reordering_is_stored_and_is_what_plays_next_time() {
    let e = env("business");
    let a = e.video("a.mp4", 60.0);
    let b = e.video("b.mp4", 60.0);
    let c = e.video("c.mp4", 60.0);
    let id = e.broadcast("순서", &[a.clone(), b.clone(), c.clone()], true);

    // Drag the last to the front.
    let reordered = [c.clone(), a.clone(), b.clone()]
        .iter()
        .map(|m| NewItem { media_id: m.clone(), enabled: true, repeat_count: 1 })
        .collect::<Vec<_>>();
    let saved = e.db.replace_items(&e.user, &id, &reordered).unwrap();
    assert_eq!(saved.iter().map(|i| i.position).collect::<Vec<_>>(), [0, 1, 2], "dense positions");
    assert_eq!(saved.iter().map(|i| i.filename.as_str()).collect::<Vec<_>>(), ["c.mp4", "a.mp4", "b.mp4"]);

    // Read back from a new handle on the same file: it survived the write.
    let again = CloudDb::open(&e.dir.path().join("cloud.db")).unwrap();
    assert_eq!(
        again.items_for(&id).unwrap().iter().map(|i| i.filename.as_str()).collect::<Vec<_>>(),
        ["c.mp4", "a.mp4", "b.mp4"]
    );

    e.mgr.start(&e.user, &id).unwrap();
    e.wait_for_launches(1);
    assert_eq!(played(&e.rec.last().files), ["c.mp4", "a.mp4", "b.mp4"]);
    e.mgr.shutdown();
}

// --- F, G, N ---------------------------------------------------------------

#[test]
fn a_crash_during_the_second_video_resumes_at_the_second_video() {
    let e = env("business");
    let a = e.video("first.mp4", 60.0);
    let b = e.video("second.mp4", 60.0);
    let c = e.video("third.mp4", 60.0);
    let id = e.broadcast("복구", &[a, b, c], true);

    e.mgr.start(&e.user, &id).unwrap();
    e.wait_for_launches(1);
    assert_eq!(played(&e.rec.last().files)[0], "first.mp4");

    // The server dies. Nothing is written after that, so the row keeps the last
    // checkpoint the worker left — which is the point of checkpointing.
    e.mgr.shutdown();
    e.db.record_playlist_progress(
        &id,
        &louver_cloud::PlaylistProgress {
            index: 2,
            current_item: Some("second.mp4".into()),
            next_item: Some("third.mp4".into()),
            position_secs: 12.0,
            duration_secs: 60.0,
            cycle_secs: 180.0,
            play_count: 3,
        },
    )
    .unwrap();

    // It comes back: recovery restarts what was meant to be running.
    let recovered = e.mgr.recover_all().unwrap();
    assert_eq!(recovered, 1);
    e.wait_for_launches(2);

    // Same videos, same order, beginning where it left off rather than at the top.
    assert_eq!(played(&e.rec.last().files), ["second.mp4", "third.mp4", "first.mp4"]);

    // And the dashboard still counts in the user's order, not the rotated one.
    let b = e.db.broadcast_owned(&e.user, &id).unwrap();
    assert_eq!(b.current_index, 2, "position is reported in the arranged order");
    e.mgr.shutdown();
}

#[test]
fn a_dead_ffmpeg_is_replaced_and_the_playlist_keeps_going() {
    let e = env("business");
    let a = e.video("x.mp4", 60.0);
    let b = e.video("y.mp4", 60.0);
    let id = e.broadcast("재연결", &[a, b], true);

    e.mgr.start(&e.user, &id).unwrap();
    e.wait_for_launches(1);
    e.rec.crash();
    e.wait_for_launches(2);

    let b = e.db.broadcast_owned(&e.user, &id).unwrap();
    assert_eq!(b.desired_state, DesiredState::Running, "a crash is not an instruction to stop");
    assert!(b.restart_count >= 1, "the restart was counted");
    assert_eq!(played(&e.rec.last().files).len(), 2, "the whole playlist came back");
    e.mgr.shutdown();
}

// --- H ---------------------------------------------------------------------

#[test]
fn a_playlist_the_user_stopped_is_never_restarted() {
    let e = env("business");
    let a = e.video("one.mp4", 60.0);
    let b = e.video("two.mp4", 60.0);
    let id = e.broadcast("정지", &[a, b], true);

    e.mgr.start(&e.user, &id).unwrap();
    e.wait_for_launches(1);
    e.mgr.stop(&e.user, &id).unwrap();

    let launches = e.rec.launches();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert_eq!(e.rec.launches(), launches, "something started it again");
    assert_eq!(e.mgr.recover_all().unwrap(), 0, "recovery must not resume a deliberate stop");
    assert_eq!(e.db.broadcast_owned(&e.user, &id).unwrap().desired_state, DesiredState::Stopped);
}

// --- I ---------------------------------------------------------------------

#[test]
fn a_scheduled_broadcast_starts_when_its_time_comes_and_not_before() {
    let e = env("business");
    let a = e.video("night.mp4", 60.0);
    let id = e.broadcast("예약", &[a], true);

    let start = chrono::Utc::now() + chrono::Duration::minutes(10);
    e.db.update_broadcast_owned(
        &e.user,
        &id,
        &louver_cloud::BroadcastPatch {
            schedule: Some(louver_cloud::Schedule {
                enabled: true,
                start_at: Some(start.to_rfc3339()),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .unwrap();

    // Ten minutes early: nothing happens.
    e.mgr.run_schedules(chrono::Utc::now());
    assert_eq!(e.rec.launches(), 0);
    assert_eq!(e.db.broadcast_owned(&e.user, &id).unwrap().desired_state, DesiredState::Stopped);

    // At the time: it starts, without anyone's browser being open.
    e.mgr.run_schedules(start + chrono::Duration::seconds(5));
    e.wait_for_launches(1);
    assert_eq!(e.db.broadcast_owned(&e.user, &id).unwrap().desired_state, DesiredState::Running);

    // And a second pass does not start it twice.
    e.mgr.run_schedules(start + chrono::Duration::minutes(1));
    assert_eq!(e.rec.launches(), 1);
    e.mgr.shutdown();
}

#[test]
fn a_scheduled_window_stops_itself_at_the_end() {
    let e = env("business");
    let a = e.video("window.mp4", 60.0);
    let id = e.broadcast("창", &[a], true);

    let start = chrono::Utc::now();
    e.db.update_broadcast_owned(
        &e.user,
        &id,
        &louver_cloud::BroadcastPatch {
            schedule: Some(louver_cloud::Schedule {
                enabled: true,
                start_at: Some(start.to_rfc3339()),
                stop_at: Some((start + chrono::Duration::hours(2)).to_rfc3339()),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .unwrap();

    e.mgr.run_schedules(start + chrono::Duration::seconds(1));
    e.wait_for_launches(1);
    e.mgr.run_schedules(start + chrono::Duration::hours(2));

    let b = e.db.broadcast_owned(&e.user, &id).unwrap();
    assert_eq!(b.desired_state, DesiredState::Stopped);
    assert_eq!(b.runtime_state, RuntimeState::Stopped);
}

// --- J ---------------------------------------------------------------------

#[test]
fn the_plan_limit_counts_playlists_the_same_as_anything_else() {
    let e = env("basic"); // one concurrent stream
    let a = e.video("a.mp4", 60.0);
    let b = e.video("b.mp4", 60.0);
    let first = e.broadcast("하나", &[a.clone(), b.clone()], true);
    let second = e.broadcast("둘", &[a, b], true);

    e.mgr.start(&e.user, &first).unwrap();
    e.wait_for_launches(1);
    let refused = e.mgr.start(&e.user, &second).unwrap_err();
    assert!(matches!(refused, louver_cloud::CloudError::LimitReached { .. }), "{refused:?}");
    assert_eq!(e.rec.launches(), 1, "the refusal must not have spawned anything");
    e.mgr.shutdown();
}

// --- K, L ------------------------------------------------------------------

#[test]
fn a_broadcast_made_before_playlists_existed_still_starts() {
    let e = env("business");
    let a = e.video("legacy.mp4", 120.0);
    // Exactly what the previous release wrote: a broadcast row with a media_id
    // and no items at all.
    let id = e.db.create_broadcast(&e.user, "옛 방송", &a, &e.dest, true).unwrap().id;
    e.db.raw().lock().unwrap().execute("DELETE FROM broadcast_items WHERE broadcast_id=?1", [&id]).unwrap();
    e.db.raw().lock().unwrap().execute("UPDATE broadcasts SET item_count=0 WHERE id=?1", [&id]).unwrap();

    // It plays, from the column the old code wrote.
    e.mgr.start(&e.user, &id).unwrap();
    e.wait_for_launches(1);
    assert_eq!(played(&e.rec.last().files), ["legacy.mp4"]);
    e.mgr.shutdown();

    // And opening the database again adopts it into a one-item playlist, so the
    // editor has something to show.
    let reopened = CloudDb::open(&e.dir.path().join("cloud.db")).unwrap();
    let items = reopened.items_for(&id).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].filename, "legacy.mp4");
    assert_eq!(items[0].position, 0);
    // The old column still points at the same video, for anything that reads it.
    assert_eq!(reopened.broadcast(&id).unwrap().media_id, a);
}

#[test]
fn a_stream_key_saved_before_this_release_still_opens_and_sends() {
    let e = env("business");
    // A destination row written the old way, with no `kind` column value of its
    // own, and its key sealed under the same account name.
    let dest =
        e.db.create_destination(&e.user, "옛 대상", "rtmps://a.rtmps.youtube.com/live2", "••••").unwrap();
    e.mgr
        .secret_store()
        .set(&louver_cloud::credentials::destination_account(&dest.id), "olde-keyy-1234-5678")
        .unwrap();

    let a = e.video("still.mp4", 60.0);
    let id = e.db.create_broadcast(&e.user, "옛 키", &a, &dest.id, true).unwrap().id;
    e.db.replace_items(&e.user, &id, &[NewItem { media_id: a, enabled: true, repeat_count: 1 }]).unwrap();

    e.mgr.start(&e.user, &id).unwrap();
    e.wait_for_launches(1);

    // It reached FFmpeg as an ingest URL, and the key is not in the log.
    let args = e.rec.last().args.join(" ");
    assert!(args.contains("rtmps://a.rtmps.youtube.com/live2/olde-keyy-1234-5678"), "{args}");
    let events = e.db.events_owned(&e.user, &id, 50).unwrap();
    let text = events.iter().map(|x| x.message.clone()).collect::<Vec<_>>().join("\n");
    assert!(!text.contains("olde-keyy"), "the key reached the event log: {text}");
    e.mgr.shutdown();
}
