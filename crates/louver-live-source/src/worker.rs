//! One broadcast, one FFmpeg, one owner.
//!
//! ## The isolation this module is built around
//!
//! The worker holds a [`std::process::Child`] and that is the **only** handle
//! it has on any process. It never reads a pid file, never walks `/proc`, never
//! calls `pkill`, and never sends a signal to anything it did not spawn. So the
//! strongest thing that can go wrong here — a watchdog that fires wrongly, a
//! restart loop, a panic — costs this worker's own FFmpeg and nothing else.
//! `tests/isolation.rs` asserts that property against the source of this crate,
//! because it is the one guarantee a reviewer cannot check by reading a diff.
//!
//! It also holds no database. State goes to this worker's own JSON file
//! ([`crate::state`]), so there is no connection, no mutex and no row shared
//! with the broadcasts running today.
//!
//! ## Why the source is resolved again on every attempt
//!
//! YouTube's manifest URLs are time-limited and carry a signature. A worker
//! that resolved once and restarted with the stored URL would work for a few
//! hours and then fail in a way that looks like a network fault. So each
//! attempt resolves from scratch, and the resolved URL is held in memory for
//! exactly as long as the FFmpeg that uses it.
//!
//! That same signature is why **the resolved URL is never logged and never
//! written to the state file**. It is closer to a credential than to an
//! address. The log line carries the video id, which is in every share link.

use crate::args::capped_live_args;
use crate::error::{LiveSourceError, Result};
use crate::resolver::{classify, LiveSourceResolver, SourceKind};
use crate::state::{Phase, StateStore, WorkerState};
use crate::watchdog::{FrameWatchdog, Verdict};
use louver_core::streaming::ffmpeg::{FfmpegCommandBuilder, FfmpegTools};
use louver_core::OutputProfile;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How many times a fault may be retried before the worker gives up.
///
/// Bounded on purpose. An unbounded reconnect against a broadcast that has
/// ended is a machine spinning for ever on a stream that is never coming back.
pub const DEFAULT_MAX_RESTARTS: u32 = 5;

/// The backoff schedule, in seconds, reused from the shape production already
/// uses: double each time, capped, so a flapping source is not hammered.
fn backoff_secs(attempt: u32) -> u64 {
    match attempt {
        0 => 2,
        1 => 4,
        2 => 8,
        3 => 16,
        4 => 32,
        _ => 60,
    }
}

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub worker_id: String,
    /// What the user typed: a YouTube watch URL, or a direct stream URL.
    pub source: String,
    /// The concat manifest the sound comes from.
    pub manifest_path: PathBuf,
    /// Where the output goes. Held here and nowhere else — not in the state
    /// file, not in a log line.
    pub destination: String,
    pub state_dir: PathBuf,
    pub tools: FfmpegTools,
    pub profile: OutputProfile,
    pub max_restarts: u32,
    pub stall_after: Duration,
    pub grace: Duration,
    /// Stop after this long. `None` runs until stopped or until it gives up;
    /// the tests and the CLI's `--run-for` use it to bound a run.
    pub run_for: Option<Duration>,
}

impl WorkerConfig {
    pub fn new(
        worker_id: impl Into<String>,
        source: impl Into<String>,
        manifest_path: impl Into<PathBuf>,
        destination: impl Into<String>,
        state_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            worker_id: worker_id.into(),
            source: source.into(),
            manifest_path: manifest_path.into(),
            destination: destination.into(),
            state_dir: state_dir.into(),
            tools: FfmpegTools::new("ffmpeg", "ffprobe"),
            profile: OutputProfile::P1080p30,
            max_restarts: DEFAULT_MAX_RESTARTS,
            stall_after: crate::watchdog::DEFAULT_STALL_AFTER,
            grace: crate::watchdog::DEFAULT_GRACE,
            run_for: None,
        }
    }
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// `run_for` elapsed, or a stop was asked for, while sending.
    Stopped { frames: u64, restarts: u32 },
    /// FFmpeg ended by itself and no restart was due.
    SourceEnded { frames: u64, restarts: u32 },
    /// Out of attempts.
    GaveUp { restarts: u32, reason: String },
}

/// Why one attempt ended. Internal, but it decides whether to retry.
#[derive(Debug)]
enum AttemptEnd {
    /// The watchdog fired. Retry.
    Fault(Verdict),
    /// FFmpeg exited. Retry — a live source that drops the connection exits.
    Exited { code: Option<i32> },
    /// A stop was asked for, or the run clock ran out. Do not retry.
    Stopped,
}

pub struct LiveWorker {
    cfg: WorkerConfig,
    resolver: Arc<dyn LiveSourceResolver>,
    store: StateStore,
    state: WorkerState,
    stop: Arc<AtomicBool>,
}

impl LiveWorker {
    pub fn new(cfg: WorkerConfig, resolver: Arc<dyn LiveSourceResolver>) -> Self {
        let store = StateStore::new(&cfg.state_dir, &cfg.worker_id);
        let mut state = WorkerState::new(&cfg.worker_id);
        state.video_id = crate::resolver::video_id(&cfg.source);
        Self { cfg, resolver, store, state, stop: Arc::new(AtomicBool::new(false)) }
    }

    /// A handle a caller can set to ask this worker to wind down.
    pub fn stop_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }

    pub fn state_path(&self) -> &std::path::Path {
        self.store.path()
    }

    fn say(&self, line: &str) {
        // The destination and the resolved manifest URL are not parameters to
        // this function and are not in scope at any call site.
        println!("[louver][live-source][{}] {line}", self.cfg.worker_id);
    }

    fn persist(&mut self, phase: Phase) {
        self.state.phase = phase;
        let _ = self.store.save(&self.state);
    }

    /// Resolve the source for this attempt.
    ///
    /// A direct stream URL goes through production's own validator, which is
    /// the same check the existing CCTV path makes. A YouTube watch URL goes to
    /// the resolver, which validates what it gets back.
    fn resolve_now(&self) -> Result<String> {
        match classify(&self.cfg.source)? {
            SourceKind::YoutubeWatch => {
                let r = self.resolver.resolve(&self.cfg.source)?;
                if !r.is_live {
                    return Err(LiveSourceError::not_live("라이브 방송이 아닙니다."));
                }
                // The id comes from the address the user typed, which is a
                // share link. The resolved manifest URL carries a signature and
                // is deliberately absent from this line.
                self.say(&format!(
                    "소스 확인 — video={} live=YES source_size={}",
                    self.state.video_id.as_deref().unwrap_or("-"),
                    match (r.width, r.height) {
                        (Some(w), Some(h)) => format!("{w}x{h}"),
                        _ => "unknown".into(),
                    }
                ));
                Ok(r.manifest_url)
            }
            SourceKind::DirectStream => {
                let checked = louver_cloud::cctv::validate(&self.cfg.source)
                    .map_err(|e| LiveSourceError::invalid(format!("주소를 사용할 수 없습니다: {e}")))?;
                Ok(checked.into_string())
            }
        }
    }

    /// Run until stopped, until the source ends for good, or until out of
    /// attempts.
    pub fn run(&mut self) -> Result<Outcome> {
        let deadline = self.cfg.run_for.map(|d| Instant::now() + d);
        let mut restarts: u32 = 0;
        let mut total_frames: u64 = 0;

        loop {
            if self.should_stop(deadline) {
                self.persist(Phase::Stopped);
                return Ok(Outcome::Stopped { frames: total_frames, restarts });
            }

            self.persist(Phase::Resolving);
            let manifest_url = match self.resolve_now() {
                Ok(u) => u,
                Err(e) => {
                    // A source that cannot be resolved is not a transport
                    // fault. "Not live" and "private" will not fix themselves
                    // on a retry, so these end the worker rather than loop.
                    self.state.last_error = Some(e.message.clone());
                    self.persist(Phase::GaveUp);
                    self.say(&format!("소스 해석 실패 [{}] — {}", e.kind.as_str(), e.message));
                    return Err(e);
                }
            };

            self.persist(Phase::Starting);
            let end = self.one_attempt(&manifest_url, deadline, &mut total_frames)?;
            if matches!(end, AttemptEnd::Stopped) {
                self.persist(Phase::Stopped);
                return Ok(Outcome::Stopped { frames: total_frames, restarts });
            }

            // Both remaining ends are retryable: a live source that goes away
            // either freezes the picture (the watchdog) or drops the connection
            // (FFmpeg exits). The attempt count is what bounds either of them.
            if restarts >= self.cfg.max_restarts {
                let reason = match &end {
                    AttemptEnd::Fault(v) => format!("영상이 멈춘 상태가 반복됩니다 ({})", v_token(v)),
                    _ => "영상 소스 연결이 반복해서 끊어집니다".to_string(),
                };
                self.state.last_error = Some(reason.clone());
                self.persist(Phase::GaveUp);
                self.say(&format!("재시도 {restarts}회 후 중단 — {reason}"));
                return Ok(Outcome::GaveUp { restarts, reason });
            }

            let wait = backoff_secs(restarts);
            restarts += 1;
            self.state.restarts = restarts;
            match &end {
                AttemptEnd::Fault(v) => {
                    self.state.last_verdict = v.as_str().to_string();
                    self.state.last_error = Some(format!("영상 정지 감지 ({})", v_token(v)));
                    self.say(&format!(
                        "영상 정지 감지 [{}] — {wait}초 후 재연결 (시도 {restarts}/{})",
                        v.as_str(),
                        self.cfg.max_restarts
                    ));
                }
                AttemptEnd::Exited { code } => {
                    self.state.last_error = Some("영상 소스 연결이 끊어졌습니다".into());
                    self.say(&format!(
                        "FFmpeg 종료 (exit={}) — {wait}초 후 재연결 (시도 {restarts}/{})",
                        code.map(|c| c.to_string()).unwrap_or_else(|| "signal".into()),
                        self.cfg.max_restarts
                    ));
                }
                AttemptEnd::Stopped => unreachable!("handled above"),
            }
            self.persist(Phase::Reconnecting);
            if !self.sleep_unless_stopped(Duration::from_secs(wait), deadline) {
                self.persist(Phase::Stopped);
                return Ok(Outcome::Stopped { frames: total_frames, restarts });
            }
        }
    }

    /// Spawn one FFmpeg and watch it until something ends the attempt.
    fn one_attempt(
        &mut self,
        manifest_url: &str,
        deadline: Option<Instant>,
        total_frames: &mut u64,
    ) -> Result<AttemptEnd> {
        let builder = FfmpegCommandBuilder::new(self.cfg.tools.clone(), self.cfg.profile);
        let args =
            capped_live_args(&builder, &self.cfg.manifest_path, manifest_url, &self.cfg.destination, true);

        let mut cmd = Command::new(&self.cfg.tools.ffmpeg);
        cmd.args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child =
            cmd.spawn().map_err(|e| LiveSourceError::ffmpeg(format!("FFmpeg를 실행할 수 없습니다: {e}")))?;

        let (tx, rx) = mpsc::channel::<String>();
        for (name, pipe) in
            [("out", child.stdout.take().map(Pipe::Out)), ("err", child.stderr.take().map(Pipe::Err))]
        {
            if let Some(p) = pipe {
                let tx = tx.clone();
                let _ = name;
                std::thread::spawn(move || p.pump(tx));
            }
        }
        drop(tx);

        self.say("송출 시작 — 영상=라이브 소스 / 소리=플레이리스트 (원본 오디오 미사용), 최대 1080p");
        self.persist(Phase::Sending);

        let started = Instant::now();
        let mut dog = FrameWatchdog::new(started, self.cfg.stall_after, self.cfg.grace);
        let mut last_check = started;
        let mut last_persist = started;

        loop {
            // 1. Did someone ask us to stop, or did the clock run out?
            if self.should_stop(deadline) {
                self.kill_own_child(&mut child);
                *total_frames = dog.frame();
                self.state.frames = dog.frame();
                return Ok(AttemptEnd::Stopped);
            }

            // 2. Did FFmpeg exit on its own?
            match child.try_wait() {
                Ok(Some(status)) => {
                    *total_frames = dog.frame();
                    self.state.frames = dog.frame();
                    return Ok(AttemptEnd::Exited { code: status.code() });
                }
                Ok(None) => {}
                Err(e) => {
                    self.kill_own_child(&mut child);
                    return Err(LiveSourceError::ffmpeg(format!("FFmpeg 상태를 확인할 수 없습니다: {e}")));
                }
            }

            // 3. Drain whatever progress has arrived.
            let now = Instant::now();
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(line) => {
                    dog.observe(&line, now);
                }
                Err(RecvTimeoutError::Timeout) => {}
                // Both pipes closed: FFmpeg is on its way out. The `try_wait`
                // above will see it on the next pass.
                Err(RecvTimeoutError::Disconnected) => {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }

            // 4. Ask the watchdog, on a timer rather than per line.
            if now.duration_since(last_check) >= Duration::from_secs(1) {
                last_check = now;
                let verdict = dog.verdict(now);
                if verdict.is_fault() {
                    // This worker's own child, by handle.
                    self.kill_own_child(&mut child);
                    *total_frames = dog.frame();
                    self.state.frames = dog.frame();
                    return Ok(AttemptEnd::Fault(verdict));
                }
            }

            // 5. Keep the state file current, cheaply.
            if now.duration_since(last_persist) >= Duration::from_secs(2) {
                last_persist = now;
                self.state.frames = dog.frame();
                self.state.last_verdict = "healthy".into();
                let _ = self.store.save(&self.state);
            }
        }
    }

    /// Kill the child this worker spawned, and nothing else.
    ///
    /// `kill()` acts on the handle returned by `spawn`, so there is no pid to
    /// get wrong and no way for this to reach another broadcast's FFmpeg. The
    /// `wait()` afterwards reaps it, so a long run does not accumulate zombies.
    fn kill_own_child(&self, child: &mut Child) {
        let _ = child.kill();
        let _ = child.wait();
    }

    fn should_stop(&self, deadline: Option<Instant>) -> bool {
        self.stop.load(Ordering::SeqCst) || deadline.map(|d| Instant::now() >= d).unwrap_or(false)
    }

    /// Wait out a backoff, returning false if the wait was cut short.
    fn sleep_unless_stopped(&self, how_long: Duration, deadline: Option<Instant>) -> bool {
        let until = Instant::now() + how_long;
        while Instant::now() < until {
            if self.should_stop(deadline) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        true
    }
}

fn v_token(v: &Verdict) -> String {
    match v {
        Verdict::Healthy => "healthy".into(),
        Verdict::VideoStalled { frames, frozen_secs } => format!("frame={frames} {frozen_secs}초 정지"),
        Verdict::ProcessStalled { frozen_secs } => format!("{frozen_secs}초 무응답"),
    }
}

/// One of FFmpeg's two pipes, read a line at a time into the channel.
enum Pipe {
    Out(std::process::ChildStdout),
    Err(std::process::ChildStderr),
}

impl Pipe {
    fn pump(self, tx: mpsc::Sender<String>) {
        use std::io::{BufRead, BufReader};
        let reader: Box<dyn BufRead> = match self {
            Pipe::Out(o) => Box::new(BufReader::new(o)),
            Pipe::Err(e) => Box::new(BufReader::new(e)),
        };
        for line in reader.lines() {
            match line {
                Ok(l) => {
                    if tx.send(l).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backoff_doubles_and_is_capped() {
        let seen: Vec<u64> = (0..8).map(backoff_secs).collect();
        assert_eq!(seen, vec![2, 4, 8, 16, 32, 60, 60, 60]);
    }

    #[test]
    fn a_config_defaults_to_bounded_retries_and_1080p() {
        let c = WorkerConfig::new("w", "https://youtu.be/dQw4w9WgXcQ", "/tmp/m.txt", "rtmp://x/y", "/tmp/s");
        assert_eq!(c.max_restarts, DEFAULT_MAX_RESTARTS);
        assert_eq!(c.profile, OutputProfile::P1080p30);
        assert!(c.run_for.is_none());
    }

    #[test]
    fn a_worker_records_the_video_id_but_never_the_destination() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = WorkerConfig::new(
            "w1",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "/tmp/m.txt",
            "rtmps://a.rtmps.youtube.com/live2/super-secret-key",
            dir.path(),
        );
        struct Never;
        impl LiveSourceResolver for Never {
            fn resolve(&self, _: &str) -> Result<crate::resolver::ResolvedSource> {
                unreachable!()
            }
        }
        let w = LiveWorker::new(cfg, Arc::new(Never));
        assert_eq!(w.state.video_id.as_deref(), Some("dQw4w9WgXcQ"));
        w.store.save(&w.state).unwrap();
        let raw = std::fs::read_to_string(w.state_path()).unwrap();
        assert!(!raw.contains("super-secret-key"), "{raw}");
        assert!(!raw.contains("rtmps"), "{raw}");
    }
}
