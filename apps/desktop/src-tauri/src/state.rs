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
        let encoder = tools.as_ref().map(|t| t.detect_encoder()).unwrap_or_else(|| "libx264".to_string());
        logger.info(LogTarget::App, &format!("최적화 인코더: {encoder}"));

        let fallback = FfmpegTools::new("ffmpeg", "ffprobe");
        let builder = FfmpegCommandBuilder::new(tools.clone().unwrap_or(fallback.clone()), profile)
            .with_encoder(encoder.clone());

        let keys = Arc::new(StreamKeyStore::new(crate::platform::secrets::default_store()));
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

        Ok(Self {
            cache: MediaCache::new(cache_dir),
            paths,
            db,
            logger,
            tools,
            encoder,
            keys,
            runtime: Mutex::new(runtime),
            metrics: Mutex::new(MetricsCollector::new()),
            net: TcpNetworkChecker,
            sleep,
            clock,
            db_recovery_notice,
            startup_notice: Mutex::new(None),
            normalize_cancel: Mutex::new(None),
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

    pub fn license_state(&self) -> louver_core::license::LicenseState {
        louver_core::license::load_state(
            &self.paths.license_file(),
            self.db.get_setting_or(settings_keys::ENFORCE_DEVICE_BINDING, "false") == "true",
            // DEV_LICENSE is compiled out of release builds (§46).
            cfg!(debug_assertions),
        )
    }
}
