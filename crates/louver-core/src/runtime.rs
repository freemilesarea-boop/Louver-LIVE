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
    /// Why the last attempt to start failed, if it did. Cleared by the next
    /// successful start. Without this a scheduled start that failed left the
    /// dashboard reading OFFLINE with nothing to say for itself.
    pub last_start_error: Option<LouverError>,
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
/// Which sink a local test is publishing to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "target")]
pub enum LocalTestSink {
    None,
    /// A real RTMP endpoint on this machine — a real socket and handshake.
    Rtmp(String),
    /// No endpoint was listening, so the stream went to a file instead.
    File(String),
}

impl LocalTestSink {
    pub fn label(&self) -> String {
        match self {
            Self::None => "없음".into(),
            Self::Rtmp(url) => format!("로컬 RTMP · {url}"),
            Self::File(p) => format!("파일 · {p}"),
        }
    }
}

/// Is anything listening at this `rtmp://host:port/...` URL?
///
/// Answered by trying to *bind* the port, not by connecting to it. Connecting
/// would be worse than useless here: the test ingest accepts exactly one
/// connection at a time, so a probe that connects is accepted as the real
/// publisher, fails the RTMP handshake, and takes the listener down with it —
/// leaving the broadcast that follows a moment later with nothing to connect
/// to. Binding disturbs nothing: if the address is already taken, something is
/// listening.
///
/// Only meaningful for a local address. A remote test ingest is taken at its
/// word, since binding a local port says nothing about a remote host.
fn something_is_listening(url: &str) -> bool {
    let rest = url.split("://").nth(1).unwrap_or("");
    let authority = rest.split('/').next().unwrap_or("");
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap_or(1935u16)),
        None => (authority.to_string(), 1935u16),
    };
    if host.is_empty() {
        return false;
    }
    let is_local = matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1" | "0.0.0.0");
    if !is_local {
        return true;
    }
    match std::net::TcpListener::bind(("127.0.0.1", port)) {
        // The port is free, so nothing is there to receive the stream.
        Ok(listener) => {
            drop(listener);
            false
        }
        Err(e) => e.kind() == std::io::ErrorKind::AddrInUse,
    }
}

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
    pub ffmpeg_memory_bytes: u64,
    pub reconnect_count: u32,
    /// Seconds since FFmpeg last reported progress; a rising value is a stall.
    pub seconds_since_progress: Option<u64>,
    /// True while the publisher is connected and bytes are moving.
    pub publishing: bool,
    pub bytes_sent: u64,
    /// Where a local test is publishing, when one is running.
    pub local_test_sink: LocalTestSink,
}

/// How long to wait before retrying a scheduled start that failed.
///
/// Long enough that a broken schedule does not fill the log, short enough that
/// fixing the cause during the window still gets a broadcast out of it.
const SCHEDULE_RETRY: Duration = Duration::from_secs(60);

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

/// Why a start did not happen.
///
/// The distinction is the whole point: one of these means the broadcast engine
/// is broken and belongs in red on the dashboard; the other means YouTube would
/// not take the metadata, which leaves the engine perfectly healthy.
#[derive(Debug)]
enum StartFailure {
    Stream(LouverError),
    PreStart(LouverError),
}

impl From<LouverError> for StartFailure {
    fn from(e: LouverError) -> Self {
        StartFailure::Stream(e)
    }
}

/// Work that has to succeed before FFmpeg is launched.
///
/// This exists so that manual and scheduled starts cannot drift apart: both
/// reach FFmpeg through [`BroadcastRuntime::start`], so both run whatever is
/// installed here, in the same place in the sequence. The desktop app uses it
/// to put the broadcast's YouTube metadata in place *before* the stream
/// connects — applying it afterwards means YouTube goes live under the
/// channel's default title first, which is what viewers and the watch page
/// see.
pub trait PreStartHook: Send + Sync + std::fmt::Debug {
    fn before_stream(&self, opts: &StartOptions) -> Result<()>;
}

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
    /// Skip the pre-start hook. Set when the user has been shown that the
    /// YouTube side cannot be applied and has chosen to broadcast anyway.
    pub skip_pre_start: bool,
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
    /// Which sink the last local test used, for the UI to report honestly.
    local_test_sink: LocalTestSink,
    /// When a restart is due after a backoff.
    restart_due: Option<Instant>,
    last_args: Vec<String>,
    /// Suppresses scheduler-driven starts after the user stops manually inside
    /// a window, until that window ends.
    suppressed_occurrence: Option<Occurrence>,
    /// Why the last start attempt failed, kept so the UI can say so instead of
    /// leaving the dashboard reading OFFLINE with no explanation.
    last_start_error: Option<LouverError>,
    pre_start: Option<Arc<dyn PreStartHook>>,
    /// The scheduled occurrence whose start failed, and when to try it again.
    ///
    /// The tick runs once a second; without this a schedule pointing at an
    /// empty playlist would attempt — and log — a failed start every second
    /// for the length of the window. Retrying is still wanted, because the
    /// user may add a video mid-window and expect the broadcast to pick up.
    failed_occurrence: Option<(Occurrence, Instant)>,
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
            local_test_sink: LocalTestSink::None,
            restart_due: None,
            last_args: Vec::new(),
            suppressed_occurrence: None,
            last_start_error: None,
            pre_start: None,
            failed_occurrence: None,
        }
    }

    /// Install the work that runs before every broadcast, manual or scheduled.
    pub fn set_pre_start(&mut self, hook: Arc<dyn PreStartHook>) {
        self.pre_start = Some(hook);
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
            last_start_error: self.last_start_error.clone(),
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
    ///
    /// Everything fallible is in `start_inner`, because the supervisor moves to
    /// PREPARING partway through and PREPARING counts as *active*. A failure
    /// that left it there made `is_active()` permanently true, and the
    /// scheduler — which skips its tick while the runtime is active — then sat
    /// out the rest of the window and reported tomorrow as the next broadcast.
    /// So any failure after `begin()` ends the session properly, in ERROR.
    pub fn start(&mut self, opts: StartOptions) -> Result<()> {
        if self.is_active() {
            return Err(LouverError::new(ErrorCode::StreamAlreadyRunning));
        }
        match self.start_inner(opts) {
            Ok(()) => Ok(()),
            // The broadcast engine itself failed: no playlist, no key, FFmpeg
            // would not spawn. That is an ERROR, and the dashboard should say
            // so in red.
            Err(StartFailure::Stream(e)) => {
                if self.state() == StreamState::Preparing {
                    self.abandon_start(&e);
                }
                Err(e)
            }
            // The pre-start work declined — the YouTube metadata could not be
            // applied. Nothing about the broadcast engine is broken, and
            // painting the whole dashboard red over a title says otherwise.
            // The runtime goes back to where it was, and the caller shows the
            // choice.
            Err(StartFailure::PreStart(e)) => {
                self.cancel_start();
                Err(e)
            }
        }
    }

    /// Undo a start the pre-start work declined, as if it had not begun.
    ///
    /// Not [`Self::abandon_start`]: that records a stream failure, and this is
    /// not one. No ERROR state, no `last_start_error`, nothing in red.
    fn cancel_start(&mut self) {
        self.supervisor.cancel();
        if let Some(id) = self.session_id.take() {
            let _ = self.db.update_session_state(id, StreamState::Stopped, 0, true, None);
        }
        self.plan = None;
        self.session_state = None;
        self.started_at = None;
        self.start_wall = None;
        self.scheduled_end = None;
        self.reason = None;
        let _ = self.sleep.allow_sleep();
        let _ = self.session_store.clear();
        self.publish();
    }

    /// Roll back a start that never reached a process.
    fn abandon_start(&mut self, e: &LouverError) {
        self.supervisor.abort(e.clone());
        if let Some(id) = self.session_id.take() {
            let _ = self.db.update_session_state(id, StreamState::Error, 0, false, Some(&e.to_string()));
        }
        self.plan = None;
        self.session_state = None;
        self.started_at = None;
        self.start_wall = None;
        self.scheduled_end = None;
        self.reason = None;
        // A scheduled window retries once a minute, so the same cause would
        // otherwise write the same line for the length of the window.
        let repeat = self.last_start_error.as_ref().map(|p| p.to_string()) == Some(e.to_string());
        self.last_start_error = Some(e.clone());
        let _ = self.sleep.allow_sleep();
        let _ = self.session_store.clear();
        if !repeat {
            self.log_err(e);
        }
        self.publish();
    }

    fn start_inner(&mut self, opts: StartOptions) -> std::result::Result<(), StartFailure> {
        // Named separately from the empty case: a schedule whose playlist was
        // deleted is a different problem from one whose playlist has no usable
        // video, and telling the user "no broadcastable videos" about a
        // playlist that is not there sends them looking in the wrong place.
        let playlist = self.db.get_playlist(opts.playlist_id)?.ok_or_else(|| {
            let code = if opts.occurrence.is_some() {
                ErrorCode::SchedulePlaylistMissing
            } else {
                ErrorCode::PlaylistMissing
            };
            LouverError::with_detail(code, format!("playlist id {}", opts.playlist_id))
        })?;
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
        )
        .map_err(|e| {
            if opts.occurrence.is_some() && e.code == ErrorCode::StreamEmptyPlaylist {
                LouverError::with_detail(ErrorCode::SchedulePlaylistEmpty, playlist.name.clone())
            } else {
                e
            }
        })?;

        let destination = if opts.dry_run {
            self.local_test_destination()?
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
        self.last_start_error = None;

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

        // Before FFmpeg, not after: once the stream connects, YouTube is live
        // under whatever title the resource already had. Its refusal is tagged
        // so the caller can tell it apart from the engine failing.
        if !opts.dry_run && !opts.skip_pre_start {
            if let Some(hook) = self.pre_start.clone() {
                hook.before_stream(&opts).map_err(StartFailure::PreStart)?;
            }
        }

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
                // A window whose start failed is retried, not abandoned: the
                // usual cause is something the user can fix without touching
                // the schedule (add a video, enter the stream key), and the
                // window should pick up as soon as they do.
                if let Some((failed, next_try)) = &self.failed_occurrence {
                    if failed == &occurrence && Instant::now() < *next_try {
                        return;
                    }
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

    /// Clear the retry delay on the window that failed, for tests and for a
    /// user action that plainly changed the cause (adding videos, saving a key).
    pub fn retry_failed_occurrence_now(&mut self) {
        if let Some((o, _)) = self.failed_occurrence.take() {
            self.failed_occurrence = Some((o, Instant::now()));
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
            skip_pre_start: false,
        };
        match self.start(opts) {
            // `start` has already logged and rolled the state machine back to
            // ERROR; all that is left is to space out the retry.
            Err(_) => self.failed_occurrence = Some((occurrence.clone(), Instant::now() + SCHEDULE_RETRY)),
            Ok(()) => self.failed_occurrence = None,
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
                skip_pre_start: false,
            };
            if let Err(e) = self.start(opts) {
                // `start` has already logged it; this only passes the message
                // to the launch notice the UI shows.
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
    /// Where a local test publishes to.
    ///
    /// A local RTMP endpoint when one is listening, a file otherwise. The
    /// engine is identical either way — same concat, same stream copy, same
    /// supervisor — but publishing over a real socket exercises the RTMP
    /// handshake, reconnection and the ingest's view of the stream, which a
    /// file sink cannot. `npm run app` starts such an endpoint, so the button
    /// normally takes the socket path without the user arranging anything.
    fn local_test_destination(&mut self) -> Result<String> {
        let url = self.db.get_setting_or(crate::settings_keys::LOCAL_TEST_URL, crate::DEFAULT_LOCAL_TEST_URL);
        if !url.is_empty() && something_is_listening(&url) {
            self.local_test_sink = LocalTestSink::Rtmp(url.clone());
            return Ok(url);
        }
        std::fs::create_dir_all(&self.dry_run_dir)?;
        let path = self.dry_run_dir.join("dry-run.flv").to_string_lossy().into_owned();
        self.local_test_sink = LocalTestSink::File(path.clone());
        Ok(path)
    }

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
            ffmpeg_memory_bytes: 0, // likewise
            reconnect_count: status.reconnect_count,
            seconds_since_progress: status.seconds_since_data,
            publishing: status.state == StreamState::Live && status.progress.total_bytes > 0,
            bytes_sent: status.progress.total_bytes,
            local_test_sink: if self.dry_run { self.local_test_sink.clone() } else { LocalTestSink::None },
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

#[cfg(test)]
mod local_test_sink_tests {
    use super::something_is_listening;

    #[test]
    fn a_port_with_a_listener_on_it_is_reported_as_listening() {
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(something_is_listening(&format!("rtmp://127.0.0.1:{port}/live/x")));
    }

    #[test]
    fn a_free_port_is_not() {
        // Bind and release, so the port is known to have been free.
        let port = {
            let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            l.local_addr().unwrap().port()
        };
        assert!(!something_is_listening(&format!("rtmp://127.0.0.1:{port}/live/x")));
    }

    #[test]
    fn the_check_never_connects_so_a_single_connection_listener_survives_it() {
        // The bug this guards against: a probe that *connects* is accepted as
        // the publisher by a `-listen 1` ingest, fails its handshake, and takes
        // the listener down — so the broadcast that follows finds nothing
        // there. After probing, the listener must still be able to accept.
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();

        assert!(something_is_listening(&format!("rtmp://127.0.0.1:{port}/live/x")));

        l.set_nonblocking(true).unwrap();
        match l.accept() {
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {} // nothing was consumed
            Ok(_) => panic!("the check opened a connection; a single-connection ingest would be gone"),
            Err(e) => panic!("unexpected accept error: {e}"),
        }
    }

    #[test]
    fn a_remote_ingest_is_taken_at_its_word() {
        // Binding a local port says nothing about a remote host, so the URL is
        // used rather than second-guessed.
        assert!(something_is_listening("rtmp://example.com:1935/live/x"));
    }
}
