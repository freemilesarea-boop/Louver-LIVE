//! Broadcast control, preflight and dry run (§26, §29, §30).

use super::CmdResult;
use crate::state::AppState;
use louver_core::config::StreamMode;
use louver_core::error::{ErrorCode, LouverError};
use louver_core::runtime::{RuntimeStatus, StartOptions, StartReason, StreamDiagnostics};
use louver_core::streaming::preflight::{self, PreflightInput, PreflightReport};
use tauri::State;

#[tauri::command]
pub fn get_status(state: State<'_, AppState>) -> CmdResult<RuntimeStatus> {
    Ok(state.runtime.lock().unwrap().status())
}

/// Run CHECK 1–7 without starting anything (§29).
#[tauri::command]
pub fn run_preflight(
    state: State<'_, AppState>,
    playlist_id: i64,
    dry_run: bool,
) -> CmdResult<PreflightReport> {
    let playlist = state.db.get_playlist(playlist_id)?;
    let items = state.db.list_playlist_items(playlist_id)?;
    let media: Vec<_> = items
        .iter()
        .filter(|i| i.enabled)
        .filter_map(|i| state.db.get_media(i.media_id).ok().flatten())
        .collect();

    let version = state.tools.as_ref().and_then(|t| t.version().ok());
    let ffmpeg_problems = state.ffmpeg_caps.as_ref().map(|c| c.problems()).unwrap_or_default();
    let url = state.db.get_setting_or(louver_core::settings_keys::RTMPS_URL, louver_core::DEFAULT_RTMPS_URL);

    Ok(preflight::run(
        &PreflightInput {
            playlist_exists: playlist.is_some(),
            media: &media,
            has_stream_key: state.keys.has_key(),
            rtmps_url: &url,
            ffmpeg_ok: state.tools.is_some(),
            ffmpeg_version: version.as_deref(),
            // No probe result at all means FFmpeg itself is missing, which
            // `ffmpeg_ok` above already fails on — so this must not fail twice.
            ffmpeg_can_broadcast: state.ffmpeg_caps.as_ref().map(|c| c.can_broadcast()).unwrap_or(true),
            ffmpeg_problems: &ffmpeg_problems,
            license_allows_broadcast: state.license_state().status.allows_broadcast(),
            dry_run,
        },
        &state.net,
    ))
}

/// 방송 시작 (§26). Preflight must pass first.
///
/// `skip_youtube` is the user's answer to "we could not apply your broadcast
/// settings — start anyway?". It is never a default: the caller has to have
/// been told what it gives up.
#[tauri::command]
pub fn start_broadcast(
    state: State<'_, AppState>,
    playlist_id: i64,
    skip_youtube: Option<bool>,
) -> CmdResult<RuntimeStatus> {
    let report = run_preflight(state.clone(), playlist_id, false)?;
    if !report.can_broadcast {
        let f = report.first_failure().expect("a blocked report must name a failure");
        return Err(LouverError::with_detail(
            f.code.as_deref().and_then(code_from_str).unwrap_or(ErrorCode::StreamInvalidTransition),
            f.detail.clone(),
        ));
    }

    let mut rt = state.runtime.lock().unwrap();
    rt.start(StartOptions {
        playlist_id,
        reason: StartReason::Manual,
        dry_run: false,
        scheduled_end: None,
        occurrence: None,
        order_seed: None,
        skip_pre_start: skip_youtube.unwrap_or(false),
    })?;
    Ok(rt.status())
}

/// 방송 종료 (§26). The UI confirms before calling this.
#[tauri::command]
pub fn stop_broadcast(state: State<'_, AppState>) -> CmdResult<RuntimeStatus> {
    let mut rt = state.runtime.lock().unwrap();
    rt.stop(true)?;
    Ok(rt.status())
}

/// Local Stream Test — the full broadcast pipeline against a local ingest (§30).
///
/// Not a simulation: the same concat, the same stream copy, the same
/// supervisor. Only the destination differs, and it prefers a real local RTMP
/// endpoint over a file when one is listening.
#[tauri::command]
pub fn start_dry_run(state: State<'_, AppState>, playlist_id: i64) -> CmdResult<RuntimeStatus> {
    let report = run_preflight(state.clone(), playlist_id, true)?;
    if !report.can_broadcast {
        let f = report.first_failure().expect("a blocked report must name a failure");
        return Err(LouverError::with_detail(ErrorCode::StreamNotNormalized, f.detail.clone()));
    }
    let mut rt = state.runtime.lock().unwrap();
    rt.start(StartOptions {
        playlist_id,
        reason: StartReason::Manual,
        dry_run: true,
        scheduled_end: None,
        occurrence: None,
        order_seed: None,
        skip_pre_start: true,
    })?;
    Ok(rt.status())
}

/// Which mode the live process is in, for the debug panel (§41).
#[tauri::command]
pub fn stream_mode_label(state: State<'_, AppState>) -> CmdResult<String> {
    let m = match state.db.get_setting_or(louver_core::settings_keys::STREAM_MODE, "stream_copy").as_str() {
        "compatibility_encode" => StreamMode::CompatibilityEncode,
        _ => StreamMode::StreamCopy,
    };
    Ok(m.label().to_string())
}

/// Inspect the command FFmpeg is actually running (§13).
///
/// The dashboard badge shows the configured mode; this shows what is really
/// executing, which is the first thing to check when CPU is unexpectedly high.
#[tauri::command]
pub fn stream_diagnostics(state: State<'_, AppState>) -> CmdResult<StreamDiagnostics> {
    let mut d = state.runtime.lock().unwrap().diagnostics();
    if let Some(pid) = d.ffmpeg_pid {
        let m = state.metrics.lock().unwrap().sample(Some(pid));
        d.ffmpeg_cpu_percent = m.ffmpeg_cpu_percent;
        d.ffmpeg_memory_bytes = m.ffmpeg_memory_bytes;
    }
    Ok(d)
}

/// Developer tool: kill FFmpeg to watch the supervisor recover (§59).
#[tauri::command]
pub fn simulate_ffmpeg_crash(state: State<'_, AppState>) -> CmdResult<()> {
    if !state.developer_mode() {
        return Err(LouverError::with_detail(ErrorCode::ConfigInvalid, "developer mode is off"));
    }
    state.runtime.lock().unwrap().simulate_crash()
}

fn code_from_str(s: &str) -> Option<ErrorCode> {
    louver_core::error::ALL_ERROR_CODES.iter().copied().find(|c| c.as_str() == s)
}
