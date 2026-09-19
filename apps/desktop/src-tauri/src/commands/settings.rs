//! Settings, stream key and licence commands (§15, §45, §46).

use super::CmdResult;
use crate::state::AppState;
use louver_core::license::LicenseState;
use louver_core::security::{key_hint, validate_stream_key};
use louver_core::system::format_bytes;
use louver_core::{settings_keys, OutputProfile};
use serde::Serialize;
use tauri::State;

#[derive(Serialize)]
pub struct SettingsView {
    pub rtmps_url: String,
    pub output_profile: String,
    pub stream_mode: String,
    pub launch_at_startup: bool,
    pub start_minimized: bool,
    pub minimize_to_tray: bool,
    pub auto_reconnect: bool,
    pub developer_mode: bool,
    pub enforce_device_binding: bool,
    pub first_run_complete: bool,
    /// Playlist the user last worked with, restored on the next launch.
    pub active_playlist: Option<i64>,
    pub cache_location: String,
    pub cache_size_bytes: u64,
    pub cache_size_label: String,
    /// Masked stream key, never the real one (§15).
    pub stream_key_masked: String,
    pub stream_key_hint: Option<String>,
    pub has_stream_key: bool,
    pub secret_backend: String,
    pub secret_backend_is_secure: bool,
    pub ffmpeg_path: Option<String>,
    pub ffmpeg_version: Option<String>,
    pub hardware_encoder: String,
    pub logs_dir: String,
    pub app_version: String,
    pub license: LicenseState,
    pub profiles: Vec<ProfileOption>,
}

#[derive(Serialize)]
pub struct ProfileOption {
    pub id: String,
    pub label: String,
    pub video_kbps: u32,
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> CmdResult<SettingsView> {
    let db = &state.db;
    let flag = |k: &str, d: &str| db.get_setting_or(k, d) == "true";

    Ok(SettingsView {
        rtmps_url: db.get_setting_or(settings_keys::RTMPS_URL, louver_core::DEFAULT_RTMPS_URL),
        output_profile: db.get_setting_or(settings_keys::OUTPUT_PROFILE, "1080p30"),
        stream_mode: db.get_setting_or(settings_keys::STREAM_MODE, "stream_copy"),
        launch_at_startup: flag(settings_keys::LAUNCH_AT_STARTUP, "false"),
        start_minimized: flag(settings_keys::START_MINIMIZED, "false"),
        minimize_to_tray: flag(settings_keys::MINIMIZE_TO_TRAY, "true"),
        auto_reconnect: flag(settings_keys::AUTO_RECONNECT, "true"),
        developer_mode: state.developer_mode(),
        enforce_device_binding: flag(settings_keys::ENFORCE_DEVICE_BINDING, "false"),
        first_run_complete: flag(settings_keys::FIRST_RUN_COMPLETE, "false"),
        active_playlist: db
            .get_setting(settings_keys::ACTIVE_PLAYLIST)
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok()),
        cache_location: state.cache.root().to_string_lossy().into_owned(),
        cache_size_bytes: state.cache.total_size(),
        cache_size_label: format_bytes(state.cache.total_size()),
        stream_key_masked: state.keys.masked(),
        stream_key_hint: state.keys.get().ok().flatten().as_deref().map(key_hint),
        has_stream_key: state.keys.has_key(),
        secret_backend: state.keys.backend_name().to_string(),
        secret_backend_is_secure: state.keys.is_secure(),
        ffmpeg_path: state.tools.as_ref().map(|t| t.ffmpeg.to_string_lossy().into_owned()),
        ffmpeg_version: state.tools.as_ref().and_then(|t| t.version().ok()),
        hardware_encoder: state.encoder.clone(),
        logs_dir: state.paths.logs_dir().to_string_lossy().into_owned(),
        app_version: louver_core::VERSION.to_string(),
        license: state.license_state(),
        profiles: OutputProfile::all()
            .iter()
            .map(|p| ProfileOption { id: p.id().into(), label: p.label().into(), video_kbps: p.video_kbps() })
            .collect(),
    })
}

#[tauri::command]
pub fn set_setting(state: State<'_, AppState>, key: String, value: String) -> CmdResult<()> {
    state.db.set_setting(&key, &value)
}

/// Store the stream key in the OS keychain. It never touches SQLite (§15, §60).
#[tauri::command]
pub fn set_stream_key(state: State<'_, AppState>, key: String) -> CmdResult<()> {
    validate_stream_key(&key)?;
    state.keys.set(&key)?;
    state.logger.info(louver_core::logging::LogTarget::App, "스트림 키를 저장했습니다");
    Ok(())
}

/// Reveal the key after the user confirms (§15). The UI shows it briefly.
#[tauri::command]
pub fn reveal_stream_key(state: State<'_, AppState>) -> CmdResult<String> {
    state.keys.require()
}

#[tauri::command]
pub fn clear_stream_key(state: State<'_, AppState>) -> CmdResult<()> {
    state.keys.clear()
}

#[tauri::command]
pub fn get_license(state: State<'_, AppState>) -> CmdResult<LicenseState> {
    Ok(state.license_state())
}

/// Install a licence file the user picked (§46).
#[tauri::command]
pub fn install_license(state: State<'_, AppState>, path: String) -> CmdResult<LicenseState> {
    std::fs::copy(&path, state.paths.license_file()).map_err(|e| {
        louver_core::LouverError::with_detail(louver_core::ErrorCode::LicenseMalformed, e.to_string())
    })?;
    Ok(state.license_state())
}
