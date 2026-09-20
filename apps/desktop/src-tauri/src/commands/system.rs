//! Metrics, logs, cache maintenance and window/tray helpers (§24, §28, §34, §45).

use super::CmdResult;
use crate::state::AppState;
use louver_core::database::models::StreamEvent;
use louver_core::logging::LogTarget;
use louver_core::system::{available_disk_bytes, format_bytes, SystemMetrics};
use serde::Serialize;
use tauri::State;

#[derive(Serialize)]
pub struct DashboardMetrics {
    #[serde(flatten)]
    pub metrics: SystemMetrics,
    pub app_memory_label: String,
    pub cache_bytes: u64,
    pub cache_label: String,
    pub free_disk_bytes: u64,
    pub free_disk_label: String,
    pub sleep_prevented: bool,
    /// Readiness of the two external programs the app cannot work without.
    /// Shown on the dashboard so a broken install is visible immediately
    /// rather than at the moment someone presses 방송 시작.
    pub ffmpeg_ready: bool,
    pub ffprobe_ready: bool,
    pub stream_mode_label: String,
    /// "정식" / "개발용" / "없음" — so a development licence is never mistaken
    /// for a real one.
    pub license_label: String,
    pub license_is_development: bool,
}

#[tauri::command]
pub fn get_metrics(state: State<'_, AppState>) -> CmdResult<DashboardMetrics> {
    let pid = state.runtime.lock().unwrap().ffmpeg_pid();
    let m = state.metrics.lock().unwrap().sample(pid);
    let cache = state.cache.total_size();
    let free = available_disk_bytes(state.cache.root());
    let license = state.license_state();
    Ok(DashboardMetrics {
        app_memory_label: format_bytes(m.app_memory_bytes),
        metrics: m,
        cache_bytes: cache,
        cache_label: format_bytes(cache),
        free_disk_bytes: free,
        free_disk_label: format_bytes(free),
        sleep_prevented: state.sleep.is_preventing(),
        ffmpeg_ready: state.tools.as_ref().is_some_and(|t| t.ffmpeg.exists()),
        ffprobe_ready: state.tools.as_ref().is_some_and(|t| t.ffprobe.exists()),
        stream_mode_label: match state
            .db
            .get_setting_or(louver_core::settings_keys::STREAM_MODE, "stream_copy")
            .as_str()
        {
            "compatibility_encode" => louver_core::StreamMode::CompatibilityEncode,
            _ => louver_core::StreamMode::StreamCopy,
        }
        .label()
        .to_string(),
        license_label: match license.status {
            louver_core::license::LicenseStatus::Valid => "정식",
            louver_core::license::LicenseStatus::Development => "개발용",
            _ => "없음",
        }
        .to_string(),
        license_is_development: license.status == louver_core::license::LicenseStatus::Development,
    })
}

#[tauri::command]
pub fn recent_events(state: State<'_, AppState>, limit: Option<i64>) -> CmdResult<Vec<StreamEvent>> {
    state.db.recent_events(limit.unwrap_or(200))
}

#[tauri::command]
pub fn read_log(state: State<'_, AppState>, target: String, lines: Option<usize>) -> CmdResult<Vec<String>> {
    let t = match target.as_str() {
        "stream" => LogTarget::Stream,
        "ffmpeg" => LogTarget::Ffmpeg,
        _ => LogTarget::App,
    };
    Ok(state.logger.tail(t, lines.unwrap_or(500)))
}

#[tauri::command]
pub fn logs_directory(state: State<'_, AppState>) -> CmdResult<String> {
    Ok(state.paths.logs_dir().to_string_lossy().into_owned())
}

/// Clear the cache. The UI warns first when entries are in use (§10).
#[tauri::command]
pub fn clear_cache(state: State<'_, AppState>) -> CmdResult<String> {
    if state.runtime.lock().unwrap().is_active() {
        return Err(louver_core::LouverError::with_detail(
            louver_core::ErrorCode::StreamAlreadyRunning,
            "방송 중에는 캐시를 삭제할 수 없습니다",
        ));
    }
    let freed = state.cache.clear_all()?;

    // Everything that pointed into the cache now needs re-optimizing.
    for m in state.db.list_media()? {
        if m.normalized_path.is_some() {
            state.db.update_media_status(
                m.id,
                louver_core::database::models::MediaStatus::OptimizationRequired,
                None,
                None,
                None,
                None,
            )?;
        }
    }
    Ok(format_bytes(freed))
}

/// How many cached files a clear would destroy that a playlist still needs (§10).
#[tauri::command]
pub fn cache_in_use_count(state: State<'_, AppState>) -> CmdResult<usize> {
    let mut in_use = std::collections::HashSet::new();
    for p in state.db.list_playlists()? {
        for i in state.db.list_playlist_items(p.id)? {
            if let Some(m) = state.db.get_media(i.media_id)? {
                if m.normalized_path.is_some() {
                    in_use.insert(m.id);
                }
            }
        }
    }
    Ok(in_use.len())
}

/// Notice shown once at startup after a recovery (§32).
#[tauri::command]
pub fn take_startup_notice(state: State<'_, AppState>) -> CmdResult<Option<String>> {
    if let Some(n) = state.db_recovery_notice.clone() {
        if state.startup_notice.lock().unwrap().is_none() {
            return Ok(Some(n));
        }
    }
    Ok(state.startup_notice.lock().unwrap().take())
}

/// The warnings §62 requires before the first broadcast.
#[tauri::command]
pub fn uptime_warnings() -> CmdResult<Vec<String>> {
    Ok(vec![
        "방송 중에는 컴퓨터와 인터넷 연결이 유지되어야 합니다.".into(),
        "노트북의 경우 덮개를 닫으면 방송이 중단될 수 있습니다.".into(),
        "안정적인 라이브를 위해 유선 인터넷 연결을 권장합니다.".into(),
    ])
}
