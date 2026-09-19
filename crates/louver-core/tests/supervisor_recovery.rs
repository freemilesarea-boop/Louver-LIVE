//! Crash, reconnect and recovery integration tests (§56, §32, §33).
//!
//! Unlike the unit tests in `supervisor.rs`, these drive real OS processes so
//! that spawning, killing, exit-code handling and orphan cleanup are exercised
//! for real.

mod common;

use common::*;
use louver_core::config::StreamMode;
use louver_core::session::{decide_recovery, RecoveryDecision, SessionState, SessionStore};
use louver_core::streaming::state::StreamState;
use louver_core::streaming::supervisor::{StreamSupervisor, SupervisorAction};
use louver_core::system::MetricsCollector;
use std::path::PathBuf;
use std::time::Duration;

/// An FFmpeg that will run for a long time, so the test can kill it.
fn long_running_args(out: &std::path::Path) -> Vec<String> {
    vec![
        "-hide_banner".into(), "-nostdin".into(), "-loglevel".into(), "error".into(),
        "-progress".into(), "pipe:1".into(), "-y".into(),
        "-f".into(), "lavfi".into(),
        "-i".into(), "testsrc2=size=320x240:rate=30".into(),
        "-f".into(), "lavfi".into(),
        "-i".into(), "sine=frequency=440:sample_rate=48000".into(),
        "-c:v".into(), "libx264".into(), "-preset".into(), "ultrafast".into(),
        "-pix_fmt".into(), "yuv420p".into(),
        "-c:a".into(), "aac".into(), "-t".into(), "600".into(),
        "-f".into(), "flv".into(),
        out.to_string_lossy().into_owned(),
    ]
}

/// FFmpeg that fails immediately, standing in for "YouTube refused us".
fn failing_args() -> Vec<String> {
    vec![
        "-hide_banner".into(), "-nostdin".into(), "-loglevel".into(), "error".into(),
        "-y".into(),
        "-i".into(), "/nonexistent/input/file.mp4".into(),
        "-f".into(), "null".into(), "-".into(),
    ]
}

fn wait_until(mut f: impl FnMut() -> bool, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn a_real_ffmpeg_is_spawned_reaches_live_and_reports_progress() {
    let tools = require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let mut sup = StreamSupervisor::new(StreamMode::StreamCopy);
    sup.begin().unwrap();

    let child = sup
        .spawn(&tools.ffmpeg, &long_running_args(&dir.path().join("o.flv")), |_| {})
        .expect("spawn failed");
    let pid = child.pid();
    sup.attach(child).unwrap();
    assert_eq!(sup.state(), StreamState::Connecting);

    assert!(
        wait_until(|| sup.has_produced_output(), Duration::from_secs(20)),
        "ffmpeg never reported progress"
    );
    sup.mark_live().unwrap();
    assert_eq!(sup.state(), StreamState::Live);
    assert_eq!(sup.poll(), SupervisorAction::Running);

    let p = sup.progress();
    assert!(p.frames > 0, "no frames reported: {p:?}");

    // The pid really is an ffmpeg, which is what orphan cleanup relies on (§33).
    let mut m = MetricsCollector::new();
    assert!(m.is_ffmpeg_process(pid.unwrap()), "spawned pid is not recognised as ffmpeg");

    sup.stop().unwrap();
    assert_eq!(sup.state(), StreamState::Stopped);
    assert!(
        wait_until(|| !MetricsCollector::new().is_ffmpeg_process(pid.unwrap()), Duration::from_secs(10)),
        "ffmpeg survived stop()"
    );
}

#[test]
fn killing_ffmpeg_triggers_reconnect_and_a_successful_respawn() {
    let tools = require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let mut sup = StreamSupervisor::new(StreamMode::StreamCopy);
    sup.begin().unwrap();

    let args = long_running_args(&dir.path().join("o.flv"));
    let child = sup.spawn(&tools.ffmpeg, &args, |_| {}).unwrap();
    let pid = child.pid().unwrap();
    sup.attach(child).unwrap();
    assert!(wait_until(|| sup.has_produced_output(), Duration::from_secs(20)));
    sup.mark_live().unwrap();

    // §59's "Simulate FFmpeg Crash": kill the process out from under us.
    let mut m = MetricsCollector::new();
    assert!(m.kill_if_ffmpeg(pid), "could not kill the ffmpeg we spawned");

    let action = wait_for_action(&mut sup, Duration::from_secs(15));
    match action {
        SupervisorAction::RestartAfter(d) => assert_eq!(d, Duration::from_secs(2), "first backoff is 2s"),
        other => panic!("a killed ffmpeg should schedule a restart, got {other:?}"),
    }
    assert_eq!(sup.state(), StreamState::Reconnecting);
    assert_eq!(sup.restart_count(), 1);
    assert!(sup.last_error().is_some());

    // Respawn, as the supervisor loop would after the backoff.
    sup.machine_mut().transition(StreamState::Connecting).unwrap();
    let child2 = sup.spawn(&tools.ffmpeg, &args, |_| {}).unwrap();
    let pid2 = child2.pid().unwrap();
    assert_ne!(pid2, pid);
    sup.attach(child2).unwrap();
    assert!(wait_until(|| sup.has_produced_output(), Duration::from_secs(20)));
    sup.mark_live().unwrap();
    sup.note_reconnect_success();

    assert_eq!(sup.state(), StreamState::Live, "broadcast did not recover");
    assert_eq!(sup.reconnect_count(), 0, "backoff should reset after a good reconnect");

    sup.stop().unwrap();
}

#[test]
fn an_ffmpeg_that_fails_to_start_is_retried_not_treated_as_a_clean_exit() {
    let tools = require_ffmpeg!();
    let mut sup = StreamSupervisor::new(StreamMode::StreamCopy);
    sup.begin().unwrap();

    let child = sup.spawn(&tools.ffmpeg, &failing_args(), |_| {}).unwrap();
    sup.attach(child).unwrap();

    match wait_for_action(&mut sup, Duration::from_secs(15)) {
        SupervisorAction::RestartAfter(_) => {}
        other => panic!("a failed connection should retry, got {other:?}"),
    }
    assert_eq!(sup.state(), StreamState::Reconnecting);
    let err = sup.last_error().expect("an error should be recorded for the UI");
    assert_eq!(err.code_str, "LL-STREAM-002");
}

#[test]
fn a_user_stop_never_respawns_even_though_the_exit_code_is_nonzero() {
    let tools = require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let mut sup = StreamSupervisor::new(StreamMode::StreamCopy);
    sup.begin().unwrap();

    let child = sup.spawn(&tools.ffmpeg, &long_running_args(&dir.path().join("o.flv")), |_| {}).unwrap();
    let pid = child.pid().unwrap();
    sup.attach(child).unwrap();
    assert!(wait_until(|| sup.has_produced_output(), Duration::from_secs(20)));
    sup.mark_live().unwrap();

    sup.stop().unwrap();
    assert_eq!(sup.state(), StreamState::Stopped);

    // Poll repeatedly: no restart may ever be scheduled (§17).
    for _ in 0..10 {
        match sup.poll() {
            SupervisorAction::RestartAfter(_) => panic!("user stop must never reconnect"),
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    assert_eq!(sup.restart_count(), 0);
    assert!(!MetricsCollector::new().is_ffmpeg_process(pid), "process left behind");
}

#[test]
fn ffmpeg_stderr_reaches_the_log_callback_with_secrets_masked() {
    let tools = require_ffmpeg!();
    let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = std::sync::Arc::clone(&captured);

    let mut sup = StreamSupervisor::new(StreamMode::StreamCopy);
    sup.begin().unwrap();
    // A publish URL carrying a stream key, which must never appear in the log.
    let mut args = failing_args();
    args.pop();
    args.pop();
    args.extend([
        "-f".to_string(), "flv".to_string(),
        "rtmps://a.rtmps.youtube.com/live2/abcd-efgh-ijkl-mnop".to_string(),
    ]);

    let child = sup
        .spawn(&tools.ffmpeg, &args, move |line| sink.lock().unwrap().push(line.to_string()))
        .unwrap();
    sup.attach(child).unwrap();
    wait_for_action(&mut sup, Duration::from_secs(20));
    std::thread::sleep(Duration::from_millis(300));

    let lines = captured.lock().unwrap().join("\n");
    assert!(!lines.is_empty(), "no output was captured at all");
    assert!(!lines.contains("abcd-efgh"), "STREAM KEY LEAKED INTO THE LOG:\n{lines}");
    assert!(lines.contains("••••"), "the spawn line should be present but masked:\n{lines}");
}

/// Poll until the supervisor stops reporting Running.
fn wait_for_action(sup: &mut StreamSupervisor, timeout: Duration) -> SupervisorAction {
    let start = std::time::Instant::now();
    loop {
        let a = sup.poll();
        if !matches!(a, SupervisorAction::Running) || start.elapsed() > timeout {
            return a;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ---------------------------------------------------------------------------
// Session recovery across a simulated power cut (§32, §33)
// ---------------------------------------------------------------------------

#[test]
fn a_power_cut_mid_broadcast_is_detected_and_resumed_inside_the_window() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().join("session.json"));

    // "Process 1" is broadcasting and heartbeats its state...
    let mut s = SessionState {
        session_id: 42,
        playlist_id: 7,
        started_at: "2026-03-02T20:00:00Z".parse().unwrap(),
        scheduled_end: Some("2026-03-03T08:00:00Z".parse().unwrap()),
        playback_mode: louver_core::PlaybackMode::ShuffleOnce,
        stream_state: StreamState::Live,
        stream_mode: StreamMode::StreamCopy,
        user_requested_stop: false,
        last_error: None,
        ffmpeg_pid: Some(999_999),
        order_seed: 8675309,
        heartbeat_at: "2026-03-02T23:59:00Z".parse().unwrap(),
        schedule_id: Some(1),
    };
    store.save(&s).unwrap();
    store.touch(&mut s, StreamState::Live, Some(999_999)).unwrap();

    // ...then the power goes out. "Process 2" starts and finds the file.
    let found = SessionStore::new(dir.path().join("session.json")).load().expect("state lost");
    assert_eq!(found.stream_state, StreamState::Live, "looks like a crash, as it should");

    match decide_recovery(Some(found), "2026-03-03T02:00:00Z".parse().unwrap(), true) {
        RecoveryDecision::Resume { session, reason } => {
            assert_eq!(session.playlist_id, 7);
            assert_eq!(session.order_seed, 8675309, "the shuffled order must be replayed exactly");
            assert_eq!(session.ffmpeg_pid, Some(999_999), "the orphan pid must be available");
            assert!(reason.contains("비정상 종료"));
        }
        d => panic!("expected Resume, got {d:?}"),
    }
}

#[test]
fn an_orphan_pid_that_is_not_ffmpeg_is_never_killed() {
    // §33: pids get reused, so identity is verified before anything is killed.
    let mut m = MetricsCollector::new();
    let me = std::process::id();
    assert!(!m.is_ffmpeg_process(me));
    assert!(!m.kill_if_ffmpeg(me), "the recovery path tried to kill this test process");
}

#[test]
fn a_clean_shutdown_leaves_nothing_to_recover() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().join("session.json"));
    let s = SessionState {
        session_id: 1,
        playlist_id: 1,
        started_at: "2026-03-02T20:00:00Z".parse().unwrap(),
        scheduled_end: None,
        playback_mode: louver_core::PlaybackMode::Sequential,
        stream_state: StreamState::Stopped,
        stream_mode: StreamMode::StreamCopy,
        user_requested_stop: true,
        last_error: None,
        ffmpeg_pid: None,
        order_seed: 1,
        heartbeat_at: "2026-03-02T21:00:00Z".parse().unwrap(),
        schedule_id: None,
    };
    store.save(&s).unwrap();
    assert_eq!(
        decide_recovery(store.load(), "2026-03-02T21:30:00Z".parse().unwrap(), true),
        RecoveryDecision::Nothing
    );
}

#[test]
fn the_state_file_is_never_left_half_written() {
    // Atomic rename means a reader either sees the old file or the new one.
    let dir = tempfile::tempdir().unwrap();
    let path: PathBuf = dir.path().join("session.json");
    let store = SessionStore::new(&path);
    let mut s = SessionState {
        session_id: 1,
        playlist_id: 1,
        started_at: "2026-03-02T20:00:00Z".parse().unwrap(),
        scheduled_end: None,
        playback_mode: louver_core::PlaybackMode::Sequential,
        stream_state: StreamState::Live,
        stream_mode: StreamMode::StreamCopy,
        user_requested_stop: false,
        last_error: None,
        ffmpeg_pid: Some(1),
        order_seed: 1,
        heartbeat_at: "2026-03-02T20:00:00Z".parse().unwrap(),
        schedule_id: None,
    };
    for i in 0..200 {
        store.touch(&mut s, StreamState::Live, Some(i)).unwrap();
        // Every observation must be a complete, parseable document.
        let read = SessionStore::new(&path).load();
        assert!(read.is_some(), "iteration {i}: state file was unreadable");
    }
    assert!(!path.with_extension("json.tmp").exists(), "temp file left behind");
}
