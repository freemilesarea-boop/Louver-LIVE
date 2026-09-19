//! Media import, inspection and optimization commands (§6, §7, §9, §10).

use super::CmdResult;
use crate::state::AppState;
use louver_core::database::models::{Media, MediaStatus};
use louver_core::error::{ErrorCode, LouverError};
use louver_core::media::cache::{estimate_disk, media_hash, DiskEstimate};
use louver_core::media::is_supported_extension;
use louver_core::media::normalize::{normalize_one, CancelToken, NormalizeProgress};
use louver_core::media::probe::{check_compatibility, probe, Compatibility};
use louver_core::system::available_disk_bytes;
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::{Emitter, State};

#[derive(Serialize)]
pub struct ImportResult {
    pub imported: Vec<Media>,
    /// Files that could not be imported, with the reason.
    pub failed: Vec<ImportFailure>,
}

#[derive(Serialize)]
pub struct ImportFailure {
    pub path: String,
    pub code: String,
    pub message: String,
}

/// Probe each dropped file and record it. Nothing is re-encoded here (§6).
#[tauri::command]
pub fn import_media(state: State<'_, AppState>, paths: Vec<String>) -> CmdResult<ImportResult> {
    let builder = state.builder();
    let profile = state.profile();
    let mut imported = Vec::new();
    let mut failed = Vec::new();

    for p in paths {
        let path = PathBuf::from(&p);
        if !is_supported_extension(&path) {
            let e = LouverError::new(ErrorCode::MediaUnsupported);
            failed.push(ImportFailure { path: p, code: e.code_str, message: e.message });
            continue;
        }
        match import_one(&state, &builder, &path, profile) {
            Ok(m) => imported.push(m),
            Err(e) => failed.push(ImportFailure { path: p, code: e.code_str, message: e.message }),
        }
    }
    Ok(ImportResult { imported, failed })
}

fn import_one(
    state: &AppState,
    builder: &louver_core::streaming::ffmpeg::FfmpegCommandBuilder,
    path: &Path,
    profile: louver_core::OutputProfile,
) -> CmdResult<Media> {
    let info = probe(builder, path)?;
    let hash = media_hash(path)?;

    // A cached normalized copy from an earlier run is reused verbatim (§8).
    let cached = state.cache.lookup(&hash, profile);
    let compat = check_compatibility(&info, profile);
    let status = match (&cached, &compat) {
        (Some(_), _) => MediaStatus::Normalized,
        (None, Compatibility::Compatible) => MediaStatus::Compatible,
        (None, _) => MediaStatus::OptimizationRequired,
    };

    let media = Media {
        id: 0,
        source_path: path.to_string_lossy().into_owned(),
        display_name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        status,
        media_hash: hash.clone(),
        normalized_path: cached
            .as_ref()
            .map(|_| state.cache.normalized_path(&hash, profile).to_string_lossy().into_owned()),
        normalized_profile: cached.as_ref().map(|c| c.profile.clone()),
        normalized_duration_secs: cached.as_ref().map(|c| c.duration_secs),
        duration_secs: info.duration_secs,
        width: info.width,
        height: info.height,
        fps: info.fps,
        video_codec: info.video_codec.clone(),
        audio_codec: info.audio_codec.clone(),
        pixel_format: Some(info.pixel_format.clone()),
        is_hdr: info.is_hdr,
        file_size: info.file_size,
        added_at: String::new(),
        last_error: None,
    };
    let id = state.db.upsert_media(&media)?;
    Ok(state.db.get_media(id)?.unwrap_or(media))
}

#[tauri::command]
pub fn list_media(state: State<'_, AppState>) -> CmdResult<Vec<Media>> {
    state.db.list_media()
}

#[tauri::command]
pub fn delete_media(state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    state.db.delete_media(id)
}

/// Why a given file needs optimizing, for the UI's explanation (§7).
#[tauri::command]
pub fn compatibility_reasons(state: State<'_, AppState>, id: i64) -> CmdResult<Vec<String>> {
    let m = state.db.get_media(id)?.ok_or_else(|| LouverError::new(ErrorCode::MediaFileMissing))?;
    let info = probe(&state.builder(), Path::new(&m.source_path))?;
    Ok(check_compatibility(&info, state.profile()).reasons().to_vec())
}

/// Disk-usage plan shown before optimization starts (§10).
#[tauri::command]
pub fn estimate_optimization(state: State<'_, AppState>, media_ids: Vec<i64>) -> CmdResult<DiskEstimate> {
    let profile = state.profile();
    let mut durations = Vec::new();
    for id in media_ids {
        if let Some(m) = state.db.get_media(id)? {
            // Files already cached cost nothing more.
            if state.cache.lookup(&m.media_hash, profile).is_none() && !m.status.is_broadcast_ready() {
                durations.push(m.duration_secs);
            }
        }
    }
    Ok(estimate_disk(&durations, profile, available_disk_bytes(state.cache.root())))
}

/// Normalize a batch. Emits `louver://normalize` progress events (§9, §40).
#[tauri::command]
pub fn optimize_media(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    media_ids: Vec<i64>,
) -> CmdResult<usize> {
    let profile = state.profile();
    let builder = state.builder();

    // Refuse to start rather than filling the disk (§10).
    let estimate = estimate_optimization(state.clone(), media_ids.clone())?;
    if !estimate.has_enough_space {
        return Err(LouverError::with_detail(
            ErrorCode::StorageInsufficientSpace,
            format!(
                "필요 {} / 여유 {}",
                louver_core::system::format_bytes(estimate.estimated_bytes),
                louver_core::system::format_bytes(estimate.available_bytes)
            ),
        ));
    }

    let cancel = CancelToken::new();
    *state.normalize_cancel.lock().unwrap() = Some(cancel.clone());

    let total = media_ids.len();
    let mut done = 0usize;
    for (idx, id) in media_ids.iter().enumerate() {
        if cancel.is_cancelled() {
            break;
        }
        let Some(m) = state.db.get_media(*id)? else { continue };

        let info = match probe(&builder, Path::new(&m.source_path)) {
            Ok(i) => i,
            Err(e) => {
                state.db.update_media_status(
                    *id,
                    MediaStatus::Missing,
                    None,
                    None,
                    None,
                    Some(&e.message),
                )?;
                continue;
            }
        };

        let emit = {
            let app = app.clone();
            let name = m.display_name.clone();
            let id = *id;
            move |percent: f64| {
                let _ = app.emit(
                    "louver://normalize",
                    NormalizeProgress {
                        media_id: id,
                        file_name: name.clone(),
                        percent,
                        files_done: idx,
                        files_total: total,
                        remaining_files: total.saturating_sub(idx + 1),
                        estimated_cache_bytes: 0,
                    },
                );
            }
        };

        match normalize_one(
            &builder,
            &state.cache,
            Path::new(&m.source_path),
            &m.media_hash,
            &info,
            profile,
            &cancel,
            emit,
        ) {
            Ok(out) => {
                state.db.update_media_status(
                    *id,
                    MediaStatus::Normalized,
                    Some(&out.output_path.to_string_lossy()),
                    Some(profile.id()),
                    Some(out.duration_secs),
                    None,
                )?;
                done += 1;
            }
            Err(e) if e.code == ErrorCode::MediaNormalizeCancelled => break,
            Err(e) => {
                state.db.update_media_status(*id, MediaStatus::Failed, None, None, None, Some(&e.message))?;
            }
        }
    }

    *state.normalize_cancel.lock().unwrap() = None;
    Ok(done)
}

/// The 중단 button (§9).
#[tauri::command]
pub fn cancel_optimization(state: State<'_, AppState>) -> CmdResult<()> {
    if let Some(c) = state.normalize_cancel.lock().unwrap().as_ref() {
        c.cancel();
    }
    Ok(())
}
