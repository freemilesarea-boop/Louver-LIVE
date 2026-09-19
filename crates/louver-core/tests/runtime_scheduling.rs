//! End-to-end runtime tests (§57, §64).
//!
//! These drive the *whole* broadcast loop — scheduler, supervisor, recovery —
//! on a virtual clock and a fake process, so the §64 success scenario is
//! verified in seconds rather than over a day.

use louver_core::clock::{Clock, TestClock};
use louver_core::config::{OutputProfile, StreamMode};
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
    let playlist_id = db
        .create_playlist("Night Jazz", PlaybackMode::Sequential, OutputProfile::P1080p30)
        .unwrap();
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

fn schedule(h: &Harness, days: DaysOfWeek, start: &str, end: &str) {
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
    })
    .unwrap();

    let args = h.launcher.last_args().join(" ");
    assert!(args.contains("abcd-efgh-ijkl-mnop"), "ffmpeg must actually receive the key");
    assert!(args.contains("-stream_loop -1"), "the broadcast must loop forever");
    assert!(args.contains("-c copy"), "the live path must be a pure remux (§41)");

    // ...but nothing that reaches the log or the database contains it.
    assert!(!h.events.logs().contains("abcd-efgh"), "key leaked to the event sink");
    let stored = h.db.recent_events(100).unwrap();
    assert!(
        !stored.iter().any(|e| e.message.contains("abcd-efgh")),
        "key leaked into stream_events"
    );
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
    schedule(&h, DaysOfWeek::everyday(), "20:00", "08:00");

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
    schedule(&h, DaysOfWeek::weekdays(), "09:00", "18:00");
    h.rt.tick();
    assert!(!h.rt.is_active(), "a weekday schedule started on Saturday");
}

#[test]
fn the_scheduler_does_not_stop_a_manual_broadcast() {
    let mut h = harness(&format!("{MON} 12:00:00"));
    schedule(&h, DaysOfWeek::everyday(), "20:00", "22:00");

    h.rt.start(StartOptions {
        playlist_id: h.playlist_id,
        reason: StartReason::Manual,
        dry_run: false,
        scheduled_end: None,
        occurrence: None,
        order_seed: Some(1),
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
    schedule(&h, DaysOfWeek::everyday(), "20:00", "23:00");

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
    schedule(&h, DaysOfWeek::everyday(), "20:00", "23:00");
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
    schedule(&h, DaysOfWeek::everyday(), "20:00", "23:00");
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
    schedule(&h, DaysOfWeek::weekdays(), "09:00", "18:00");

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
    schedule(&h, DaysOfWeek::weekdays(), "09:00", "18:00");
    h.rt.recover_on_startup();
    assert!(!h.rt.is_active());
    assert_eq!(h.launcher.launch_count(), 0);
}

#[test]
fn a_power_cut_mid_broadcast_resumes_with_the_same_play_order() {
    let mut h = harness(&format!("{MON} 20:00:00"));
    schedule(&h, DaysOfWeek::everyday(), "20:00", "08:00");

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
    // Same database and state file as the crashed process.
    std::fs::copy(h.session_store.path(), h2.session_store.path()).unwrap();
    let h2_playlist = h2.playlist_id;
    h2.db.update_playlist(h2_playlist, "Night Jazz", PlaybackMode::ShuffleOnce, OutputProfile::P1080p30).unwrap();
    h2.db.create_schedule(&Schedule {
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
    let h = harness(&format!("{MON} 20:00:00"));
    schedule(&h, DaysOfWeek::everyday(), "20:00", "23:00");
    let mut h = h;
    h.rt.tick();
    mark_connected(&mut h.rt);
    h.rt.tick();
    assert!(h.session_store.load().is_some());
    drop(h.rt);

    // Restart the next afternoon, long after the window closed.
    let mut h2 = harness(&format!("{TUE} 14:00:00"));
    std::fs::copy(h.session_store.path(), h2.session_store.path()).unwrap();
    let pid = h2.playlist_id;
    h2.db.create_schedule(&Schedule {
        id: 0, playlist_id: pid, days_of_week: DaysOfWeek::everyday(),
        start_time: "20:00".into(), end_time: "23:00".into(), enabled: true,
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
    let err = h
        .rt
        .start(StartOptions {
            playlist_id: h.playlist_id,
            reason: StartReason::Manual,
            dry_run: false,
            scheduled_end: None,
            occurrence: None,
            order_seed: Some(1),
        })
        .unwrap_err();
    assert_eq!(err.code_str, "LL-STREAM-001");
    assert_eq!(h.rt.state(), StreamState::Error);
    assert!(h.events.logs().contains("LL-STREAM-001"));
}
