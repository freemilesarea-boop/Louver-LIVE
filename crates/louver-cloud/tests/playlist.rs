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
    assert!(matches!(refused, louver_cloud::CloudError::ConcurrencyReached { .. }), "{refused:?}");
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

// --- the output path, pinned -------------------------------------------------

/// The exact command that reached YouTube before playlists existed, and has to
/// keep reaching it.
///
/// This is not a style check. The change that introduced playlists touched only
/// the input side — one playlist row became N — and this asserts that: the
/// flags, their order, the muxer, the stream copy and the shape of the ingest
/// URL are what the working version sent, whether the playlist has one video or
/// three.
#[test]
fn the_output_path_is_exactly_what_youtube_was_given_before() {
    for videos in [1usize, 3] {
        let e = env("business");
        e.mgr
            .secret_store()
            .set(&louver_cloud::credentials::destination_account(&e.dest), "abcd-1234-efgh-5678")
            .unwrap();
        let media: Vec<String> = (0..videos).map(|i| e.video(&format!("v{i}.mp4"), 60.0)).collect();
        let id = e.broadcast("출력 경로", &media, true);

        e.mgr.start(&e.user, &id).unwrap();
        e.wait_for_launches(1);
        let args = e.rec.last().args;

        // The input side: one concat manifest, looping for ever.
        let i = args.iter().position(|a| a == "-i").expect("no input");
        assert_eq!(args.iter().filter(|a| *a == "-i").count(), 1, "more than one input");
        assert_eq!(
            args[i - 6..i],
            ["-stream_loop", "-1", "-f", "concat", "-safe", "0"].map(String::from),
            "the input flags changed: {args:?}"
        );

        // Read at wall-clock speed, or the whole playlist would be pushed as
        // fast as the disk can read it.
        assert!(args.contains(&"-re".to_string()), "{args:?}");

        // The output side: a straight copy into FLV over RTMPS, with the flag
        // that stops FFmpeg writing a duration into a live stream.
        let n = args.len();
        assert_eq!(
            args[n - 7..n - 1],
            ["-c", "copy", "-f", "flv", "-flvflags", "no_duration_filesize"].map(String::from),
            "the output flags changed: {args:?}"
        );

        // And the destination is the ingest URL, key last, nothing added.
        let destination = args.last().unwrap();
        assert_eq!(destination, "rtmps://a/live2/abcd-1234-efgh-5678", "{args:?}");

        // Nothing that would re-encode: no scaler, no encoder, no bitrate.
        for forbidden in ["-vf", "-s", "-r", "-b:v", "libx264", "-c:v", "-c:a"] {
            assert!(!args.iter().any(|a| a == forbidden), "{forbidden} in a stream copy: {args:?}");
        }

        // The manifest holds exactly the videos, in order, and nothing else.
        assert_eq!(e.rec.last().files.len(), videos);
        e.mgr.shutdown();
    }
}

/// A single-video broadcast and a one-item playlist must be the same command.
///
/// The regression this guards against is the one that was suspected: that the
/// playlist rewrite changed what a broadcast made before it sends.
#[test]
fn a_legacy_broadcast_and_a_one_item_playlist_send_the_same_command() {
    let e = env("business");
    e.mgr
        .secret_store()
        .set(&louver_cloud::credentials::destination_account(&e.dest), "abcd-1234-efgh-5678")
        .unwrap();
    let a = e.video("only.mp4", 90.0);

    // The pre-playlist shape: a row with a media_id and no items.
    let legacy = e.db.create_broadcast(&e.user, "옛 방송", &a, &e.dest, true).unwrap().id;
    e.db.raw()
        .lock()
        .unwrap()
        .execute("DELETE FROM broadcast_items WHERE broadcast_id=?1", [&legacy])
        .unwrap();
    e.mgr.start(&e.user, &legacy).unwrap();
    e.wait_for_launches(1);
    let before = e.rec.last();
    e.mgr.stop(&e.user, &legacy).unwrap();

    // The new shape: the same video as a one-item playlist.
    let modern = e.broadcast("새 방송", &[a], true);
    e.mgr.start(&e.user, &modern).unwrap();
    e.wait_for_launches(2);
    let after = e.rec.last();
    e.mgr.shutdown();

    // Same files, and the same argv apart from the working directory each
    // broadcast gets for its own manifest.
    assert_eq!(before.files, after.files);
    let strip = |args: Vec<String>| -> Vec<String> {
        args.into_iter()
            .map(|a| if a.ends_with("manifest.txt") { "MANIFEST".to_string() } else { a })
            .collect()
    };
    assert_eq!(strip(before.args), strip(after.args));
}

// --- subscription enforcement through the manager ---------------------------
//
// The gate lives in the database, but the paths that reach it are the manager's:
// a user pressing START, the scheduler reaching an occurrence, and a boot
// recovering whatever was running. The third is the one that would be missed,
// because nobody is watching a boot.

/// Take away the subscription an account has, the way cancelling does.
fn unsubscribe(e: &Env) {
    e.db.cancel_subscription(&e.user).unwrap();
}

#[test]
fn an_account_with_no_subscription_cannot_start_a_broadcast() {
    let e = env("business");
    let a = e.video("one.mp4", 60.0);
    let id = e.broadcast("밤 라디오", &[a], true);
    unsubscribe(&e);

    let refused = e.mgr.start(&e.user, &id).unwrap_err();
    assert!(matches!(refused, louver_cloud::CloudError::NoSubscription), "{refused:?}");
    assert_eq!(e.rec.launches(), 0, "no FFmpeg may be spawned for an unsubscribed account");
    assert_eq!(
        e.db.broadcast(&id).unwrap().desired_state,
        DesiredState::Stopped,
        "a refused start must not leave the broadcast wanting to run"
    );
    e.mgr.shutdown();
}

#[test]
fn the_scheduler_does_not_start_an_unsubscribed_accounts_broadcast() {
    let e = env("business");
    let a = e.video("one.mp4", 60.0);
    let id = e.broadcast("예약 방송", &[a], true);

    // A schedule whose moment is now.
    let now = chrono::Utc::now();
    e.db.update_broadcast_owned(
        &e.user,
        &id,
        &louver_cloud::BroadcastPatch {
            schedule: Some(louver_cloud::Schedule {
                enabled: true,
                start_at: Some((now - chrono::Duration::minutes(1)).to_rfc3339()),
                stop_at: None,
                timezone: "UTC".into(),
                offset_minutes: 0,
                repeat_days: 0,
                last_run_at: None,
            }),
            ..Default::default()
        },
    )
    .unwrap();
    unsubscribe(&e);

    e.mgr.run_schedules(now);

    assert_eq!(e.rec.launches(), 0, "the scheduler started a broadcast with no subscription behind it");
    assert_eq!(e.db.broadcast(&id).unwrap().desired_state, DesiredState::Stopped);
    // And it said why, in words the user can act on.
    let log = e.db.events_for(&id, 50).unwrap().into_iter().map(|x| x.message).collect::<Vec<_>>().join("\n");
    assert!(log.contains("활성화된 요금제가 없어 예약 방송을 시작하지 못했습니다"), "{log}");
    e.mgr.shutdown();
}

#[test]
fn a_restart_does_not_revive_an_unsubscribed_accounts_broadcast() {
    let e = env("business");
    let a = e.video("one.mp4", 60.0);
    let id = e.broadcast("밤 라디오", &[a], true);

    // On air, legitimately, on Business.
    e.mgr.start(&e.user, &id).unwrap();
    e.wait_for_launches(1);
    e.mgr.shutdown();

    // The subscription ends while the server is down. `desired_state` is still
    // running, which is exactly what recovery reads.
    unsubscribe(&e);
    assert_eq!(e.db.broadcast(&id).unwrap().desired_state, DesiredState::Running);

    let launches_before = e.rec.launches();
    let recovered = e.mgr.recover_all().unwrap();
    assert_eq!(recovered, 0, "recovery revived a broadcast with no subscription behind it");
    assert_eq!(e.rec.launches(), launches_before, "and spawned nothing");

    // The intent is kept, not rewritten: subscribing again is meant to bring
    // this back, and discarding `desired_state` would silently lose that.
    assert_eq!(e.db.broadcast(&id).unwrap().desired_state, DesiredState::Running);
    let log = e.db.events_for(&id, 50).unwrap().into_iter().map(|x| x.message).collect::<Vec<_>>().join("\n");
    assert!(log.contains("활성화된 요금제가 없어 방송을 복구하지 않았습니다"), "{log}");

    // Subscribe again, and the same recovery brings it back.
    e.db.activate_subscription(&e.user, "business").unwrap();
    assert_eq!(e.mgr.recover_all().unwrap(), 1, "a paying account's recovery must still work");
    e.wait_for_launches(launches_before + 1);
    e.mgr.shutdown();
}

/// A PayApp that always agrees, so a cancellation can be driven end to end here.
/// What it answers with is pinned down in `payapp.rs`; what this file cares about
/// is what the manager does afterwards.
#[derive(Debug)]
struct AgreeableProvider;

impl louver_cloud::billing::FormPost for AgreeableProvider {
    fn post_form(&self, _url: &str, fields: &[(&str, &str)]) -> louver_cloud::Result<String> {
        let cmd = fields.iter().find(|(k, _)| *k == "cmd").map(|(_, v)| *v).unwrap_or("");
        Ok(match cmd {
            "rebillRegist" => {
                "state=1&errno=00000&rebill_no=99001&payurl=https%3A%2F%2Fpayapp.kr%2Fp%2F1".into()
            }
            _ => "state=1&errno=00000".to_string(),
        })
    }
}

#[test]
fn cancelling_a_subscription_closes_every_way_of_getting_on_air() {
    // Test G. The cancellation itself is PayApp's business; this is about what is
    // left afterwards. Creating, starting, restarting, the scheduler and a boot
    // recovery all have to be shut at once, because they are five different doors
    // into the same room and the last two have nobody watching them.
    let e = env("none");
    let config = louver_cloud::billing::Config {
        userid: "247streams".into(),
        linkkey: "link-key-for-tests-only".into(),
        linkval: "link-val-for-tests-only".into(),
        api_url: "https://fake.payapp.test/oapi/apiLoad.html".into(),
        public_url: "https://247streams.kr".into(),
    };
    let pay = louver_cloud::billing::Payapp::new(e.db.clone(), Arc::new(AgreeableProvider), config);

    // Pay for Pro, the way a real account does: register, then a verified
    // notification.
    let order = pay.checkout(&e.user, "pro", "01012345678").unwrap();
    let feedback: std::collections::BTreeMap<String, String> = [
        ("userid", "247streams"),
        ("linkkey", "link-key-for-tests-only"),
        ("linkval", "link-val-for-tests-only"),
        ("price", "39900"),
        ("pay_state", "4"),
        ("pay_date", "2026-09-27 12:00:05"),
        ("pay_type", "card"),
        ("mul_no", "990001"),
        ("rebill_no", "99001"),
        ("var1", &order.billing_id),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    assert!(pay.handle_feedback(&feedback).is_accepted());

    // On air on Pro, and a schedule set for a moment that has just passed.
    let a = e.video("one.mp4", 60.0);
    let id = e.broadcast("밤 라디오", &[a], true);
    let now = chrono::Utc::now();
    e.db.update_broadcast_owned(
        &e.user,
        &id,
        &louver_cloud::BroadcastPatch {
            schedule: Some(louver_cloud::Schedule {
                enabled: true,
                start_at: Some((now - chrono::Duration::minutes(1)).to_rfc3339()),
                stop_at: None,
                timezone: "UTC".into(),
                offset_minutes: 0,
                repeat_days: 0,
                last_run_at: None,
            }),
            ..Default::default()
        },
    )
    .unwrap();
    e.mgr.start(&e.user, &id).unwrap();
    e.wait_for_launches(1);
    e.mgr.shutdown();
    let launches = e.rec.launches();

    // Cancel, for real: PayApp is asked, and the entitlement goes with it.
    pay.cancel(&e.user).unwrap();
    assert!(!e.db.subscription(&e.user).unwrap().active);

    // 1. A new broadcast cannot be created.
    let b = e.video("two.mp4", 30.0);
    let refused = e.db.create_broadcast(&e.user, "새 방송", &b, &e.dest, true).unwrap_err();
    assert!(matches!(refused, louver_cloud::CloudError::NoSubscription), "{refused:?}");

    // 2. START is refused, and 3. so is a restart, which goes through it.
    assert!(matches!(e.mgr.start(&e.user, &id).unwrap_err(), louver_cloud::CloudError::NoSubscription));
    assert!(matches!(e.mgr.restart(&e.user, &id).unwrap_err(), louver_cloud::CloudError::NoSubscription));

    // 4. The scheduler passes its occurrence and starts nothing.
    e.mgr.run_schedules(chrono::Utc::now());

    // 5. And a boot does not bring it back.
    assert_eq!(e.mgr.recover_all().unwrap(), 0);

    assert_eq!(e.rec.launches(), launches, "nothing may be spawned after a cancellation");
    e.mgr.shutdown();
}

#[test]
fn a_paying_accounts_plan_decides_how_many_streams_it_gets() {
    for (plan, allowed) in [("basic", 1usize), ("pro", 2), ("business", 3)] {
        let e = env(plan);
        let clip = e.video("one.mp4", 60.0);
        let ids: Vec<String> = (0..allowed + 1)
            .map(|i| e.broadcast(&format!("{i}"), std::slice::from_ref(&clip), true))
            .collect();

        for (i, id) in ids.iter().take(allowed).enumerate() {
            e.mgr.start(&e.user, id).unwrap_or_else(|x| panic!("{plan} stream {i} refused: {x}"));
        }
        e.wait_for_launches(allowed);

        let refused = e.mgr.start(&e.user, &ids[allowed]).unwrap_err();
        match &refused {
            louver_cloud::CloudError::ConcurrencyReached { allowed: a, .. } => {
                assert_eq!(*a as usize, allowed, "{plan}")
            }
            other => panic!("{plan} allowed one too many: {other:?}"),
        }
        assert_eq!(e.rec.launches(), allowed, "{plan} spawned a process for a refused start");
        e.mgr.shutdown();
    }
}

// --- fast-path media keeps every part of the lifecycle --------------------
//
// Preparation now leaves most uploads at their own size and frame rate, so the
// broadcasts built on them must behave exactly as the canonical ones did:
// start, crash, recover, stop, loop, single pass. These drive the real manager
// over media marked the way the fast path marks them.

/// Mark this media the way a fast-path preparation does.
fn mark_native(e: &Env, media_id: &str, signature: &str) {
    e.db.record_prepared_signature(media_id, "direct", signature).unwrap();
}

#[test]
fn a_broadcast_on_fast_path_media_starts_crashes_recovers_and_stops() {
    // Test H. Nothing about the lifecycle knows or cares how the file was
    // prepared — but that is a claim, and this is the check.
    let e = env("business");
    let a = e.video("one.mp4", 60.0);
    let b = e.video("two.mp4", 60.0);
    mark_native(&e, &a, "h264/1280x720/2/1");
    mark_native(&e, &b, "h264/1280x720/2/1");
    let id = e.broadcast("밤 라디오", &[a, b], true);

    e.mgr.start(&e.user, &id).unwrap();
    e.wait_for_launches(1);
    // Both items are in the manifest, in order.
    assert_eq!(played(&e.rec.last().files), ["one.mp4", "two.mp4"]);

    // A crash is still the supervisor's business.
    e.rec.crash();
    e.wait_for_launches(2);
    assert_eq!(e.db.broadcast(&id).unwrap().desired_state, DesiredState::Running);

    // A restart of the server still brings it back.
    e.mgr.shutdown();
    assert_eq!(e.mgr.recover_all().unwrap(), 1);
    e.wait_for_launches(3);

    e.mgr.stop(&e.user, &id).unwrap();
    assert_eq!(e.db.broadcast(&id).unwrap().desired_state, DesiredState::Stopped);
    e.mgr.shutdown();
}

#[test]
fn a_playlist_whose_items_cannot_be_joined_never_reaches_ffmpeg() {
    // The rule that makes the fast path safe, enforced where it counts: not at
    // the edit, which a user can skip, but at the moment something would be
    // sent. Both the start button and a boot recovery go through it.
    let e = env("business");
    let a = e.video("one.mp4", 60.0);
    let b = e.video("two.mp4", 60.0);
    mark_native(&e, &a, "h264/1280x720/2/1/aaaa");
    mark_native(&e, &b, "h264/1920x1080/30/1/bbbb");
    let id = e.broadcast("섞인 방송", &[a, b], true);

    let refused = e.mgr.start(&e.user, &id).unwrap_err();
    assert!(refused.to_string().contains("영상 형식이 서로 달라"), "{refused}");
    assert_eq!(e.rec.launches(), 0, "an unjoinable playlist reached FFmpeg");

    // And recovery holds the same line, where nobody is watching.
    e.db.update_broadcast_owned(
        &e.user,
        &id,
        &louver_cloud::BroadcastPatch { loop_forever: Some(true), ..Default::default() },
    )
    .unwrap();
    e.db.claim_stream_slot(&e.user, &id).unwrap();
    assert_eq!(e.db.broadcast(&id).unwrap().desired_state, DesiredState::Running);
    assert_eq!(e.mgr.recover_all().unwrap(), 0, "recovery started an unjoinable playlist");
    assert_eq!(e.rec.launches(), 0);
    e.mgr.shutdown();
}

#[test]
fn loop_and_single_pass_are_unchanged_on_fast_path_media() {
    // Test I. "전체 반복" keeps `-stream_loop -1` and never ends by itself;
    // "한 바퀴만" still ends after one pass of the playlist's own length.
    let e = env("business");
    let a = e.video("short.mp4", 2.0);
    mark_native(&e, &a, "h264/1280x720/2/1");
    let looping = e.broadcast("반복", std::slice::from_ref(&a), true);

    e.mgr.start(&e.user, &looping).unwrap();
    e.wait_for_launches(1);
    assert!(
        e.rec.last().args.windows(2).any(|w| w == ["-stream_loop", "-1"]),
        "the looping flag went missing",
    );
    std::thread::sleep(std::time::Duration::from_millis(2500));
    assert_eq!(
        e.db.broadcast(&looping).unwrap().desired_state,
        DesiredState::Running,
        "a looping broadcast ended itself",
    );
    e.mgr.stop(&e.user, &looping).unwrap();

    let b = e.video("once.mp4", 2.0);
    mark_native(&e, &b, "h264/1280x720/2/1");
    let once = e.broadcast("한 바퀴", &[b], false);
    e.mgr.start(&e.user, &once).unwrap();
    e.wait_for_launches(2);
    for _ in 0..200 {
        if e.db.broadcast(&once).unwrap().desired_state == DesiredState::Stopped {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let row = e.db.broadcast(&once).unwrap();
    assert_eq!(row.desired_state, DesiredState::Stopped, "a single pass never ended");
    assert_eq!(row.runtime_state, RuntimeState::Stopped);
    e.mgr.shutdown();
}
