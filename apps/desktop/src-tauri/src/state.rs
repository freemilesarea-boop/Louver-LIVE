//! Application state shared by every Tauri command.

use louver_core::clock::{Clock, SystemClock};
use louver_core::config::OutputProfile;
use louver_core::database::models::EventLevel;
use louver_core::database::Database;
use louver_core::error::Result;
use louver_core::logging::{LogTarget, Logger};
use louver_core::media::cache::MediaCache;
use louver_core::runtime::{BroadcastRuntime, FfmpegLauncher, RuntimeEvents, RuntimeStatus, StreamLauncher};
use louver_core::security::StreamKeyStore;
use louver_core::session::SessionStore;
use louver_core::streaming::ffmpeg::{FfmpegCommandBuilder, FfmpegTools};
use louver_core::system::{MetricsCollector, SleepPreventer, TcpNetworkChecker};
use louver_core::{settings_keys, AppPaths};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};

/// Publishes runtime status to the webview and to the log files.
pub struct TauriEvents {
    app: AppHandle,
    logger: Arc<Logger>,
}

impl RuntimeEvents for TauriEvents {
    fn on_status(&self, status: &RuntimeStatus) {
        let _ = self.app.emit("louver://status", status);
    }
    fn on_log(&self, level: EventLevel, message: &str) {
        // The logger masks; nothing here needs to remember to (§34).
        match level {
            EventLevel::Error => self.logger.error(LogTarget::Stream, message),
            EventLevel::Warn => self.logger.warn(LogTarget::Stream, message),
            EventLevel::Info => self.logger.info(LogTarget::Stream, message),
        }
        let _ = self.app.emit("louver://log", serde_json::json!({ "level": level, "message": message }));
    }
}

pub struct AppState {
    pub paths: AppPaths,
    pub db: Database,
    pub cache: MediaCache,
    pub logger: Arc<Logger>,
    pub tools: Option<FfmpegTools>,
    pub encoder: String,
    /// What the bundled FFmpeg can actually do, probed once at startup (§15).
    pub ffmpeg_caps: Option<louver_core::streaming::ffmpeg::FfmpegCapabilities>,
    pub keys: Arc<StreamKeyStore>,
    pub runtime: Mutex<BroadcastRuntime>,
    pub metrics: Mutex<MetricsCollector>,
    pub net: TcpNetworkChecker,
    pub sleep: Arc<dyn SleepPreventer>,
    pub clock: Arc<dyn Clock>,
    /// Set when the database had to be recovered at startup.
    pub db_recovery_notice: Option<String>,
    /// Message from session recovery, shown once by the UI.
    pub startup_notice: Mutex<Option<String>>,
    pub normalize_cancel: Mutex<Option<louver_core::media::normalize::CancelToken>>,
    /// Media ids waiting to be analysed and prepared, and whether a worker is
    /// already draining them.
    ///
    /// Adding files twice in a row must not start a second encoder: two
    /// preparations at once make each slower and finish the pair no sooner.
    /// The second call appends to the queue and returns; the running worker
    /// picks the ids up.
    pub media_queue: Arc<Mutex<std::collections::VecDeque<i64>>>,
    pub media_worker_running: Arc<std::sync::atomic::AtomicBool>,
    /// YouTube account, metadata and the chat bot (V2). Deliberately not
    /// reachable from the broadcast tick.
    pub youtube: Arc<crate::youtube_service::YoutubeService>,
}

impl AppState {
    pub fn build(app: AppHandle, paths: AppPaths) -> Result<Self> {
        paths.ensure()?;
        let logger = Logger::new(paths.logs_dir())?;

        let (db, recovered) = Database::open_or_recover(&paths.database())?;
        let db_recovery_notice = recovered
            .map(|p| format!("데이터베이스가 손상되어 새로 만들었습니다. 이전 파일: {}", p.display()));
        if let Some(n) = &db_recovery_notice {
            logger.warn(LogTarget::App, n);
        }

        let sidecar = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let tools = FfmpegTools::discover(sidecar.as_deref()).ok();
        if tools.is_none() {
            logger.error(LogTarget::App, "FFmpeg 사이드카를 찾지 못했습니다 (LL-CONFIG-002)");
        }

        let profile = OutputProfile::from_id(&db.get_setting_or(settings_keys::OUTPUT_PROFILE, "1080p30"))
            .unwrap_or_default();

        // Proves the encoder on this hardware rather than trusting the list.
        // Probes what this build can really do, rather than trusting the
        // encoder list or a version string (§15).
        let ffmpeg_caps = tools.as_ref().map(|t| t.capabilities());
        for problem in ffmpeg_caps.as_ref().map(|c| c.problems()).unwrap_or_default() {
            logger.error(LogTarget::App, &format!("FFmpeg 제한: {problem}"));
        }
        let encoder = ffmpeg_caps
            .as_ref()
            .and_then(|c| c.h264_encoder.clone())
            .unwrap_or_else(|| "libx264".to_string());
        logger.info(LogTarget::App, &format!("MEDIA_ENGINE encoder={encoder}"));

        let fallback = FfmpegTools::new("ffmpeg", "ffprobe");
        let builder = FfmpegCommandBuilder::new(tools.clone().unwrap_or(fallback.clone()), profile)
            .with_encoder(encoder.clone());

        let secrets = crate::platform::secrets::default_store();
        let keys = Arc::new(StreamKeyStore::new(Arc::clone(&secrets)));
        let sleep: Arc<dyn SleepPreventer> = Arc::new(crate::platform::sleep::OsSleepPreventer::new());
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);

        let ffmpeg_log = Arc::clone(&logger);
        let launcher: Arc<dyn StreamLauncher> = Arc::new(FfmpegLauncher {
            program: tools.clone().unwrap_or(fallback).ffmpeg,
            log: Arc::new(move |line: &str| ffmpeg_log.info(LogTarget::Ffmpeg, line)),
        });

        let events: Arc<dyn RuntimeEvents> = Arc::new(TauriEvents { app, logger: Arc::clone(&logger) });

        let cache_dir = db
            .get_setting(settings_keys::CACHE_LOCATION)
            .ok()
            .flatten()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| paths.cache_dir());

        let runtime = BroadcastRuntime::new(
            db.clone(),
            builder,
            launcher,
            Arc::clone(&clock),
            Arc::clone(&keys),
            Arc::clone(&sleep),
            events,
            SessionStore::new(paths.session_file()),
            paths.manifest_file(),
            paths.dry_run_dir(),
        );

        let youtube = Arc::new(crate::youtube_service::YoutubeService::new(
            db.clone(),
            Arc::clone(&secrets),
            Arc::clone(&logger),
            Arc::clone(&keys),
        ));

        Ok(Self {
            cache: MediaCache::new(cache_dir),
            paths,
            db,
            logger,
            tools,
            encoder,
            ffmpeg_caps,
            keys,
            runtime: Mutex::new(runtime),
            metrics: Mutex::new(MetricsCollector::new()),
            net: TcpNetworkChecker,
            sleep,
            clock,
            db_recovery_notice,
            startup_notice: Mutex::new(None),
            normalize_cancel: Mutex::new(None),
            media_queue: Arc::new(Mutex::new(std::collections::VecDeque::new())),
            media_worker_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            youtube,
        })
    }

    /// The command builder for the currently selected profile.
    pub fn builder(&self) -> FfmpegCommandBuilder {
        let profile =
            OutputProfile::from_id(&self.db.get_setting_or(settings_keys::OUTPUT_PROFILE, "1080p30"))
                .unwrap_or_default();
        FfmpegCommandBuilder::new(
            self.tools.clone().unwrap_or_else(|| FfmpegTools::new("ffmpeg", "ffprobe")),
            profile,
        )
        .with_encoder(self.encoder.clone())
    }

    pub fn profile(&self) -> OutputProfile {
        OutputProfile::from_id(&self.db.get_setting_or(settings_keys::OUTPUT_PROFILE, "1080p30"))
            .unwrap_or_default()
    }

    /// Developer mode gates the crash-simulation tools (§59).
    pub fn developer_mode(&self) -> bool {
        self.db.get_setting_or(settings_keys::DEVELOPER_MODE, "false") == "true" || cfg!(debug_assertions)
    }
}
