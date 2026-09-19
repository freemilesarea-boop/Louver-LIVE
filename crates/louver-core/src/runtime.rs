//! The broadcast runtime: the loop that ties scheduling, supervision and
//! recovery together (§17, §20, §32).
//!
//! [`BroadcastRuntime::tick`] is called about once a second by the desktop
//! app. Everything it does is driven by injected dependencies — a [`Clock`], a
//! [`StreamLauncher`] and an event sink — so the whole behaviour, including
//! scheduled start/stop and crash recovery, is covered by tests that neither
//! sleep nor need FFmpeg.

use crate::clock::Clock;
use crate::config::{OutputProfile, StreamMode};
use crate::database::models::{EventLevel, Schedule};
use crate::database::Database;
use crate::error::{ErrorCode, LouverError, Result};
use crate::scheduler::{Occurrence, ScheduleDecision, Scheduler};
use crate::security::{build_ingest_url, StreamKeyStore};
use crate::session::{SessionState, SessionStore};
use crate::streaming::engine::{build_plan, new_order_seed, SessionPlan};
use crate::streaming::ffmpeg::FfmpegCommandBuilder;
use crate::streaming::state::StreamState;
use crate::streaming::supervisor::{ProcessHandle, StreamSupervisor, SupervisorAction, SupervisorStatus};
use crate::system::SleepPreventer;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Starts FFmpeg. Abstracted so tests can run the whole runtime without it.
pub trait StreamLauncher: Send + Sync {
    fn launch(&self, supervisor: &mut StreamSupervisor, args: &[String]) -> Result<Box<dyn ProcessHandle>>;
}

/// Launches the real bundled FFmpeg.
pub struct FfmpegLauncher {
    pub program: PathBuf,
    pub log: Arc<dyn Fn(&str) + Send + Sync>,
}

impl StreamLauncher for FfmpegLauncher {
    fn launch(&self, supervisor: &mut StreamSupervisor, args: &[String]) -> Result<Box<dyn ProcessHandle>> {
        let log = Arc::clone(&self.log);
        supervisor.spawn(&self.program, args, move |l| log(l))
    }
}

/// Where runtime state changes are published (the UI, and the log).
pub trait RuntimeEvents: Send + Sync {
    fn on_status(&self, status: &RuntimeStatus);
    fn on_log(&self, level: EventLevel, message: &str);
}

/// A sink that drops everything, for tests and headless runs.
pub struct NullEvents;

impl RuntimeEvents for NullEvents {
    fn on_status(&self, _s: &RuntimeStatus) {}
    fn on_log(&self, _l: EventLevel, _m: &str) {}
}

/// Why a broadcast is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartReason {
    /// The user pressed 방송 시작.
    Manual,
    /// A schedule window opened.
    Scheduled,
    /// Resumed after a crash or power cut.
    Recovered,
}

/// Everything the dashboard needs, in one serializable struct (§24, §28).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeStatus {
    pub supervisor: SupervisorStatus,
    pub playlist_id: Option<i64>,
    pub playlist_name: Option<String>,
    pub current_item: Option<String>,
    pub next_item: Option<String>,
    pub current_index: Option<usize>,
    pub item_count: usize,
    /// Seconds since the broadcast started.
    pub elapsed_secs: i64,
    /// Seconds until the scheduled end, when scheduled.
    pub remaining_secs: Option<i64>,
    pub scheduled_end: Option<String>,
    pub start_reason: Option<StartReason>,
    pub dry_run: bool,
    /// Where the next scheduled broadcast begins.
    pub next_scheduled_start: Option<String>,
    pub cycle_duration_secs: f64,
}

impl RuntimeStatus {
    pub fn is_live(&self) -> bool {
        self.supervisor.state == StreamState::Live
    }
}

/// What the live FFmpeg process is actually doing (§13, §41).
///
/// The dashboard badge reflects the *configured* mode. This reflects the argv
/// of the process that is really running, so "is this session stream copy?"
/// can be answered from evidence rather than from configuration — which is the
/// first thing to check when CPU is unexpectedly high.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamDiagnostics {
    pub state: StreamState,
    /// Mode the session was configured with.
    pub configured_mode: StreamMode,
    /// True when the running argv contains no video encoder at all.
    pub argv_is_stream_copy: bool,
    /// Set when the configured mode and the real command disagree.
    pub mismatch: Option<String>,
    /// Any video-encoder arguments found in the live command.
    pub video_encoder_args: Vec<String>,
    /// The live command, masked so it is safe to display and copy (§34).
    pub masked_command: Vec<String>,
    pub ffmpeg_pid: Option<u32>,
    pub ffmpeg_cpu_percent: f32,
    /// Plain-language verdict for the developer panel.
    pub verdict: String,
}

/// Video-encoder arguments that must never appear in a stream-copy command.
const VIDEO_ENCODER_MARKERS: &[&str] = &[
    "-c:v",
    "-vcodec",
    "-b:v",
    "-crf",
    "-preset",
    "-x264-params",
    "libx264",
    "libx265",
    "h264_nvenc",
    "h264_qsv",
    "h264_amf",
    "h264_videotoolbox",
];

/// Options for one broadcast.
#[derive(Debug, Clone)]
pub struct StartOptions {
    pub playlist_id: i64,
    pub reason: StartReason,
    /// Write to a local file instead of RTMPS (§30).
    pub dry_run: bool,
    pub scheduled_end: Option<chrono::DateTime<chrono::Utc>>,
    pub occurrence: Option<Occurrence>,
    /// Reuse a previous session's order after a crash (§32).
    pub order_seed: Option<i64>,
}

/// The broadcast loop.
pub struct BroadcastRuntime {
    db: Database,
    builder: FfmpegCommandBuilder,
    launcher: Arc<dyn StreamLauncher>,
    clock: Arc<dyn Clock>,
    keys: Arc<StreamKeyStore>,
    sleep: Arc<dyn SleepPreventer>,
    events: Arc<dyn RuntimeEvents>,
    session_store: SessionStore,
    manifest_path: PathBuf,
    dry_run_dir: PathBuf,

    supervisor: StreamSupervisor,
    plan: Option<SessionPlan>,
    occurrence: Option<Occurrence>,
    session_id: Option<i64>,
    session_state: Option<SessionState>,
    started_at: Option<Instant>,
    start_wall: Option<chrono::DateTime<chrono::Utc>>,
    scheduled_end: Option<chrono::DateTime<chrono::Utc>>,
    reason: Option<StartReason>,
    dry_run: bool,
    /// When a restart is due after a backoff.
    restart_due: Option<Instant>,
    last_args: Vec<String>,
    /// Suppresses scheduler-driven starts after the user stops manually inside
    /// a window, until that window ends.
    suppressed_occurrence: Option<Occurrence>,
}

impl BroadcastRuntime {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: Database,
        builder: FfmpegCommandBuilder,
        launcher: Arc<dyn StreamLauncher>,
        clock: Arc<dyn Clock>,
        keys: Arc<StreamKeyStore>,
        sleep: Arc<dyn SleepPreventer>,
        events: Arc<dyn RuntimeEvents>,
        session_store: SessionStore,
        manifest_path: PathBuf,
        dry_run_dir: PathBuf,
    ) -> Self {
        Self {
            db,
            builder,
            launcher,
            clock,
            keys,
            sleep,
            events,
            session_store,
            manifest_path,
            dry_run_dir,
            supervisor: StreamSupervisor::new(StreamMode::StreamCopy),
            plan: None,
            occurrence: None,
            session_id: None,
            session_state: None,
            started_at: None,
            start_wall: None,
            scheduled_end: None,
            reason: None,
            dry_run: false,
            restart_due: None,
            last_args: Vec::new(),
            suppressed_occurrence: None,
        }
    }

    pub fn state(&self) -> StreamState {
        self.supervisor.state()
    }

    pub fn is_active(&self) -> bool {
        self.supervisor.state().is_active()
    }

    pub fn plan(&self) -> Option<&SessionPlan> {
        self.plan.as_ref()
    }

    pub fn ffmpeg_pid(&self) -> Option<u32> {
        self.supervisor.status().pid
    }

    pub fn status(&self) -> RuntimeStatus {
        let sup = self.supervisor.status();
        let elapsed = self.started_at.map(|t| t.elapsed().as_secs() as i64).unwrap_or(0);
        let (cur, next, idx) = match self.plan.as_ref().and_then(|p| {
            p.item_at(elapsed as f64)
                .map(|(i, it)| (i, it.display_name.clone(), p.next_item(i).map(|n| n.display_name.clone())))
        }) {
            Some((i, c, n)) => (Some(c), n, Some(i)),
            None => (None, None, None),
        };

        RuntimeStatus {
            supervisor: sup,
            playlist_id: self.plan.as_ref().map(|p| p.playlist_id),
            playlist_name: self
                .plan
                .as_ref()
                .and_then(|p| self.db.get_playlist(p.playlist_id).ok().flatten())
                .map(|p| p.name),
            current_item: cur,
            next_item: next,
            current_index: idx,
            item_count: self.plan.as_ref().map(|p| p.items.len()).unwrap_or(0),
            elapsed_secs: elapsed,
            remaining_secs: self.scheduled_end.map(|e| (e - self.clock.now_utc()).num_seconds().max(0)),
            scheduled_end: self.scheduled_end.map(|e| e.to_rfc3339()),
            start_reason: self.reason,
            dry_run: self.dry_run,
            next_scheduled_start: self.next_scheduled_start(),
            cycle_duration_secs: self.plan.as_ref().map(|p| p.total_duration_secs).unwrap_or(0.0),
        }
    }

    fn next_scheduled_start(&self) -> Option<String> {
        let scheds = self.db.list_schedules().ok()?;
        let sc = Scheduler::new(ClockRef(Arc::clone(&self.clock)));
        match sc.evaluate(&scheds, self.occurrence.as_ref()) {
            ScheduleDecision::Idle { next } => next.map(|o| o.start.format("%Y-%m-%d %H:%M").to_string()),
            _ => None,
        }
    }

    fn publish(&self) {
        self.events.on_status(&self.status());
    }

    fn log(&self, level: EventLevel, msg: &str) {
        self.events.on_log(level, msg);
        let _ = self.db.log_event(self.session_id, level, None, msg);
    }

    fn log_err(&self, e: &LouverError) {
        self.events.on_log(EventLevel::Error, &e.to_string());
        let _ = self.db.log_event(self.session_id, EventLevel::Error, Some(&e.code_str), &e.to_string());
    }

    // -- starting ----------------------------------------------------------

    /// Start a broadcast. The caller is expected to have run preflight (§29).
    pub fn start(&mut self, opts: StartOptions) -> Result<()> {
        if self.is_active() {
            return Err(LouverError::new(ErrorCode::StreamAlreadyRunning));
        }

        let playlist = self
            .db
            .get_playlist(opts.playlist_id)?
            .ok_or_else(|| LouverError::with_detail(ErrorCode::StreamEmptyPlaylist, "playlist not found"))?;
        let items = self.db.list_playlist_items(opts.playlist_id)?;
        let seed = opts.order_seed.unwrap_or_else(new_order_seed);

        let mode = match self.db.get_setting_or(crate::settings_keys::STREAM_MODE, "stream_copy").as_str() {
            "compatibility_encode" => StreamMode::CompatibilityEncode,
            _ => StreamMode::StreamCopy,
        };

        self.supervisor = StreamSupervisor::new(mode);
        self.supervisor.begin()?;

        let db = self.db.clone();
        let plan = build_plan(
            opts.playlist_id,
            &items,
            &move |id| db.get_media(id).ok().flatten(),
            playlist.playback_mode,
            playlist.output_profile,
            mode,
            seed,
            &self.manifest_path,
        )?;

        let destination = if opts.dry_run {
            std::fs::create_dir_all(&self.dry_run_dir)?;
            self.dry_run_dir.join("dry-run.flv").to_string_lossy().into_owned()
        } else {
            let url = self.db.get_setting_or(crate::settings_keys::RTMPS_URL, crate::DEFAULT_RTMPS_URL);
            build_ingest_url(&url, &self.keys.require()?)
        };

        let args = self.builder.build_stream_args(&plan.manifest_path, &destination, mode, true);
        self.last_args = args.clone();

        let session_id = self.db.create_session(
            opts.playlist_id,
            opts.scheduled_end.map(|e| e.to_rfc3339()).as_deref(),
            mode,
            playlist.playback_mode,
            seed,
        )?;

        self.session_id = Some(session_id);
        self.plan = Some(plan);
        self.occurrence = opts.occurrence.clone();
        self.scheduled_end = opts.scheduled_end;
        self.reason = Some(opts.reason);
        self.dry_run = opts.dry_run;
        self.restart_due = None;
        self.suppressed_occurrence = None;

        // Keep the machine awake for the whole broadcast (§23).
        if !opts.dry_run {
            let _ = self.sleep.prevent_sleep("Louver Live 방송 중");
        }

        let now = self.clock.now_utc();
        self.start_wall = Some(now);
        self.session_state = Some(SessionState {
            session_id,
            playlist_id: opts.playlist_id,
            started_at: now,
            scheduled_end: opts.scheduled_end,
            playback_mode: playlist.playback_mode,
            stream_state: StreamState::Preparing,
            stream_mode: mode,
            user_requested_stop: false,
            last_error: None,
            ffmpeg_pid: None,
            order_seed: seed,
            heartbeat_at: now,
            schedule_id: opts.occurrence.as_ref().map(|o| o.schedule_id),
        });

        self.log(
            EventLevel::Info,
            &format!(
                "방송 시작: {} ({}개 영상, {}, {:?})",
                playlist.name,
                self.plan.as_ref().map(|p| p.items.len()).unwrap_or(0),
                mode.label(),
                opts.reason
            ),
        );

        self.spawn_now()?;
        self.persist();
        self.publish();
        Ok(())
    }

    fn spawn_now(&mut self) -> Result<()> {
        let args = self.last_args.clone();
        match self.launcher.launch(&mut self.supervisor, &args) {
            Ok(child) => {
                self.supervisor.attach(child)?;
                if self.started_at.is_none() {
                    self.started_at = Some(Instant::now());
                }
                Ok(())
            }
            Err(e) => {
                self.log_err(&e);
                let _ = self.supervisor.machine_mut().transition(StreamState::Error);
                Err(e)
            }
        }
    }

    fn persist(&mut self) {
        let pid = self.supervisor.status().pid;
        let state = self.supervisor.state();
        if let Some(s) = self.session_state.as_mut() {
            s.stream_state = state;
            s.ffmpeg_pid = pid;
            s.heartbeat_at = self.clock.now_utc();
            s.user_requested_stop = self.supervisor.status().state == StreamState::Stopped
                && self.supervisor.restart_count() == 0;
            let _ = self.session_store.save(s);
        }
        if let Some(id) = self.session_id {
            let _ = self.db.update_session_state(
                id,
                state,
                self.supervisor.restart_count() as i64,
                state == StreamState::Stopped,
                self.supervisor.last_error().map(|e| e.to_string()).as_deref(),
            );
        }
    }

    // -- stopping ----------------------------------------------------------

    /// Stop the broadcast. `user_initiated` is what blocks reconnection (§17).
    pub fn stop(&mut self, user_initiated: bool) -> Result<()> {
        if self.supervisor.state().is_terminal() && self.plan.is_none() {
            return Ok(());
        }
        if user_initiated {
            // Do not let the scheduler immediately restart the window the user
            // just stopped out of.
            self.suppressed_occurrence = self.occurrence.clone();
        }
        self.supervisor.stop()?;
        let _ = self.sleep.allow_sleep();
        self.log(
            EventLevel::Info,
            if user_initiated {
                "방송을 종료했습니다 (사용자 요청)"
            } else {
                "방송을 종료했습니다 (예약 종료)"
            },
        );
        if let Some(s) = self.session_state.as_mut() {
            s.user_requested_stop = true;
            s.stream_state = StreamState::Stopped;
            let _ = self.session_store.save(s);
        }
        if let Some(id) = self.session_id {
            let _ = self.db.update_session_state(
                id,
                StreamState::Stopped,
                self.supervisor.restart_count() as i64,
                true,
                None,
            );
        }
        let _ = self.session_store.clear();
        self.plan = None;
        self.occurrence = None;
        self.started_at = None;
        self.scheduled_end = None;
        self.reason = None;
        self.session_id = None;
        self.session_state = None;
        self.restart_due = None;
        self.publish();
        Ok(())
    }

    // -- the loop ----------------------------------------------------------

    /// Advance the runtime. Call roughly once a second.
    pub fn tick(&mut self) {
        self.tick_supervisor();
        self.tick_scheduler();
        self.persist();
        self.publish();
    }

    fn tick_supervisor(&mut self) {
        // A pending restart whose backoff has elapsed.
        if let Some(due) = self.restart_due {
            if Instant::now() >= due {
                self.restart_due = None;
                if self.supervisor.machine_mut().transition(StreamState::Connecting).is_ok() {
                    self.log(EventLevel::Info, "방송 엔진을 다시 시작합니다");
                    if let Err(e) = self.spawn_now() {
                        self.log_err(&e);
                        // Try again on the next backoff rather than giving up.
                        self.restart_due = Some(Instant::now() + Duration::from_secs(5));
                    }
                }
            }
            return;
        }

        // CONNECTING becomes LIVE as soon as FFmpeg pushes real bytes.
        if self.supervisor.state() == StreamState::Connecting
            && self.supervisor.has_produced_output()
            && self.supervisor.mark_live().is_ok()
        {
            self.supervisor.note_reconnect_success();
            self.log(EventLevel::Info, "방송이 시작되었습니다");
        }
        // Nothing here refreshes the stall timer. `has_produced_output` is
        // sticky, so calling `note_data` on it would reset the timer on every
        // tick and a hung FFmpeg would stay LIVE forever. Liveness is tracked
        // by the progress reader thread instead.

        match self.supervisor.poll() {
            SupervisorAction::RestartAfter(d) => {
                self.restart_due = Some(Instant::now() + d);
                self.log(
                    EventLevel::Warn,
                    &format!("방송이 중단되었습니다. {}초 후 자동으로 다시 연결합니다", d.as_secs()),
                );
            }
            SupervisorAction::Failed => {
                self.log(EventLevel::Error, "방송을 복구하지 못했습니다");
                let _ = self.sleep.allow_sleep();
            }
            SupervisorAction::Stopped => {
                let _ = self.sleep.allow_sleep();
            }
            SupervisorAction::Running | SupervisorAction::Idle => {}
        }
    }

    fn tick_scheduler(&mut self) {
        let Ok(schedules) = self.db.list_schedules() else { return };
        let sc = Scheduler::new(ClockRef(Arc::clone(&self.clock)));

        // A manual broadcast is not the scheduler's to stop.
        let manual = self.reason == Some(StartReason::Manual);
        let running = if manual { None } else { self.occurrence.clone() };

        match sc.evaluate(&schedules, running.as_ref()) {
            ScheduleDecision::ShouldBroadcast { occurrence } => {
                if self.is_active() || self.restart_due.is_some() {
                    return;
                }
                if self.suppressed_occurrence.as_ref() == Some(&occurrence) {
                    return; // the user stopped this window on purpose
                }
                self.begin_scheduled(&occurrence, &schedules);
            }
            ScheduleDecision::ShouldStop { .. } => {
                if !manual && (self.is_active() || self.restart_due.is_some()) {
                    self.log(EventLevel::Info, "예약된 종료 시간입니다");
                    let _ = self.stop(false);
                }
            }
            ScheduleDecision::Idle { .. } => {
                // Once the suppressed window has passed, allow scheduling again.
                if let Some(o) = &self.suppressed_occurrence {
                    if !o.contains(self.clock.now_local()) {
                        self.suppressed_occurrence = None;
                    }
                }
            }
        }
    }

    fn begin_scheduled(&mut self, occurrence: &Occurrence, _schedules: &[Schedule]) {
        let end_utc = local_to_utc(occurrence.end);
        let opts = StartOptions {
            playlist_id: occurrence.playlist_id,
            reason: StartReason::Scheduled,
            dry_run: false,
            scheduled_end: Some(end_utc),
            occurrence: Some(occurrence.clone()),
            order_seed: None,
        };
        if let Err(e) = self.start(opts) {
            self.log_err(&e);
        }
    }

    /// Startup recovery (§20, §32). Returns a message for the UI when something
    /// was recovered.
    pub fn recover_on_startup(&mut self) -> Option<String> {
        let previous = self.session_store.load();
        let Ok(schedules) = self.db.list_schedules() else { return None };
        let sc = Scheduler::new(ClockRef(Arc::clone(&self.clock)));
        let active = sc.recover_on_startup(&schedules);

        // Close out anything a crashed process left dangling in the database.
        let _ = self.db.mark_orphaned_sessions("이전 실행이 비정상 종료되었습니다");

        let decision = crate::session::decide_recovery(previous, self.clock.now_utc(), active.is_some());
        let (seed, message) = match decision {
            crate::session::RecoveryDecision::Nothing => (None, None),
            crate::session::RecoveryDecision::CleanUpOnly { reason, .. } => {
                let _ = self.session_store.clear();
                self.log(EventLevel::Warn, &reason);
                (None, Some(reason))
            }
            crate::session::RecoveryDecision::Resume { session, reason } => {
                self.log(EventLevel::Warn, &reason);
                (Some(session.order_seed), Some(reason))
            }
        };

        if let Some(o) = active {
            let opts = StartOptions {
                playlist_id: o.playlist_id,
                reason: if seed.is_some() { StartReason::Recovered } else { StartReason::Scheduled },
                dry_run: false,
                scheduled_end: Some(local_to_utc(o.end)),
                occurrence: Some(o),
                order_seed: seed,
            };
            if let Err(e) = self.start(opts) {
                self.log_err(&e);
                return Some(e.message);
            }
        }
        message
    }

    /// Kill an FFmpeg left behind by a crashed run, verifying identity (§33).
    pub fn clean_orphan_process(&self) -> Option<u32> {
        let pid = self.session_store.load()?.ffmpeg_pid?;
        let mut m = crate::system::MetricsCollector::new();
        m.kill_if_ffmpeg(pid).then_some(pid)
    }

    pub fn profile(&self) -> OutputProfile {
        self.builder.profile()
    }

    /// Inspect the command that is actually running (§13).
    pub fn diagnostics(&self) -> StreamDiagnostics {
        let status = self.supervisor.status();
        let argv = &self.last_args;

        let found: Vec<String> = VIDEO_ENCODER_MARKERS
            .iter()
            .filter(|m| argv.iter().any(|a| a == *m))
            .map(|m| (*m).to_string())
            .collect();
        let is_copy = found.is_empty() && argv.windows(2).any(|w| w == ["-c", "copy"]);

        let configured = status.mode;
        let mismatch = match (configured, is_copy, argv.is_empty()) {
            (_, _, true) => None, // nothing running
            (StreamMode::StreamCopy, false, _) => Some(format!(
                "설정은 STREAM COPY이지만 실제 명령에 영상 인코더가 있습니다: {}",
                found.join(", ")
            )),
            (StreamMode::CompatibilityEncode, true, _) => {
                Some("설정은 호환 모드이지만 실제 명령은 재인코딩하지 않습니다".into())
            }
            _ => None,
        };

        let verdict = if argv.is_empty() {
            "방송 중이 아닙니다".to_string()
        } else if let Some(m) = &mismatch {
            m.clone()
        } else if is_copy {
            "STREAM COPY: 영상 재인코딩 없음 (CPU 사용량이 낮아야 정상입니다)".to_string()
        } else {
            "COMPATIBILITY MODE: 실시간 재인코딩 중 (CPU 사용량이 높습니다)".to_string()
        };

        StreamDiagnostics {
            state: status.state,
            configured_mode: configured,
            argv_is_stream_copy: is_copy,
            mismatch,
            video_encoder_args: found,
            masked_command: crate::streaming::ffmpeg::mask_argv(argv),
            ffmpeg_pid: status.pid,
            ffmpeg_cpu_percent: 0.0, // filled in by the caller, which owns the sampler
            verdict,
        }
    }

    /// Move CONNECTING -> LIVE without waiting for FFmpeg's first progress line.
    ///
    /// Test hook: integration tests drive a fake process that cannot set the
    /// supervisor's internal "has produced output" flag.
    #[doc(hidden)]
    pub fn force_live(&mut self) -> Result<()> {
        self.supervisor.mark_live()?;
        self.supervisor.note_reconnect_success();
        Ok(())
    }

    /// Make a pending reconnect backoff elapse immediately.
    ///
    /// Test hook, so reconnect tests need not sleep through the real backoff.
    #[doc(hidden)]
    pub fn force_restart_due(&mut self) {
        if self.restart_due.is_some() {
            self.restart_due = Some(Instant::now());
        }
    }

    /// Force the process to die, for the crash-simulation developer tool (§59).
    pub fn simulate_crash(&mut self) -> Result<()> {
        let pid = self
            .supervisor
            .status()
            .pid
            .ok_or_else(|| LouverError::with_detail(ErrorCode::StreamFfmpegExit, "no process running"))?;
        let mut m = crate::system::MetricsCollector::new();
        if m.kill_if_ffmpeg(pid) {
            self.log(EventLevel::Warn, "개발자 도구: FFmpeg 강제 종료를 실행했습니다");
            Ok(())
        } else {
            Err(LouverError::with_detail(ErrorCode::StreamFfmpegExit, "process was not ffmpeg"))
        }
    }
}

/// Convert a local naive datetime to UTC for storage.
pub fn local_to_utc(local: chrono::NaiveDateTime) -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    chrono::Local
        .from_local_datetime(&local)
        .earliest()
        .map(|d| d.with_timezone(&chrono::Utc))
        .unwrap_or_else(|| chrono::Utc.from_utc_datetime(&local))
}

/// Lets a `&Arc<dyn Clock>` be used where `Scheduler` wants an owned clock.
#[derive(Debug, Clone)]
struct ClockRef(Arc<dyn Clock>);

impl Clock for ClockRef {
    fn now_utc(&self) -> chrono::DateTime<chrono::Utc> {
        self.0.now_utc()
    }
    fn now_local(&self) -> chrono::NaiveDateTime {
        self.0.now_local()
    }
}
