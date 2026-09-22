//! Media import, inspection and optimization commands (§6, §7, §9, §10).

use super::CmdResult;
use crate::state::AppState;
use louver_core::database::models::{Media, MediaStatus};
use louver_core::error::{ErrorCode, LouverError};
use louver_core::logging::LogTarget;
use louver_core::media::cache::{estimate_disk, media_hash, DiskEstimate};
use louver_core::media::is_supported_extension;
use louver_core::media::normalize::{
    engine_label_ko, mode_label_ko, normalize_one, plan_for, CancelToken, NormalizeProgress,
};
use louver_core::media::probe::probe;
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

    // A cached normalized copy from an earlier run is reused verbatim (§8):
    // the file was prepared once and nothing about it has changed, so it is
    // ready without reading a packet of the source.
    let cached = state.cache.lookup(&hash, profile);
    let status = if cached.is_some() { MediaStatus::Normalized } else { MediaStatus::OptimizationRequired };

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

/// Why a given file has to be prepared. Advanced detail, not the main UI (§1).
#[tauri::command]
pub fn compatibility_reasons(state: State<'_, AppState>, id: i64) -> CmdResult<Vec<String>> {
    let m = state.db.get_media(id)?.ok_or_else(|| LouverError::new(ErrorCode::MediaFileMissing))?;
    let builder = state.builder();
    let path = Path::new(&m.source_path);
    let info = probe(&builder, path)?;
    let p = plan_for(&builder, path, &info, state.profile());
    let mut r = p.video_reasons.clone();
    r.extend(p.audio_reasons.clone());
    // A remux has no per-stream reason: nothing about the picture or the sound
    // is wrong, only the container they are wrapped in.
    if r.is_empty() {
        r.push("파일 형식만 방송용으로 정리하면 됩니다".into());
    }
    Ok(r)
}

/// Disk-usage plan shown before optimization starts (§10).
#[tauri::command]
pub fn estimate_optimization(state: State<'_, AppState>, media_ids: Vec<i64>) -> CmdResult<DiskEstimate> {
    let profile = state.profile();
    // Duration and current size per file: a file that only needs a remux comes
    // out about as big as it went in, so its own size is the better estimate.
    let mut durations: Vec<(f64, u64)> = Vec::new();
    for id in media_ids {
        if let Some(m) = state.db.get_media(id)? {
            // Files already cached cost nothing more.
            if state.cache.lookup(&m.media_hash, profile).is_none() && !m.status.is_broadcast_ready() {
                durations.push((m.duration_secs, m.file_size));
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

    // Probe everything first, so the batch knows what it is in for: which
    // files are instant, how many seconds of video there are in total, and
    // therefore how long the whole thing will take (§10).
    struct Job {
        id: i64,
        name: String,
        source: String,
        hash: String,
        info: louver_core::media::probe::MediaInfo,
        plan: louver_core::media::probe::TranscodePlan,
    }

    let mut jobs: Vec<Job> = Vec::new();
    for id in &media_ids {
        if cancel.is_cancelled() {
            break;
        }
        let Some(m) = state.db.get_media(*id)? else { continue };
        match probe(&builder, Path::new(&m.source_path)) {
            Ok(info) => {
                let plan = plan_for(&builder, Path::new(&m.source_path), &info, profile);
                jobs.push(Job {
                    id: *id,
                    name: m.display_name,
                    source: m.source_path,
                    hash: m.media_hash,
                    info,
                    plan,
                });
            }
            Err(e) => {
                state.db.update_media_status(
                    *id,
                    MediaStatus::Missing,
                    None,
                    None,
                    None,
                    Some(&e.message),
                )?;
            }
        }
    }

    // The ones that need no encoder go first. They finish in the time it takes
    // to copy the file, so a library that is mostly already broadcast-ready
    // turns ready almost at once instead of waiting behind one slow encode.
    jobs.sort_by_key(|j| !j.plan.is_remux());

    let remux_count = jobs.iter().filter(|j| j.plan.is_remux()).count();
    state.logger.info(
        LogTarget::App,
        &format!(
            "MEDIA_OPTIMIZE_START files={} remux={} partial={} full={} engine={}",
            jobs.len(),
            remux_count,
            jobs.iter().filter(|j| j.plan.is_partial()).count(),
            jobs.iter().filter(|j| !j.plan.video.is_copy() && !j.plan.audio.is_copy()).count(),
            builder.encoder(),
        ),
    );

    let total = jobs.len();
    let total_media_secs: f64 = jobs.iter().map(|j| j.info.duration_secs.max(0.0)).sum();
    let batch_started = std::time::Instant::now();
    let mut media_secs_done = 0.0f64;
    let mut done = 0usize;

    for (idx, job) in jobs.iter().enumerate() {
        if cancel.is_cancelled() {
            break;
        }
        let this_duration = job.info.duration_secs.max(0.0);
        let mode = mode_label_ko(&job.plan).to_string();
        let engine = if job.plan.video.is_copy() {
            engine_label_ko("copy")
        } else {
            engine_label_ko(builder.encoder())
        }
        .to_string();

        let emit = {
            let app = app.clone();
            let name = job.name.clone();
            let id = job.id;
            let mode = mode.clone();
            let engine = engine.clone();
            move |percent: f64| {
                // Rate over the whole batch so far, in seconds of video per
                // second of wall clock. Using the batch rather than this file
                // keeps the estimate steady when a remux and an encode follow
                // each other, which differ by two orders of magnitude.
                let elapsed = batch_started.elapsed().as_secs_f64();
                let processed = media_secs_done + this_duration * (percent / 100.0);
                let rate = if elapsed > 0.5 && processed > 0.0 { processed / elapsed } else { 0.0 };
                let remaining_media = (total_media_secs - processed).max(0.0);
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
                        mode_label: mode.clone(),
                        speed_x: rate,
                        eta_secs: if rate > 0.0 { remaining_media / rate } else { -1.0 },
                        engine_label: engine.clone(),
                    },
                );
            }
        };

        match normalize_one(
            &builder,
            &state.cache,
            Path::new(&job.source),
            &job.hash,
            &job.info,
            profile,
            &cancel,
            emit,
        ) {
            Ok(out) => {
                state.logger.info(LogTarget::App, &out.summary(&job.info, profile));
                state.db.update_media_status(
                    job.id,
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
                state.logger.warn(
                    LogTarget::App,
                    &format!("MEDIA_OPTIMIZE_FAIL mode={} {}", job.plan.label(), e.message),
                );
                state.db.update_media_status(
                    job.id,
                    MediaStatus::Failed,
                    None,
                    None,
                    None,
                    Some(&e.message),
                )?;
            }
        }
        media_secs_done += this_duration;
    }

    state.logger.info(
        LogTarget::App,
        &format!("MEDIA_OPTIMIZE_END done={done}/{total} took={:.1}s", batch_started.elapsed().as_secs_f64()),
    );

    *state.normalize_cancel.lock().unwrap() = None;
    Ok(done)
}

/// Add files and make them broadcastable, in one call (§1, §5).
///
/// The user picks videos and waits; there is no second button to find. What
/// happens in between depends on the files: one that already matches the
/// profile is ready the moment it is probed and nothing is written, and one
/// that does not is prepared — as a remux, or as an encode of only the stream
/// that needs it. Both outcomes look the same from the outside, which is the
/// point: "optimization" is not a thing this program asks a person to think
/// about.
///
/// Import and preparation stay separate commands underneath. A batch that
/// fails to prepare has still been imported, so the library shows the files
/// with what went wrong rather than losing them.
#[tauri::command]
pub fn add_media(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    paths: Vec<String>,
) -> CmdResult<AddResult> {
    let result = import_media(state.clone(), paths)?;
    let pending: Vec<i64> =
        result.imported.iter().filter(|m| !m.status.is_broadcast_ready()).map(|m| m.id).collect();

    let ready_at_once = result.imported.len() - pending.len();
    state.logger.info(
        LogTarget::App,
        &format!(
            "MEDIA_ADD imported={} ready={} to_prepare={} failed={}",
            result.imported.len(),
            ready_at_once,
            pending.len(),
            result.failed.len(),
        ),
    );

    let prepared = if pending.is_empty() { 0 } else { optimize_media(app, state, pending)? };
    Ok(AddResult { imported: result.imported, failed: result.failed, ready_at_once, prepared })
}

#[derive(Serialize)]
pub struct AddResult {
    pub imported: Vec<Media>,
    pub failed: Vec<ImportFailure>,
    /// Files that needed nothing done to them at all (§4).
    pub ready_at_once: usize,
    /// Files that were prepared, whether by a remux or an encode.
    pub prepared: usize,
}

/// The 중단 button (§9).
#[tauri::command]
pub fn cancel_optimization(state: State<'_, AppState>) -> CmdResult<()> {
    if let Some(c) = state.normalize_cancel.lock().unwrap().as_ref() {
        c.cancel();
    }
    Ok(())
}
