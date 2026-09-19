//! FFmpeg process supervision: health, exit handling and reconnect backoff (§17).
//!
//! The supervisor owns the child process and the restart policy. It is written
//! against a [`ProcessHandle`] trait so the reconnect tests can drive a fake
//! process without spawning FFmpeg (§56).

use crate::config::StreamMode;
use crate::error::{ErrorCode, LouverError, Result};
use crate::streaming::ffmpeg::{mask_argv, mask_secrets};
use crate::streaming::state::{StateMachine, StreamState};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Reconnect delays in seconds: 2, 5, 10, then 20, capped at 60 (§17).
pub fn reconnect_delay_secs(attempt: u32) -> u64 {
    match attempt {
        0 => 0,
        1 => 2,
        2 => 5,
        3 => 10,
        _ => 20u64.saturating_mul(1 + u64::from(attempt.saturating_sub(3)) / 4).min(60),
    }
}

/// A running FFmpeg, abstracted so tests can substitute a fake.
pub trait ProcessHandle: Send {
    /// `Some(success)` once the process has exited, `None` while it runs.
    fn try_exited(&mut self) -> Option<bool>;
    /// Ask the process to stop, then make sure it is gone.
    fn terminate(&mut self) -> Result<()>;
    fn pid(&self) -> Option<u32>;
}

/// Live progress scraped from `-progress pipe:1` (§28, §40).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamProgress {
    pub frames: u64,
    pub fps: f64,
    /// Output bitrate in kbit/s as reported by FFmpeg.
    pub bitrate_kbps: f64,
    /// Bytes pushed to the muxer so far.
    pub total_bytes: u64,
    /// Media time processed, in milliseconds.
    pub out_time_ms: u64,
    pub speed: f64,
}

/// Parse one `key=value` line of FFmpeg's `-progress` output.
pub fn apply_progress_line(p: &mut StreamProgress, line: &str) -> bool {
    let Some((k, v)) = line.split_once('=') else { return false };
    let (k, v) = (k.trim(), v.trim());
    match k {
        "frame" => p.frames = v.parse().unwrap_or(p.frames),
        "fps" => p.fps = v.parse().unwrap_or(p.fps),
        "bitrate" => {
            // e.g. "10216.3kbits/s" or "N/A"
            p.bitrate_kbps = v
                .trim_end_matches("bits/s")
                .trim_end_matches('k')
                .parse()
                .unwrap_or(p.bitrate_kbps);
        }
        "total_size" => p.total_bytes = v.parse().unwrap_or(p.total_bytes),
        "out_time_ms" => p.out_time_ms = v.parse::<u64>().map(|us| us / 1000).unwrap_or(p.out_time_ms),
        "speed" => p.speed = v.trim_end_matches('x').parse().unwrap_or(p.speed),
        "progress" => return v == "end",
        _ => {}
    }
    false
}

/// Everything the dashboard shows about the running session (§28).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisorStatus {
    pub state: StreamState,
    pub mode: StreamMode,
    pub pid: Option<u32>,
    pub restart_count: u32,
    pub reconnect_count: u32,
    /// Seconds since FFmpeg last reported progress; large values mean a stall.
    pub seconds_since_data: Option<u64>,
    pub progress: StreamProgress,
    pub last_error: Option<LouverError>,
    pub next_retry_in_secs: Option<u64>,
}

/// Spawns and watches the live FFmpeg process.
pub struct StreamSupervisor {
    machine: StateMachine,
    mode: StreamMode,
    child: Option<Box<dyn ProcessHandle>>,
    restart_count: u32,
    reconnect_count: u32,
    last_data_at: Option<Instant>,
    progress: Arc<Mutex<StreamProgress>>,
    last_error: Option<LouverError>,
    /// Tail of FFmpeg stderr, already masked, for the error disclosure (§35).
    stderr_tail: Arc<Mutex<Vec<String>>>,
    saw_data: Arc<AtomicBool>,
    data_epoch_ms: Arc<AtomicU64>,
    /// Consider the stream stalled after this long without progress.
    stall_timeout: Duration,
}

impl StreamSupervisor {
    pub fn new(mode: StreamMode) -> Self {
        Self {
            machine: StateMachine::new(),
            mode,
            child: None,
            restart_count: 0,
            reconnect_count: 0,
            last_data_at: None,
            progress: Arc::new(Mutex::new(StreamProgress::default())),
            last_error: None,
            stderr_tail: Arc::new(Mutex::new(Vec::new())),
            saw_data: Arc::new(AtomicBool::new(false)),
            data_epoch_ms: Arc::new(AtomicU64::new(0)),
            stall_timeout: Duration::from_secs(30),
        }
    }

    pub fn with_stall_timeout(mut self, d: Duration) -> Self {
        self.stall_timeout = d;
        self
    }

    pub fn state(&self) -> StreamState {
        self.machine.state()
    }

    pub fn machine_mut(&mut self) -> &mut StateMachine {
        &mut self.machine
    }

    pub fn mode(&self) -> StreamMode {
        self.mode
    }

    pub fn set_mode(&mut self, m: StreamMode) {
        self.mode = m;
    }

    pub fn restart_count(&self) -> u32 {
        self.restart_count
    }

    pub fn reconnect_count(&self) -> u32 {
        self.reconnect_count
    }

    pub fn progress(&self) -> StreamProgress {
        self.progress.lock().unwrap().clone()
    }

    pub fn stderr_tail(&self) -> Vec<String> {
        self.stderr_tail.lock().unwrap().clone()
    }

    pub fn last_error(&self) -> Option<LouverError> {
        self.last_error.clone()
    }

    pub fn status(&self) -> SupervisorStatus {
        SupervisorStatus {
            state: self.machine.state(),
            mode: self.mode,
            pid: self.child.as_ref().and_then(|c| c.pid()),
            restart_count: self.restart_count,
            reconnect_count: self.reconnect_count,
            seconds_since_data: self.last_data_at.map(|t| t.elapsed().as_secs()),
            progress: self.progress(),
            last_error: self.last_error.clone(),
            next_retry_in_secs: (self.machine.state() == StreamState::Reconnecting)
                .then(|| reconnect_delay_secs(self.reconnect_count)),
        }
    }

    /// Begin a session. Call before the first [`Self::attach`].
    pub fn begin(&mut self) -> Result<()> {
        self.machine.begin()?;
        self.restart_count = 0;
        self.reconnect_count = 0;
        self.last_error = None;
        self.stderr_tail.lock().unwrap().clear();
        *self.progress.lock().unwrap() = StreamProgress::default();
        Ok(())
    }

    /// Attach a freshly started process and move to CONNECTING.
    pub fn attach(&mut self, child: Box<dyn ProcessHandle>) -> Result<()> {
        self.child = Some(child);
        self.last_data_at = Some(Instant::now());
        self.saw_data.store(false, Ordering::SeqCst);
        if self.machine.state() != StreamState::Connecting {
            self.machine.transition(StreamState::Connecting)?;
        }
        Ok(())
    }

    /// Called when FFmpeg has produced output, i.e. YouTube accepted the stream.
    pub fn mark_live(&mut self) -> Result<()> {
        self.last_data_at = Some(Instant::now());
        if self.machine.state() != StreamState::Live {
            self.machine.transition(StreamState::Live)?;
        }
        Ok(())
    }

    pub fn note_data(&mut self) {
        self.last_data_at = Some(Instant::now());
    }

    /// True when FFmpeg is alive but has stopped producing output.
    pub fn is_stalled(&self) -> bool {
        self.machine.state() == StreamState::Live
            && self
                .last_data_at
                .map(|t| t.elapsed() > self.stall_timeout)
                .unwrap_or(false)
    }

    /// The user pressed Stop. Terminates FFmpeg and blocks all reconnection (§17).
    pub fn stop(&mut self) -> Result<()> {
        self.machine.request_user_stop();
        if self.machine.state() != StreamState::Stopping && !self.machine.state().is_terminal() {
            self.machine.transition(StreamState::Stopping)?;
        }
        if let Some(mut c) = self.child.take() {
            c.terminate()?;
        }
        if self.machine.state() != StreamState::Stopped {
            self.machine.transition(StreamState::Stopped)?;
        }
        Ok(())
    }

    /// What the supervisor loop should do next.
    #[must_use]
    pub fn poll(&mut self) -> SupervisorAction {
        // A stalled-but-alive process is killed so the restart path can run.
        if self.is_stalled() {
            if let Some(mut c) = self.child.take() {
                let _ = c.terminate();
            }
            self.last_error = Some(LouverError::with_detail(
                ErrorCode::StreamFfmpegExit,
                format!("no output for {}s", self.stall_timeout.as_secs()),
            ));
            let _ = self.machine.transition(StreamState::Reconnecting);
            self.reconnect_count += 1;
            return SupervisorAction::RestartAfter(Duration::from_secs(reconnect_delay_secs(
                self.reconnect_count,
            )));
        }

        let Some(child) = self.child.as_mut() else {
            return SupervisorAction::Idle;
        };
        let Some(ok) = child.try_exited() else {
            return SupervisorAction::Running;
        };

        self.child = None;
        let next = match self.machine.on_process_exit(ok) {
            Ok(s) => s,
            Err(e) => {
                self.last_error = Some(e);
                return SupervisorAction::Failed;
            }
        };

        match next {
            StreamState::Stopped => SupervisorAction::Stopped,
            StreamState::Reconnecting => {
                self.restart_count += 1;
                self.reconnect_count += 1;
                if self.last_error.is_none() {
                    let tail = self.stderr_tail();
                    self.last_error = Some(LouverError::with_detail(
                        ErrorCode::StreamFfmpegExit,
                        tail.last().cloned().unwrap_or_else(|| "ffmpeg exited".into()),
                    ));
                }
                SupervisorAction::RestartAfter(Duration::from_secs(reconnect_delay_secs(
                    self.reconnect_count,
                )))
            }
            _ => SupervisorAction::Running,
        }
    }

    /// A restart succeeded; clear the backoff so the next fault starts at 2s.
    pub fn note_reconnect_success(&mut self) {
        self.reconnect_count = 0;
        self.last_error = None;
    }

    /// Spawn FFmpeg and wire up its stdout/stderr readers.
    ///
    /// `args` is an argv vector; no shell is used. The command line is logged
    /// only in masked form.
    pub fn spawn(
        &mut self,
        program: &PathBuf,
        args: &[String],
        mut on_log: impl FnMut(&str) + Send + 'static,
    ) -> Result<Box<dyn ProcessHandle>> {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        on_log(&format!("spawn: {} {}", program.display(), mask_argv(args).join(" ")));

        let mut child = cmd
            .spawn()
            .map_err(|e| LouverError::with_detail(ErrorCode::StreamFfmpegSpawn, e.to_string()))?;

        let progress = Arc::clone(&self.progress);
        let saw_data = Arc::clone(&self.saw_data);
        let epoch = Arc::clone(&self.data_epoch_ms);
        if let Some(out) = child.stdout.take() {
            std::thread::spawn(move || {
                for line in BufReader::new(out).lines().map_while(std::result::Result::ok) {
                    let mut p = progress.lock().unwrap();
                    apply_progress_line(&mut p, &line);
                    if p.total_bytes > 0 || p.frames > 0 {
                        saw_data.store(true, Ordering::SeqCst);
                        epoch.store(now_ms(), Ordering::SeqCst);
                    }
                }
            });
        }

        let tail = Arc::clone(&self.stderr_tail);
        if let Some(err) = child.stderr.take() {
            std::thread::spawn(move || {
                for line in BufReader::new(err).lines().map_while(std::result::Result::ok) {
                    let masked = mask_secrets(&line);
                    on_log(&masked);
                    let mut t = tail.lock().unwrap();
                    t.push(masked);
                    if t.len() > 50 {
                        t.remove(0);
                    }
                }
            });
        }

        Ok(Box::new(OsProcess { child: Some(child) }))
    }

    /// True once FFmpeg has actually pushed bytes, which is how the session
    /// knows CONNECTING became LIVE.
    pub fn has_produced_output(&self) -> bool {
        self.saw_data.load(Ordering::SeqCst)
    }

    /// Wall-clock ms of the last observed output, 0 if none yet.
    pub fn last_output_epoch_ms(&self) -> u64 {
        self.data_epoch_ms.load(Ordering::SeqCst)
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorAction {
    /// Nothing attached.
    Idle,
    /// Process healthy.
    Running,
    /// Session finished at the user's request.
    Stopped,
    /// Respawn FFmpeg after this delay.
    RestartAfter(Duration),
    /// Unrecoverable.
    Failed,
}

/// A real OS child process.
pub struct OsProcess {
    child: Option<Child>,
}

impl ProcessHandle for OsProcess {
    fn try_exited(&mut self) -> Option<bool> {
        let c = self.child.as_mut()?;
        match c.try_wait() {
            Ok(Some(status)) => Some(status.success()),
            Ok(None) => None,
            Err(_) => Some(false),
        }
    }

    fn terminate(&mut self) -> Result<()> {
        if let Some(mut c) = self.child.take() {
            // FFmpeg flushes the FLV trailer on SIGTERM; on Windows `kill` is
            // the only option Rust exposes.
            #[cfg(unix)]
            {
                use std::io::Write;
                if let Some(mut stdin) = c.stdin.take() {
                    let _ = stdin.write_all(b"q\n");
                }
                unsafe {
                    libc_kill(c.id() as i32, 15);
                }
                for _ in 0..30 {
                    if let Ok(Some(_)) = c.try_wait() {
                        return Ok(());
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            let _ = c.kill();
            let _ = c.wait();
        }
        Ok(())
    }

    fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(|c| c.id())
    }
}

#[cfg(unix)]
extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scriptable stand-in for FFmpeg (§56).
    struct FakeProcess {
        exits_with: Option<bool>,
        terminated: Arc<AtomicBool>,
        pid: u32,
    }

    impl FakeProcess {
        fn running(pid: u32) -> (Box<dyn ProcessHandle>, Arc<AtomicBool>) {
            let t = Arc::new(AtomicBool::new(false));
            (Box::new(FakeProcess { exits_with: None, terminated: Arc::clone(&t), pid }), t)
        }
        fn crashed() -> Box<dyn ProcessHandle> {
            Box::new(FakeProcess {
                exits_with: Some(false),
                terminated: Arc::new(AtomicBool::new(false)),
                pid: 1,
            })
        }
    }

    impl ProcessHandle for FakeProcess {
        fn try_exited(&mut self) -> Option<bool> {
            if self.terminated.load(Ordering::SeqCst) {
                return Some(false);
            }
            self.exits_with
        }
        fn terminate(&mut self) -> Result<()> {
            self.terminated.store(true, Ordering::SeqCst);
            Ok(())
        }
        fn pid(&self) -> Option<u32> {
            Some(self.pid)
        }
    }

    fn live_supervisor() -> StreamSupervisor {
        let mut s = StreamSupervisor::new(StreamMode::StreamCopy);
        s.begin().unwrap();
        let (p, _) = FakeProcess::running(4242);
        s.attach(p).unwrap();
        s.mark_live().unwrap();
        s
    }

    // --- backoff schedule (§17) -------------------------------------------

    #[test]
    fn backoff_follows_the_specified_schedule() {
        assert_eq!(reconnect_delay_secs(1), 2);
        assert_eq!(reconnect_delay_secs(2), 5);
        assert_eq!(reconnect_delay_secs(3), 10);
        assert_eq!(reconnect_delay_secs(4), 20);
        assert_eq!(reconnect_delay_secs(6), 20);
    }

    #[test]
    fn backoff_is_monotonic_and_capped_at_60s() {
        let mut prev = 0;
        for a in 1..200u32 {
            let d = reconnect_delay_secs(a);
            assert!(d >= prev, "attempt {a} went backwards");
            assert!(d <= 60, "attempt {a} exceeded the 60s cap: {d}");
            prev = d;
        }
        assert_eq!(reconnect_delay_secs(199), 60);
    }

    // --- crash / reconnect (§56) ------------------------------------------

    #[test]
    fn crashed_process_triggers_restart_with_backoff() {
        let mut s = live_supervisor();
        s.child = Some(FakeProcess::crashed());
        match s.poll() {
            SupervisorAction::RestartAfter(d) => assert_eq!(d, Duration::from_secs(2)),
            other => panic!("expected restart, got {other:?}"),
        }
        assert_eq!(s.state(), StreamState::Reconnecting);
        assert_eq!(s.restart_count(), 1);
        assert!(s.last_error().is_some(), "a crash must record an error for the UI");
    }

    #[test]
    fn repeated_crashes_escalate_the_delay() {
        let mut s = live_supervisor();
        let expected = [2u64, 5, 10, 20];
        for want in expected {
            s.child = Some(FakeProcess::crashed());
            match s.poll() {
                SupervisorAction::RestartAfter(d) => assert_eq!(d.as_secs(), want),
                other => panic!("expected restart, got {other:?}"),
            }
            // supervisor would respawn here
            let (p, _) = FakeProcess::running(1);
            s.machine_mut().transition(StreamState::Connecting).unwrap();
            s.attach(p).unwrap();
            s.mark_live().unwrap();
        }
        assert_eq!(s.restart_count(), 4);
    }

    #[test]
    fn a_successful_reconnect_resets_the_backoff() {
        let mut s = live_supervisor();
        s.child = Some(FakeProcess::crashed());
        s.poll();
        s.child = Some(FakeProcess::crashed());
        s.poll();
        assert_eq!(s.reconnect_count(), 2);

        s.machine_mut().transition(StreamState::Connecting).unwrap();
        let (p, _) = FakeProcess::running(9);
        s.attach(p).unwrap();
        s.mark_live().unwrap();
        s.note_reconnect_success();
        assert_eq!(s.reconnect_count(), 0);

        s.child = Some(FakeProcess::crashed());
        match s.poll() {
            SupervisorAction::RestartAfter(d) => assert_eq!(d.as_secs(), 2, "backoff must restart at 2s"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn user_stop_terminates_the_child_and_never_restarts() {
        let mut s = StreamSupervisor::new(StreamMode::StreamCopy);
        s.begin().unwrap();
        let (p, terminated) = FakeProcess::running(77);
        s.attach(p).unwrap();
        s.mark_live().unwrap();

        s.stop().unwrap();
        assert!(terminated.load(Ordering::SeqCst), "child was not terminated");
        assert_eq!(s.state(), StreamState::Stopped);

        // Even if a stale exit event arrives afterwards, no restart happens.
        s.child = Some(FakeProcess::crashed());
        assert_eq!(s.poll(), SupervisorAction::Stopped);
        assert_eq!(s.restart_count(), 0);
    }

    #[test]
    fn healthy_process_reports_running_and_no_retry() {
        let mut s = live_supervisor();
        assert_eq!(s.poll(), SupervisorAction::Running);
        let st = s.status();
        assert_eq!(st.state, StreamState::Live);
        assert_eq!(st.pid, Some(4242));
        assert!(st.next_retry_in_secs.is_none());
        assert_eq!(st.mode, StreamMode::StreamCopy);
    }

    #[test]
    fn detached_supervisor_is_idle() {
        let mut s = StreamSupervisor::new(StreamMode::StreamCopy);
        assert_eq!(s.poll(), SupervisorAction::Idle);
    }

    // --- stall detection (§17: "마지막 데이터 전송 시각") --------------------

    #[test]
    fn a_live_process_that_stops_producing_output_is_restarted() {
        let mut s = StreamSupervisor::new(StreamMode::StreamCopy)
            .with_stall_timeout(Duration::from_millis(50));
        s.begin().unwrap();
        let (p, terminated) = FakeProcess::running(5);
        s.attach(p).unwrap();
        s.mark_live().unwrap();
        assert_eq!(s.poll(), SupervisorAction::Running);

        std::thread::sleep(Duration::from_millis(80));
        assert!(s.is_stalled());
        match s.poll() {
            SupervisorAction::RestartAfter(d) => assert_eq!(d.as_secs(), 2),
            other => panic!("stalled process was not restarted: {other:?}"),
        }
        assert!(terminated.load(Ordering::SeqCst), "stalled child must be killed");
        assert_eq!(s.state(), StreamState::Reconnecting);
    }

    #[test]
    fn note_data_keeps_a_busy_stream_from_being_declared_stalled() {
        let mut s = StreamSupervisor::new(StreamMode::StreamCopy)
            .with_stall_timeout(Duration::from_millis(80));
        s.begin().unwrap();
        let (p, _) = FakeProcess::running(5);
        s.attach(p).unwrap();
        s.mark_live().unwrap();
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(30));
            s.note_data();
            assert!(!s.is_stalled());
        }
    }

    // --- progress parsing (§28, §40) --------------------------------------

    #[test]
    fn progress_lines_are_parsed() {
        let mut p = StreamProgress::default();
        for line in [
            "frame=1800", "fps=30.0", "bitrate=10216.3kbits/s",
            "total_size=1234567", "out_time_ms=60000000", "speed=1.00x",
            "progress=continue",
        ] {
            assert!(!apply_progress_line(&mut p, line));
        }
        assert_eq!(p.frames, 1800);
        assert_eq!(p.fps, 30.0);
        assert!((p.bitrate_kbps - 10216.3).abs() < 0.01);
        assert_eq!(p.total_bytes, 1_234_567);
        assert_eq!(p.out_time_ms, 60_000); // µs -> ms
        assert_eq!(p.speed, 1.0);
        assert!(apply_progress_line(&mut p, "progress=end"));
    }

    #[test]
    fn progress_tolerates_na_and_garbage_without_losing_state() {
        let mut p = StreamProgress { frames: 10, bitrate_kbps: 5.0, ..Default::default() };
        apply_progress_line(&mut p, "bitrate=N/A");
        apply_progress_line(&mut p, "frame=");
        apply_progress_line(&mut p, "no-equals-sign");
        apply_progress_line(&mut p, "speed=N/A");
        assert_eq!(p.frames, 10, "a bad line must not zero the counter");
        assert_eq!(p.bitrate_kbps, 5.0);
    }

    #[test]
    fn status_reports_the_pending_retry_while_reconnecting() {
        let mut s = live_supervisor();
        s.child = Some(FakeProcess::crashed());
        s.poll();
        assert_eq!(s.status().next_retry_in_secs, Some(2));
        assert_eq!(s.status().state, StreamState::Reconnecting);
    }
}
