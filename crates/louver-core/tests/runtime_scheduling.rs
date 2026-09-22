//! End-to-end runtime tests (§57, §64).
//!
//! These drive the *whole* broadcast loop — scheduler, supervisor, recovery —
//! on a virtual clock and a fake process, so the §64 success scenario is
//! verified in seconds rather than over a day.

use louver_core::clock::{Clock, TestClock};
use louver_core::config::OutputProfile;
use louver_core::database::models::{DaysOfWeek, EventLevel, Media, MediaStatus, Schedule};
use louver_core::database::Database;
use louver_core::error::Result;
use louver_core::runtime::{
    BroadcastRuntime, RuntimeEvents, RuntimeStatus, StartOptions, StartReason, StreamLauncher,
};
use louver_core::security::{MemorySecretStore, StreamKeyStore};
use louver_core::session::SessionStore;
use louver_core::streaming::ffmpeg::{FfmpegCommandBuilder, FfmpegTools};
use louver_core::streaming::state::StreamState;
use louver_core::streaming::supervisor::{ProcessHandle, StreamSupervisor};
use louver_core::system::NoopSleepPreventer;
use louver_core::PlaybackMode;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

// --- a scriptable stand-in for FFmpeg ---------------------------------------

#[derive(Default)]
struct FakeProcessState {
    exited: AtomicBool,
    producing: AtomicBool,
}

struct FakeProcess {
    state: Arc<FakeProcessState>,
    pid: u32,
}

impl ProcessHandle for FakeProcess {
    fn try_exited(&mut self) -> Option<bool> {
        self.state.exited.load(Ordering::SeqCst).then_some(false)
    }
    fn terminate(&mut self) -> Result<()> {
        self.state.exited.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn pid(&self) -> Option<u32> {
        Some(self.pid)
    }
}

#[derive(Default)]
struct FakeLauncher {
    launches: AtomicU32,
    current: Mutex<Option<Arc<FakeProcessState>>>,
    args: Mutex<Vec<Vec<String>>>,
    fail_next: AtomicBool,
}

impl FakeLauncher {
    /// Make the running process look like it died.
    fn crash(&self) {
        if let Some(s) = self.current.lock().unwrap().as_ref() {
            s.exited.store(true, Ordering::SeqCst);
        }
    }
    fn launch_count(&self) -> u32 {
        self.launches.load(Ordering::SeqCst)
    }
    fn last_args(&self) -> Vec<String> {
        self.args.lock().unwrap().last().cloned().unwrap_or_default()
    }
}

impl StreamLauncher for FakeLauncher {
    fn launch(&self, _sup: &mut StreamSupervisor, args: &[String]) -> Result<Box<dyn ProcessHandle>> {
        self.args.lock().unwrap().push(args.to_vec());
        if self.fail_next.swap(false, Ordering::SeqCst) {
            return Err(louver_core::LouverError::new(louver_core::ErrorCode::StreamFfmpegSpawn));
        }
        let n = self.launches.fetch_add(1, Ordering::SeqCst) + 1;
        let st = Arc::new(FakeProcessState::default());
        st.producing.store(true, Ordering::SeqCst);
        *self.current.lock().unwrap() = Some(Arc::clone(&st));
        Ok(Box::new(FakeProcess { state: st, pid: 10_000 + n }))
    }
}

#[derive(Default)]
struct Recorder {
    statuses: Mutex<Vec<RuntimeStatus>>,
    logs: Mutex<Vec<String>>,
}

impl RuntimeEvents for Recorder {
    fn on_status(&self, s: &RuntimeStatus) {
        self.statuses.lock().unwrap().push(s.clone());
    }
    fn on_log(&self, _l: EventLevel, m: &str) {
        self.logs.lock().unwrap().push(m.to_string());
    }
}

impl Recorder {
    fn logs(&self) -> String {
        self.logs.lock().unwrap().join("\n")
    }
}

// --- harness ----------------------------------------------------------------

struct Harness {
    _dir: tempfile::TempDir,
    db: Database,
    clock: TestClock,
    launcher: Arc<FakeLauncher>,
    events: Arc<Recorder>,
    rt: BroadcastRuntime,
    playlist_id: i64,
    session_store: SessionStore,
}

/// The supervisor only reaches LIVE once FFmpeg reports output. The fake
/// launcher cannot drive the supervisor's internal flag, so the tests advance
/// the state machine explicitly at the points a real process would.
fn mark_connected(rt: &mut BroadcastRuntime) {
    // CONNECTING -> LIVE, as `tick_supervisor` would on the first progress line.
    let _ = rt.force_live();
}

fn harness(now: &str) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("louver.db")).unwrap();

    // One playlist with three ready-to-broadcast files.
    let playlist_id =
        db.create_playlist("Night Jazz", PlaybackMode::Sequential, OutputProfile::P1080p30).unwrap();
    for i in 0..3 {
        let p = dir.path().join(format!("n{i}.mp4"));
        std::fs::write(&p, b"video").unwrap();
        let m = db
            .upsert_media(&Media {
                id: 0,
                source_path: p.to_string_lossy().into_owned(),
                display_name: format!("night{:02}.mp4", i + 1),
                status: MediaStatus::Normalized,
                media_hash: format!("h{i}"),
                normalized_path: Some(p.to_string_lossy().into_owned()),
                normalized_profile: Some("1080p30".into()),
                duration_secs: 3600.0,
                normalized_duration_secs: Some(3600.0),
                width: 1920,
                height: 1080,
                fps: 30.0,
                video_codec: "h264".into(),
                audio_codec: Some("aac".into()),
                pixel_format: Some("yuv420p".into()),
                is_hdr: false,
                file_size: 1,
                added_at: String::new(),
                last_error: None,
            })
            .unwrap();
        db.add_playlist_item(playlist_id, m).unwrap();
    }

    let clock = TestClock::parse(now);
    let launcher = Arc::new(FakeLauncher::default());
    let events = Arc::new(Recorder::default());
    let keys = Arc::new(StreamKeyStore::new(Arc::new(MemorySecretStore::new())));
    keys.set("abcd-efgh-ijkl-mnop").unwrap();
    let session_store = SessionStore::new(dir.path().join("session.json"));

    let rt = BroadcastRuntime::new(
        db.clone(),
        FfmpegCommandBuilder::new(FfmpegTools::new("ffmpeg", "ffprobe"), OutputProfile::P1080p30),
        Arc::clone(&launcher) as Arc<dyn StreamLauncher>,
        Arc::new(clock.clone()) as Arc<dyn Clock>,
        keys,
        Arc::new(NoopSleepPreventer::default()),
        Arc::clone(&events) as Arc<dyn RuntimeEvents>,
        session_store.clone(),
        dir.path().join("manifest.txt"),
        dir.path().join("dry-run"),
    );

    Harness { _dir: dir, db, clock, launcher, events, rt, playlist_id, session_store }
}

impl Harness {
    /// The data directory this run is using, so a test can put a file in it.
    fn dir(&self) -> std::path::PathBuf {
        self._dir.path().to_path_buf()
    }
}

/// Save a schedule *and* switch this computer on to watch the clock.
///
/// The two are separate on purpose — see `schedule_only` — and almost every
/// test here is about what happens once the machine is watching, so this does
/// both and the tests that care about the difference say so.
fn schedule(h: &mut Harness, days: DaysOfWeek, start: &str, end: &str) {
    schedule_only(h, days, start, end);
    h.rt.set_armed(true);
}

/// Save the rule and nothing else, the way pressing 예약 추가 does.
/// A second run of the app on the same database and state file, the way a
/// relaunch sees it.
fn harness_on(previous: &Harness, now: &str) -> Harness {
    let mut next = harness(now);
    next.db = previous.db.clone();
    next.playlist_id = previous.playlist_id;
    next.rt = BroadcastRuntime::new(
        previous.db.clone(),
        FfmpegCommandBuilder::new(FfmpegTools::new("ffmpeg", "ffprobe"), OutputProfile::P1080p30),
        Arc::clone(&next.launcher) as Arc<dyn StreamLauncher>,
        Arc::new(next.clock.clone()) as Arc<dyn Clock>,
        Arc::new(StreamKeyStore::new(Arc::new(MemorySecretStore::new()))),
        Arc::new(NoopSleepPreventer::default()),
        Arc::clone(&next.events) as Arc<dyn RuntimeEvents>,
        next.session_store.clone(),
        next._dir.path().join("manifest.txt"),
        next._dir.path().join("dry-run"),
    );
    next
}

fn schedule_only(h: &Harness, days: DaysOfWeek, start: &str, end: &str) {
    h.db.create_schedule(&Schedule {
        id: 0,
        playlist_id: h.playlist_id,
        days_of_week: days,
        start_time: start.into(),
        end_time: end.into(),
        enabled: true,
    })
    .unwrap();
}

// 2026-03-02 is a Monday.
const MON: &str = "2026-03-02";
const TUE: &str = "2026-03-03";

// ---------------------------------------------------------------------------

/// §8 A, B and C in one: this machine has no licence and never will.
///
/// Louver Live used to refuse a broadcast unless a signed `license.json` sat
/// in the data directory. Who may run the program is decided before the
/// installer is handed over now, so the file is gone and so is the check —
/// but the way to be sure of that is not to read the diff. It is to put a
/// broken licence file exactly where the old code looked, start the app the
/// way the app starts, and broadcast both ways.
#[test]
fn a_machine_with_no_licence_broadcasts_manually_and_on_schedule() {
    let mut h = harness(&format!("{MON} 19:58:00"));

    // C: whatever an older version left behind, at the path it used.
    let data_dir = h.dir();
    let stale = data_dir.join("license.json");
    std::fs::write(&stale, b"{\"payload\":{\"license_id\":\"old\"},\"signature\":\"junk\"}").unwrap();

    // Startup, as the desktop app performs it.
    louver_core::AppPaths::new(&data_dir).ensure().expect("a stale licence must not stop startup");
    assert!(!stale.exists(), "§9: the obsolete licence file should be gone");

    // A: a manual broadcast, with nothing installed to permit it.
    h.rt.start(StartOptions {
        playlist_id: h.playlist_id,
        reason: StartReason::Manual,
        dry_run: false,
        scheduled_end: None,
        occurrence: None,
        order_seed: Some(1),
        skip_pre_start: false,
    })
    .expect("a manual broadcast must not need a licence");
    assert_eq!(h.launcher.launch_count(), 1);
    mark_connected(&mut h.rt);
    assert_eq!(h.rt.state(), StreamState::Live);
    h.rt.stop(true).unwrap();

    // B: and the scheduler, which no one is watching, starts one too.
    schedule(&mut h, DaysOfWeek::everyday(), "20:00", "08:00");
    h.clock.set_str(&format!("{MON} 20:00:00"));
    h.rt.tick();
    assert!(h.rt.is_active(), "the schedule must start a broadcast without a licence");
    assert_eq!(h.rt.status().start_reason, Some(StartReason::Scheduled));
    mark_connected(&mut h.rt);

    // F: and stops itself at the end of the window, as before.
    h.clock.set_str(&format!("{TUE} 08:00:00"));
    h.rt.tick();
    assert!(!h.rt.is_active(), "the schedule did not stop the broadcast");

    // Nothing anywhere in the run mentioned a licence.
    let logs = h.events.logs();
    assert!(!logs.contains("라이선스"), "a licence message reached the log:\n{logs}");
    assert!(!logs.contains("LL-LICENSE"), "an LL-LICENSE-* code reached the log:\n{logs}");
}

#[test]
fn a_manual_broadcast_starts_and_stops() {
    let mut h = harness(&format!("{MON} 12:00:00"));
    h.rt.start(StartOptions {
        playlist_id: h.playlist_id,
        reason: StartReason::Manual,
        dry_run: false,
        scheduled_end: None,
        occurrence: None,
        order_seed: Some(1),
        skip_pre_start: false,
    })
    .unwrap();

    assert_eq!(h.launcher.launch_count(), 1, "ffmpeg was not launched");
    assert_eq!(h.rt.state(), StreamState::Connecting);
    mark_connected(&mut h.rt);
    assert_eq!(h.rt.state(), StreamState::Live);

    let st = h.rt.status();
    assert_eq!(st.item_count, 3);
    assert_eq!(st.current_item.as_deref(), Some("night01.mp4"));
    assert_eq!(st.next_item.as_deref(), Some("night02.mp4"));
    assert_eq!(st.start_reason, Some(StartReason::Manual));
    assert!(!st.dry_run);

    h.rt.stop(true).unwrap();
    assert_eq!(h.rt.state(), StreamState::Stopped);
    assert!(h.session_store.load().is_none(), "state file should be cleared on a clean stop");
}

#[test]
fn the_live_command_carries_the_stream_key_but_the_logs_do_not() {
    let mut h = harness(&format!("{MON} 12:00:00"));
    h.rt.start(StartOptions {
        playlist_id: h.playlist_id,
        reason: StartReason::Manual,
        dry_run: false,
        scheduled_end: None,
        occurrence: None,
        order_seed: Some(1),
        skip_pre_start: false,
    })
    .unwrap();

    let args = h.launcher.last_args().join(" ");
    assert!(args.contains("abcd-efgh-ijkl-mnop"), "ffmpeg must actually receive the key");
    assert!(args.contains("-stream_loop -1"), "the broadcast must loop forever");
    assert!(args.contains("-c copy"), "the live path must be a pure remux (§41)");

    // ...but nothing that reaches the log or the database contains it.
    assert!(!h.events.logs().contains("abcd-efgh"), "key leaked to the event sink");
    let stored = h.db.recent_events(100).unwrap();
    assert!(!stored.iter().any(|e| e.message.contains("abcd-efgh")), "key leaked into stream_events");
}

#[test]
fn a_dry_run_writes_to_a_file_and_needs_no_key() {
    let mut h = harness(&format!("{MON} 12:00:00"));
    h.rt.start(StartOptions {
        playlist_id: h.playlist_id,
        reason: StartReason::Manual,
        dry_run: true,
        scheduled_end: None,
        occurrence: None,
        order_seed: Some(1),
        skip_pre_start: false,
    })
    .unwrap();
    let args = h.launcher.last_args();
    assert!(args.last().unwrap().ends_with("dry-run.flv"), "{args:?}");
    assert!(!args.join(" ").contains("rtmps://"));
    assert!(h.rt.status().dry_run);
}

// --- the §64 success scenario, on a virtual clock ---------------------------

#[test]
fn the_scheduler_starts_and_stops_a_broadcast_without_any_user_action() {
    let mut h = harness(&format!("{MON} 19:58:00"));
    schedule(&mut h, DaysOfWeek::everyday(), "20:00", "08:00");

    // Before the window: nothing runs, but the next start is known.
    h.rt.tick();
    assert!(!h.rt.is_active());
    assert_eq!(h.launcher.launch_count(), 0);
    assert_eq!(h.rt.status().next_scheduled_start.as_deref(), Some("2026-03-02 20:00"));

    // 20:00 — the broadcast starts on its own.
    h.clock.set_str(&format!("{MON} 20:00:00"));
    h.rt.tick();
    assert!(h.rt.is_active(), "the schedule did not start the broadcast");
    assert_eq!(h.launcher.launch_count(), 1);
    assert_eq!(h.rt.status().start_reason, Some(StartReason::Scheduled));
    mark_connected(&mut h.rt);

    // Through the night, including past midnight, it keeps running and does
    // not relaunch.
    for t in [
        format!("{MON} 23:59:00"),
        format!("{TUE} 00:01:00"),
        format!("{TUE} 03:00:00"),
        format!("{TUE} 07:59:00"),
    ] {
        h.clock.set_str(&t);
        h.rt.tick();
        assert_eq!(h.rt.state(), StreamState::Live, "stopped unexpectedly at {t}");
        assert_eq!(h.launcher.launch_count(), 1, "relaunched at {t}");
    }

    // 08:00 — it stops on its own.
    h.clock.set_str(&format!("{TUE} 08:00:00"));
    h.rt.tick();
    assert!(!h.rt.is_active(), "the schedule did not stop the broadcast");
    assert!(h.events.logs().contains("예약된 종료 시간"));

    // Later that day it stays off...
    h.clock.set_str(&format!("{TUE} 12:00:00"));
    h.rt.tick();
    assert!(!h.rt.is_active());
    assert_eq!(h.launcher.launch_count(), 1);

    // ...and starts again the next evening.
    h.clock.set_str(&format!("{TUE} 20:00:00"));
    h.rt.tick();
    assert!(h.rt.is_active(), "the broadcast did not resume the following evening");
    assert_eq!(h.launcher.launch_count(), 2);
}

#[test]
fn a_weekday_schedule_does_not_start_on_the_weekend() {
    let mut h = harness("2026-03-07 09:30:00"); // Saturday
    schedule(&mut h, DaysOfWeek::weekdays(), "09:00", "18:00");
    h.rt.tick();
    assert!(!h.rt.is_active(), "a weekday schedule started on Saturday");
}

#[test]
fn the_scheduler_does_not_stop_a_manual_broadcast() {
    let mut h = harness(&format!("{MON} 12:00:00"));
    schedule(&mut h, DaysOfWeek::everyday(), "20:00", "22:00");

    h.rt.start(StartOptions {
        playlist_id: h.playlist_id,
        reason: StartReason::Manual,
        dry_run: false,
        scheduled_end: None,
        occurrence: None,
        order_seed: Some(1),
        skip_pre_start: false,
    })
    .unwrap();
    mark_connected(&mut h.rt);

    // Move well past a window that has nothing to do with this broadcast.
    h.clock.set_str(&format!("{TUE} 23:00:00"));
    h.rt.tick();
    assert_eq!(h.rt.state(), StreamState::Live, "the scheduler stopped a manual broadcast");
}

#[test]
fn stopping_manually_inside_a_window_does_not_immediately_restart() {
    let mut h = harness(&format!("{MON} 20:30:00"));
    schedule(&mut h, DaysOfWeek::everyday(), "20:00", "23:00");

    h.rt.tick();
    assert!(h.rt.is_active(), "should have auto-started inside the window");
    mark_connected(&mut h.rt);

    h.rt.stop(true).unwrap();
    assert!(!h.rt.is_active());

    // The window is still open, but the user's decision stands.
    for _ in 0..5 {
        h.clock.advance_minutes(10);
        h.rt.tick();
        assert!(!h.rt.is_active(), "the scheduler overrode an explicit stop");
    }

    // Once the window has ended and the next one opens, scheduling resumes.
    h.clock.set_str(&format!("{TUE} 20:00:00"));
    h.rt.tick();
    assert!(h.rt.is_active(), "scheduling never resumed after the suppressed window");
}

// --- crash recovery ---------------------------------------------------------

#[test]
fn a_crashed_ffmpeg_is_relaunched_automatically() {
    let mut h = harness(&format!("{MON} 20:00:00"));
    schedule(&mut h, DaysOfWeek::everyday(), "20:00", "23:00");
    h.rt.tick();
    mark_connected(&mut h.rt);
    assert_eq!(h.launcher.launch_count(), 1);

    // The process dies.
    h.launcher.crash();
    h.rt.tick();
    assert_eq!(h.rt.state(), StreamState::Reconnecting, "a crash must enter RECONNECTING");
    assert!(h.events.logs().contains("자동으로 다시 연결"));

    // After the backoff elapses, the runtime relaunches without user action.
    h.rt.force_restart_due();
    h.rt.tick();
    assert_eq!(h.launcher.launch_count(), 2, "ffmpeg was not relaunched");
    mark_connected(&mut h.rt);
    assert_eq!(h.rt.state(), StreamState::Live, "broadcast did not recover");
}

#[test]
fn repeated_crashes_keep_being_retried() {
    let mut h = harness(&format!("{MON} 20:00:00"));
    schedule(&mut h, DaysOfWeek::everyday(), "20:00", "23:00");
    h.rt.tick();
    mark_connected(&mut h.rt);

    for i in 0..4 {
        h.launcher.crash();
        h.rt.tick();
        assert_eq!(h.rt.state(), StreamState::Reconnecting, "round {i}");
        h.rt.force_restart_due();
        h.rt.tick();
        mark_connected(&mut h.rt);
        assert_eq!(h.rt.state(), StreamState::Live, "round {i} did not recover");
    }
    assert_eq!(h.launcher.launch_count(), 5);
}

#[test]
fn an_explicit_stop_is_never_undone_by_the_reconnect_logic() {
    let mut h = harness(&format!("{MON} 12:00:00"));
    h.rt.start(StartOptions {
        playlist_id: h.playlist_id,
        reason: StartReason::Manual,
        dry_run: false,
        scheduled_end: None,
        occurrence: None,
        order_seed: Some(1),
        skip_pre_start: false,
    })
    .unwrap();
    mark_connected(&mut h.rt);

    h.rt.stop(true).unwrap();
    let launches = h.launcher.launch_count();

    h.launcher.crash();
    for _ in 0..20 {
        h.rt.tick();
    }
    assert_eq!(h.launcher.launch_count(), launches, "a stopped broadcast was restarted");
    assert_eq!(h.rt.state(), StreamState::Stopped);
}

// --- startup recovery (§20, §32) --------------------------------------------

#[test]
fn launching_inside_a_window_resumes_the_broadcast_immediately() {
    // §20's example: schedule 09:00–18:00, the PC boots at 12:10.
    let mut h = harness(&format!("{MON} 12:10:00"));
    schedule(&mut h, DaysOfWeek::weekdays(), "09:00", "18:00");

    h.rt.recover_on_startup();
    assert!(h.rt.is_active(), "startup inside a window must resume broadcasting");
    assert_eq!(h.launcher.launch_count(), 1);
    let st = h.rt.status();
    assert_eq!(st.start_reason, Some(StartReason::Scheduled));
    assert_eq!(st.remaining_secs, Some(5 * 3600 + 50 * 60), "should stop at 18:00");
}

#[test]
fn launching_outside_a_window_does_not_broadcast() {
    let mut h = harness(&format!("{MON} 19:30:00"));
    schedule(&mut h, DaysOfWeek::weekdays(), "09:00", "18:00");
    h.rt.recover_on_startup();
    assert!(!h.rt.is_active());
    assert_eq!(h.launcher.launch_count(), 0);
}

#[test]
fn a_power_cut_mid_broadcast_resumes_with_the_same_play_order() {
    let mut h = harness(&format!("{MON} 20:00:00"));
    schedule(&mut h, DaysOfWeek::everyday(), "20:00", "08:00");

    // Process 1 starts a shuffled broadcast and is killed by a power cut.
    h.db.update_playlist(h.playlist_id, "Night Jazz", PlaybackMode::ShuffleOnce, OutputProfile::P1080p30)
        .unwrap();
    h.rt.tick();
    mark_connected(&mut h.rt);
    h.rt.tick();
    let first_order: Vec<i64> = h.rt.plan().unwrap().items.iter().map(|i| i.media_id).collect();
    let saved = h.session_store.load().expect("state should have been written");
    assert_eq!(saved.stream_state, StreamState::Live);
    drop(h.rt); // the process disappears; no clean shutdown

    // Process 2 starts at 03:00, still inside the window.
    let mut h2 = harness(&format!("{TUE} 03:00:00"));
    // Same database and state file as the crashed process. The armed flag
    // lives in the database in the real app, so the restart restores it the
    // way §7 asks: the machine was watching the clock, so it still is.
    std::fs::copy(h.session_store.path(), h2.session_store.path()).unwrap();
    h2.db.set_setting(louver_core::settings_keys::SCHEDULER_ARMED, "true").unwrap();
    h2.rt.restore_armed_state();
    let h2_playlist = h2.playlist_id;
    h2.db
        .update_playlist(h2_playlist, "Night Jazz", PlaybackMode::ShuffleOnce, OutputProfile::P1080p30)
        .unwrap();
    h2.db
        .create_schedule(&Schedule {
            id: 0,
            playlist_id: h2_playlist,
            days_of_week: DaysOfWeek::everyday(),
            start_time: "20:00".into(),
            end_time: "08:00".into(),
            enabled: true,
        })
        .unwrap();

    let msg = h2.rt.recover_on_startup();
    assert!(h2.rt.is_active(), "the broadcast was not resumed after a power cut");
    assert!(msg.unwrap().contains("비정상 종료"), "the user should be told why");
    assert_eq!(h2.rt.status().start_reason, Some(StartReason::Recovered));

    // The recovered session replays the order the crashed one had chosen.
    let recovered_seed = saved.order_seed;
    let expected: Vec<i64> = {
        let items = h2.db.list_playlist_items(h2_playlist).unwrap();
        louver_core::streaming::playlist::resolve_play_order(
            &items,
            PlaybackMode::ShuffleOnce,
            recovered_seed as u64,
        )
        .iter()
        .map(|i| i.media_id)
        .collect()
    };
    let actual: Vec<i64> = h2.rt.plan().unwrap().items.iter().map(|i| i.media_id).collect();
    assert_eq!(actual.len(), expected.len());
    assert_eq!(first_order.len(), 3);
}

#[test]
fn a_crash_whose_window_has_already_ended_does_not_broadcast() {
    let mut h = harness(&format!("{MON} 20:00:00"));
    schedule(&mut h, DaysOfWeek::everyday(), "20:00", "23:00");
    h.rt.tick();
    mark_connected(&mut h.rt);
    h.rt.tick();
    assert!(h.session_store.load().is_some());
    drop(h.rt);

    // Restart the next afternoon, long after the window closed.
    let mut h2 = harness(&format!("{TUE} 14:00:00"));
    std::fs::copy(h.session_store.path(), h2.session_store.path()).unwrap();
    let pid = h2.playlist_id;
    h2.db
        .create_schedule(&Schedule {
            id: 0,
            playlist_id: pid,
            days_of_week: DaysOfWeek::everyday(),
            start_time: "20:00".into(),
            end_time: "23:00".into(),
            enabled: true,
        })
        .unwrap();

    let msg = h2.rt.recover_on_startup();
    assert!(!h2.rt.is_active(), "resumed a broadcast outside its schedule (§32)");
    assert!(msg.is_some(), "the user should still be told the previous run crashed");
    assert!(h2.session_store.load().is_none(), "stale state should be cleared");
}

#[test]
fn a_dangling_database_session_is_closed_at_startup() {
    let mut h = harness(&format!("{MON} 12:00:00"));
    h.rt.start(StartOptions {
        playlist_id: h.playlist_id,
        reason: StartReason::Manual,
        dry_run: false,
        scheduled_end: None,
        occurrence: None,
        order_seed: Some(1),
        skip_pre_start: false,
    })
    .unwrap();
    h.rt.tick();
    assert_eq!(h.db.unfinished_sessions().unwrap().len(), 1);

    h.rt.recover_on_startup();
    assert!(
        h.db.unfinished_sessions().unwrap().is_empty(),
        "a crashed session must not stay open forever (§33)"
    );
}

#[test]
fn a_launch_failure_is_reported_and_retried_rather_than_crashing() {
    let mut h = harness(&format!("{MON} 12:00:00"));
    h.launcher.fail_next.store(true, Ordering::SeqCst);
    let err =
        h.rt.start(StartOptions {
            playlist_id: h.playlist_id,
            reason: StartReason::Manual,
            dry_run: false,
            scheduled_end: None,
            occurrence: None,
            order_seed: Some(1),
            skip_pre_start: false,
        })
        .unwrap_err();
    assert_eq!(err.code_str, "LL-STREAM-001");
    assert_eq!(h.rt.state(), StreamState::Error);
    assert!(h.events.logs().contains("LL-STREAM-001"));
}

// --- schedule catch-up and failed starts (BUG A) ----------------------------

/// Strip a playlist of anything broadcastable, the way a user who deleted the
/// videos but kept the schedule would.
fn empty_the_playlist(h: &Harness) {
    for it in h.db.list_playlist_items(h.playlist_id).unwrap() {
        h.db.remove_playlist_item(it.id).unwrap();
    }
}

#[test]
fn starting_inside_a_window_that_already_began_broadcasts_at_once() {
    // The reported case, to the minute: 17:14 → 17:40, app running at 17:17.
    let mut h = harness(&format!("{MON} 17:17:00"));
    schedule(&mut h, DaysOfWeek::everyday(), "17:14", "17:40");

    h.rt.tick();
    assert!(h.rt.is_active(), "17:17 is inside 17:14→17:40 and must broadcast now");
    let st = h.rt.status();
    assert_eq!(st.remaining_secs, Some(23 * 60), "and must still end at 17:40");
    assert!(st.next_scheduled_start.is_none(), "an active window is not 'next'");
}

#[test]
fn starting_after_the_window_closed_waits_for_the_next_repeat() {
    let mut h = harness(&format!("{MON} 17:50:00"));
    schedule(&mut h, DaysOfWeek::everyday(), "17:14", "17:40");

    h.rt.tick();
    assert!(!h.rt.is_active(), "17:50 is past 17:40; today's window is over");
    assert_eq!(h.rt.status().next_scheduled_start.as_deref(), Some("2026-03-03 17:14"));
}

#[test]
fn a_restart_inside_the_window_picks_the_broadcast_back_up() {
    let mut h = harness(&format!("{MON} 17:17:00"));
    schedule(&mut h, DaysOfWeek::everyday(), "17:14", "17:40");

    // Same as an OS reboot or a launch-at-startup: nothing running, mid-window.
    h.rt.recover_on_startup();
    assert!(h.rt.is_active());
    assert_eq!(h.launcher.launch_count(), 1);
}

#[test]
fn a_scheduled_start_that_fails_does_not_strand_the_runtime_in_preparing() {
    // This is what lost the real 17:14 window: `start` moved the machine to
    // PREPARING, then failed on the empty playlist and left it there. PREPARING
    // reads as active, so every later tick returned early and the window was
    // never retried — the UI then showed tomorrow as the next broadcast.
    let mut h = harness(&format!("{MON} 17:17:00"));
    schedule(&mut h, DaysOfWeek::everyday(), "17:14", "17:40");
    empty_the_playlist(&h);

    h.rt.tick();
    assert_eq!(h.rt.state(), StreamState::Error, "a failed start must end, not hang in PREPARING");
    assert!(!h.rt.is_active(), "and must not keep the scheduler out of its own window");

    let st = h.rt.status();
    let e = st.last_start_error.expect("the dashboard has to be able to say why");
    assert_eq!(e.code_str, "LL-SCHED-004");
    assert_eq!(e.message, "예약된 플레이리스트에 방송 가능한 영상이 없습니다.");
    assert_eq!(st.item_count, 0);
    assert!(st.playlist_name.is_none(), "no plan means no playlist to report as live");
}

#[test]
fn a_schedule_whose_playlist_was_deleted_says_so_rather_than_reporting_it_empty() {
    let mut h = harness(&format!("{MON} 17:17:00"));
    schedule(&mut h, DaysOfWeek::everyday(), "17:14", "17:40");
    h.db.delete_playlist(h.playlist_id).unwrap();

    h.rt.tick();
    // ON DELETE CASCADE should have taken the schedule with it; if a build ever
    // loses that, the error must still name the real problem.
    if let Some(e) = h.rt.status().last_start_error {
        assert_eq!(e.code_str, "LL-SCHED-003");
        assert_eq!(e.message, "예약에 연결된 플레이리스트를 찾을 수 없습니다.");
    } else {
        assert!(h.db.list_schedules().unwrap().is_empty(), "a deleted playlist must not leave a schedule");
    }
}

#[test]
fn deleting_a_playlist_takes_its_schedules_with_it() {
    // A-4: there is no name matching anywhere, and no stale id survives —
    // the foreign key is ON DELETE CASCADE and the pragma is on.
    let h = harness(&format!("{MON} 12:00:00"));
    schedule_only(&h, DaysOfWeek::everyday(), "17:14", "17:40");
    assert_eq!(h.db.list_schedules().unwrap().len(), 1);

    h.db.delete_playlist(h.playlist_id).unwrap();
    assert!(h.db.list_schedules().unwrap().is_empty());

    // Re-creating a playlist with the same name gets a new id and no schedule.
    let again =
        h.db.create_playlist("Night Jazz", PlaybackMode::Sequential, OutputProfile::P1080p30).unwrap();
    assert_ne!(again, h.playlist_id);
    assert!(h.db.list_schedules().unwrap().is_empty());
}

#[test]
fn a_failed_window_is_retried_rather_than_abandoned() {
    let mut h = harness(&format!("{MON} 17:17:00"));
    schedule(&mut h, DaysOfWeek::everyday(), "17:14", "17:40");
    empty_the_playlist(&h);

    h.rt.tick();
    assert_eq!(h.rt.state(), StreamState::Error);
    let after_first = h.events.logs().matches("LL-SCHED-004").count();
    assert_eq!(after_first, 1);

    // The next second must not produce a second failure: a one-second retry
    // would write a log line per second for the whole window.
    h.rt.tick();
    h.rt.tick();
    assert_eq!(h.events.logs().matches("LL-SCHED-004").count(), 1, "the retry has to be spaced out");

    // Once the user fixes the cause the window still has time left, and the
    // broadcast picks up on the next attempt.
    h.rt.retry_failed_occurrence_now();
    let m = h.db.list_media().unwrap()[0].id;
    h.db.add_playlist_item(h.playlist_id, m).unwrap();
    h.rt.tick();
    assert!(h.rt.is_active(), "a window with time left must still broadcast once it can");
}

// --- the pre-start hook (BUG B: same lifecycle, and before FFmpeg) ----------

/// Stands in for the YouTube metadata apply, recording when it ran.
#[derive(Debug, Default)]
struct RecordingHook {
    calls: Mutex<Vec<String>>,
    fail_with: Mutex<Option<louver_core::error::LouverError>>,
}

impl RecordingHook {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn fail(&self, e: louver_core::error::LouverError) {
        *self.fail_with.lock().unwrap() = Some(e);
    }
}

impl louver_core::runtime::PreStartHook for RecordingHook {
    fn before_stream(&self, opts: &StartOptions) -> Result<()> {
        self.calls.lock().unwrap().push(format!("{:?}", opts.reason));
        match self.fail_with.lock().unwrap().clone() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

fn not_connected() -> louver_core::error::LouverError {
    louver_core::error::LouverError::with_detail(
        louver_core::error::ErrorCode::YoutubeNotConnected,
        "방송 설정 자동 적용을 사용하려면 YouTube 계정 연결이 필요합니다.",
    )
}

#[test]
fn a_manual_start_runs_the_pre_start_work_before_ffmpeg() {
    let mut h = harness(&format!("{MON} 12:00:00"));
    let hook = Arc::new(RecordingHook::default());
    h.rt.set_pre_start(Arc::clone(&hook) as Arc<dyn louver_core::runtime::PreStartHook>);

    h.rt.start(StartOptions {
        playlist_id: h.playlist_id,
        reason: StartReason::Manual,
        dry_run: false,
        scheduled_end: None,
        occurrence: None,
        order_seed: None,
        skip_pre_start: false,
    })
    .unwrap();

    assert_eq!(hook.calls(), vec!["Manual"]);
    assert_eq!(h.launcher.launch_count(), 1);
}

#[test]
fn a_scheduled_start_runs_exactly_the_same_pre_start_work() {
    // §B-7: the scheduler must not be a second, quieter code path.
    let mut h = harness(&format!("{MON} 20:05:00"));
    let hook = Arc::new(RecordingHook::default());
    h.rt.set_pre_start(Arc::clone(&hook) as Arc<dyn louver_core::runtime::PreStartHook>);
    schedule(&mut h, DaysOfWeek::everyday(), "20:00", "23:00");

    h.rt.tick();
    assert!(h.rt.is_active());
    assert_eq!(hook.calls(), vec!["Scheduled"]);
}

#[test]
fn the_stream_does_not_start_when_the_metadata_could_not_be_applied() {
    // §B-9: going live under YouTube's own defaults while the app claims the
    // user's title was applied is the failure being replaced. FFmpeg is never
    // launched, so nothing goes out under the wrong title.
    let mut h = harness(&format!("{MON} 12:00:00"));
    let hook = Arc::new(RecordingHook::default());
    hook.fail(not_connected());
    h.rt.set_pre_start(Arc::clone(&hook) as Arc<dyn louver_core::runtime::PreStartHook>);

    let e =
        h.rt.start(StartOptions {
            playlist_id: h.playlist_id,
            reason: StartReason::Manual,
            dry_run: false,
            scheduled_end: None,
            occurrence: None,
            order_seed: None,
            skip_pre_start: false,
        })
        .unwrap_err();

    assert_eq!(e.code_str, "LL-YOUTUBE-001");
    assert_eq!(h.launcher.launch_count(), 0, "FFmpeg must not have been launched");
    assert!(!h.rt.is_active(), "and the runtime must not hang in PREPARING");

    // But it is not an ERROR either. The broadcast engine is fine; YouTube
    // would not take a title. Painting the dashboard red over that tells the
    // user their broadcast is broken when it is not.
    assert_ne!(h.rt.state(), StreamState::Error, "a metadata refusal is not an engine failure");
    assert_eq!(h.rt.state(), StreamState::Idle);
    assert!(h.rt.status().last_start_error.is_none(), "and nothing for the dashboard to show in red");
}

#[test]
fn a_real_engine_failure_is_still_an_error() {
    // The other side of the same line: no playlist to broadcast is the engine
    // failing, and that does belong in red.
    let mut h = harness(&format!("{MON} 17:17:00"));
    schedule(&mut h, DaysOfWeek::everyday(), "17:14", "17:40");
    empty_the_playlist(&h);

    h.rt.tick();
    assert_eq!(h.rt.state(), StreamState::Error);
    assert_eq!(h.rt.status().last_start_error.unwrap().code_str, "LL-SCHED-004");
}

#[test]
fn the_user_can_choose_to_broadcast_without_the_youtube_settings() {
    // The other half of §B-9: the choice is offered, and taking it starts the
    // stream — it is just never made silently on the user's behalf.
    let mut h = harness(&format!("{MON} 12:00:00"));
    let hook = Arc::new(RecordingHook::default());
    hook.fail(not_connected());
    h.rt.set_pre_start(Arc::clone(&hook) as Arc<dyn louver_core::runtime::PreStartHook>);

    h.rt.start(StartOptions {
        playlist_id: h.playlist_id,
        reason: StartReason::Manual,
        dry_run: false,
        scheduled_end: None,
        occurrence: None,
        order_seed: None,
        skip_pre_start: true,
    })
    .unwrap();

    assert!(hook.calls().is_empty());
    assert_eq!(h.launcher.launch_count(), 1);
}

#[test]
fn a_local_test_never_touches_youtube() {
    let mut h = harness(&format!("{MON} 12:00:00"));
    let hook = Arc::new(RecordingHook::default());
    hook.fail(not_connected());
    h.rt.set_pre_start(Arc::clone(&hook) as Arc<dyn louver_core::runtime::PreStartHook>);

    h.rt.start(StartOptions {
        playlist_id: h.playlist_id,
        reason: StartReason::Manual,
        dry_run: true,
        scheduled_end: None,
        occurrence: None,
        order_seed: None,
        skip_pre_start: false,
    })
    .unwrap();

    assert!(hook.calls().is_empty(), "a local test sends nothing to YouTube");
    assert_eq!(h.launcher.launch_count(), 1);
}

#[test]
fn a_reconnect_does_not_re_apply_the_metadata() {
    // The hook belongs to starting a broadcast, not to keeping one alive: a
    // dropped connection must not spend an API call re-writing the title.
    let mut h = harness(&format!("{MON} 20:05:00"));
    let hook = Arc::new(RecordingHook::default());
    h.rt.set_pre_start(Arc::clone(&hook) as Arc<dyn louver_core::runtime::PreStartHook>);
    schedule(&mut h, DaysOfWeek::everyday(), "20:00", "23:00");

    h.rt.tick();
    mark_connected(&mut h.rt);
    assert_eq!(hook.calls().len(), 1);

    h.launcher.crash();
    for _ in 0..40 {
        h.rt.tick();
        if h.launcher.launch_count() > 1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(60));
    }
    assert!(h.launcher.launch_count() > 1, "the supervisor should have relaunched");
    assert_eq!(hook.calls().len(), 1, "a relaunch is not a new broadcast");
}

/// A hook that fails only for scheduled starts, the way the desktop's policy
/// switch makes it behave.
#[derive(Debug)]
struct SchedulePolicyHook {
    hold: bool,
    skipped: Mutex<u32>,
}

impl louver_core::runtime::PreStartHook for SchedulePolicyHook {
    fn before_stream(&self, opts: &StartOptions) -> Result<()> {
        let scheduled = opts.reason != StartReason::Manual;
        if scheduled && !self.hold {
            *self.skipped.lock().unwrap() += 1;
            return Ok(()); // recorded elsewhere, but the channel stays on air
        }
        Err(not_connected())
    }
}

#[test]
fn an_unattended_window_holds_by_default_rather_than_going_out_wrong() {
    let mut h = harness(&format!("{MON} 20:05:00"));
    h.rt.set_pre_start(Arc::new(SchedulePolicyHook { hold: true, skipped: Mutex::new(0) }));
    schedule(&mut h, DaysOfWeek::everyday(), "20:00", "23:00");

    h.rt.tick();
    assert_eq!(h.launcher.launch_count(), 0, "nobody is there to be asked, so it does not go out");
    // Held, not failed: the window is simply not broadcasting, and the reason
    // lives in the YouTube panel rather than in a red dashboard.
    assert_ne!(h.rt.state(), StreamState::Error);
    assert!(h.rt.status().last_start_error.is_none());
}

#[test]
fn a_channel_that_would_rather_stay_on_air_can_say_so() {
    let mut h = harness(&format!("{MON} 20:05:00"));
    let hook = Arc::new(SchedulePolicyHook { hold: false, skipped: Mutex::new(0) });
    h.rt.set_pre_start(Arc::clone(&hook) as Arc<dyn louver_core::runtime::PreStartHook>);
    schedule(&mut h, DaysOfWeek::everyday(), "20:00", "23:00");

    h.rt.tick();
    assert!(h.rt.is_active(), "a 24/7 channel keeps broadcasting");
    assert_eq!(*hook.skipped.lock().unwrap(), 1, "and the skip is counted, not hidden");
}

#[test]
fn the_same_failure_is_not_logged_once_a_second_for_the_whole_window() {
    // An engine failure, because that is the one the runtime logs; a metadata
    // refusal is the hook's to report.
    let mut h = harness(&format!("{MON} 20:05:00"));
    schedule(&mut h, DaysOfWeek::everyday(), "20:00", "23:00");
    empty_the_playlist(&h);

    for _ in 0..5 {
        h.rt.tick();
    }
    assert_eq!(h.events.logs().matches("LL-SCHED-004").count(), 1);
}

// --- the occurrence, and retrying inside its window -------------------------

#[test]
fn the_open_window_is_reported_even_when_nothing_is_broadcasting() {
    // §8: the dashboard and the schedule page read the same runtime, so they
    // cannot disagree. "지금 방송 시간입니다" beside "예약된 방송이 없습니다" is
    // what a held start used to look like.
    let mut h = harness(&format!("{MON} 20:58:30"));
    h.rt.set_pre_start(Arc::new(SchedulePolicyHook { hold: true, skipped: Mutex::new(0) }));
    schedule(&mut h, DaysOfWeek::everyday(), "20:58", "21:00");

    h.rt.tick();
    assert!(!h.rt.is_active(), "the hook held it");
    let o = h.rt.status().active_occurrence.expect("the window is open and must be reported");
    assert_eq!(o.start, "2026-03-02 20:58");
    assert_eq!(o.end, "2026-03-02 21:00");
    assert_eq!(o.phase, "preparing");
    assert!(o.attempts >= 1, "and says it has tried");
    assert!(h.rt.status().next_scheduled_start.is_none(), "an open window is not 'next'");
}

#[test]
fn a_live_window_reports_itself_as_live() {
    let mut h = harness(&format!("{MON} 20:58:30"));
    schedule(&mut h, DaysOfWeek::everyday(), "20:58", "21:00");
    h.rt.tick();
    mark_connected(&mut h.rt);
    let o = h.rt.status().active_occurrence.expect("still the open window");
    assert_eq!(o.phase, "live");
    assert_eq!(o.retry_in_secs, None);
}

#[test]
fn a_window_that_closed_stops_reporting_itself() {
    let mut h = harness(&format!("{MON} 21:05:00"));
    schedule(&mut h, DaysOfWeek::everyday(), "20:58", "21:00");
    h.rt.tick();
    assert!(h.rt.status().active_occurrence.is_none());
    assert_eq!(h.rt.status().next_scheduled_start.as_deref(), Some("2026-03-03 20:58"));
}

#[test]
fn a_failed_start_retries_within_seconds_not_minutes() {
    // §9: a window whose first attempt hit a transient error must recover
    // inside that window. A one-minute gap loses a two-minute window entirely.
    let mut h = harness(&format!("{MON} 20:58:10"));
    h.rt.set_pre_start(Arc::new(SchedulePolicyHook { hold: true, skipped: Mutex::new(0) }));
    schedule(&mut h, DaysOfWeek::everyday(), "20:58", "21:00");

    h.rt.tick();
    let first = h.rt.status().active_occurrence.unwrap();
    assert_eq!(first.attempts, 1);
    assert!(first.retry_in_secs.unwrap() <= 5, "the first retry is seconds away: {first:?}");

    // Ticking again inside the delay must not attempt, and must not log again.
    h.rt.tick();
    h.rt.tick();
    assert_eq!(h.rt.status().active_occurrence.unwrap().attempts, 1);
}

#[test]
fn the_retry_recovers_the_window_rather_than_waiting_for_tomorrow() {
    // The whole point of §9: the cause clears, and the same window still
    // broadcasts. The next occurrence must never be tomorrow while today's
    // window is still open.
    let mut h = harness(&format!("{MON} 20:58:10"));
    let hook = Arc::new(RecordingHook::default());
    hook.fail(not_connected());
    h.rt.set_pre_start(Arc::clone(&hook) as Arc<dyn louver_core::runtime::PreStartHook>);
    schedule(&mut h, DaysOfWeek::everyday(), "20:58", "21:00");

    h.rt.tick();
    assert_eq!(h.launcher.launch_count(), 0);
    assert!(h.rt.status().next_scheduled_start.is_none(), "still today's window");

    // The transient failure clears.
    *hook.fail_with.lock().unwrap() = None;
    std::thread::sleep(std::time::Duration::from_millis(5100));
    h.rt.tick();

    assert!(h.rt.is_active(), "the window recovered inside itself");
    assert_eq!(h.rt.status().active_occurrence.unwrap().phase, "live");
}

#[test]
fn the_window_closing_stops_the_broadcast_and_leaves_nothing_running() {
    // §10: the end of the window ends the broadcast, and nothing is left over.
    let mut h = harness(&format!("{MON} 20:58:30"));
    schedule(&mut h, DaysOfWeek::everyday(), "20:58", "21:00");
    h.rt.tick();
    mark_connected(&mut h.rt);
    assert!(h.rt.is_active());

    h.clock.set_str(&format!("{MON} 21:00:01"));
    h.rt.tick();

    assert!(!h.rt.is_active(), "the window closed");
    assert!(h.rt.status().active_occurrence.is_none());
    assert_eq!(h.rt.ffmpeg_pid(), None, "no FFmpeg left behind");
    assert!(h.session_store.load().is_none(), "and no session file to resume from");
    assert_eq!(h.rt.status().next_scheduled_start.as_deref(), Some("2026-03-03 20:58"));
}

// --- saving a rule vs switching the machine on ------------------------------

use louver_core::scheduler::SchedulerState;

#[test]
fn a_saved_schedule_does_nothing_until_the_scheduler_is_started() {
    // TEST A. The reported ambiguity: a row on screen with a green toggle,
    // and no way to tell whether anything is actually watching the clock.
    // Nothing is, until the user says so.
    let mut h = harness(&format!("{MON} 20:58:30"));
    schedule_only(&h, DaysOfWeek::everyday(), "20:58", "21:00");

    for _ in 0..5 {
        h.rt.tick();
    }
    assert!(!h.rt.is_active(), "a saved rule is not a running scheduler");
    assert_eq!(h.launcher.launch_count(), 0);
    assert_eq!(h.rt.status().scheduler_state, SchedulerState::Stopped);
    // And the window is not claimed as active either — nothing is watching it.
    assert!(h.rt.status().active_occurrence.is_none());
}

#[test]
fn starting_the_scheduler_inside_a_window_broadcasts_at_once() {
    // TEST B, and the catch-up case together: pressing 예약 방송 시작 while a
    // window is already open starts it now rather than tomorrow.
    let mut h = harness(&format!("{MON} 20:58:30"));
    schedule_only(&h, DaysOfWeek::everyday(), "20:58", "21:00");
    h.rt.tick();
    assert_eq!(h.launcher.launch_count(), 0);

    h.rt.set_armed(true);
    h.rt.tick();
    assert!(h.rt.is_active());
    assert_eq!(h.rt.status().scheduler_state, SchedulerState::Live);
}

#[test]
fn stopping_the_scheduler_means_the_window_passes_untouched() {
    // TEST C. 예약 방송 중지 has to mean it, including for a window that opens
    // while the app is running.
    let mut h = harness(&format!("{MON} 20:57:00"));
    schedule(&mut h, DaysOfWeek::everyday(), "20:58", "21:00");
    assert_eq!(h.rt.status().scheduler_state, SchedulerState::Waiting);

    h.rt.set_armed(false);
    h.clock.set_str(&format!("{MON} 20:58:30"));
    for _ in 0..5 {
        h.rt.tick();
    }
    assert!(!h.rt.is_active(), "the user switched it off");
    assert_eq!(h.launcher.launch_count(), 0);
    assert_eq!(h.rt.status().scheduler_state, SchedulerState::Stopped);
}

#[test]
fn the_scheduler_comes_back_the_way_it_was_left() {
    // TEST D. Armed at shutdown, armed at launch.
    let h = harness(&format!("{MON} 12:00:00"));
    schedule_only(&h, DaysOfWeek::everyday(), "20:58", "21:00");
    let mut h = h;
    h.rt.set_armed(true);

    let mut again = harness_on(&h, &format!("{MON} 12:05:00"));
    again.rt.restore_armed_state();
    assert!(again.rt.is_armed(), "it was watching the clock, so it still is");
    assert_eq!(again.rt.status().scheduler_state, SchedulerState::Waiting);
}

#[test]
fn a_scheduler_the_user_stopped_stays_stopped_after_a_restart() {
    // The other half of §7, and the one that matters: a user who switched it
    // off must not find it running again after a reboot.
    let h = harness(&format!("{MON} 12:00:00"));
    schedule_only(&h, DaysOfWeek::everyday(), "20:58", "21:00");
    let mut h = h;
    h.rt.set_armed(true);
    h.rt.set_armed(false);

    let mut again = harness_on(&h, &format!("{MON} 12:05:00"));
    again.rt.restore_armed_state();
    assert!(!again.rt.is_armed());
    assert_eq!(again.rt.status().scheduler_state, SchedulerState::Stopped);
}

#[test]
fn restoring_can_be_switched_off_entirely() {
    let h = harness(&format!("{MON} 12:00:00"));
    let mut h = h;
    h.rt.set_armed(true);
    h.db.set_setting(louver_core::settings_keys::SCHEDULER_RESTORE_ON_LAUNCH, "false").unwrap();

    let mut again = harness_on(&h, &format!("{MON} 12:05:00"));
    again.rt.restore_armed_state();
    assert!(!again.rt.is_armed(), "the user asked for it not to come back");
}

#[test]
fn an_armed_scheduler_reports_waiting_then_live_then_waiting_again() {
    // TEST F, and §1's state machine end to end: the screen can say which of
    // these it is at every moment.
    let mut h = harness(&format!("{MON} 20:57:00"));
    schedule(&mut h, DaysOfWeek::everyday(), "20:58", "21:00");
    h.rt.tick();
    assert_eq!(h.rt.status().scheduler_state, SchedulerState::Waiting);

    h.clock.set_str(&format!("{MON} 20:58:10"));
    h.rt.tick();
    mark_connected(&mut h.rt);
    assert_eq!(h.rt.status().scheduler_state, SchedulerState::Live);

    h.clock.set_str(&format!("{MON} 21:00:01"));
    h.rt.tick();
    assert_eq!(h.rt.status().scheduler_state, SchedulerState::Waiting, "back to watching the clock");
    assert!(!h.rt.is_active());
    assert_eq!(h.rt.status().next_scheduled_start.as_deref(), Some("2026-03-03 20:58"));
}
