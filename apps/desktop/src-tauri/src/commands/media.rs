//! Media import, inspection and optimization commands (§6, §7, §9, §10).

use super::CmdResult;
use crate::state::AppState;
use louver_core::database::models::{Media, MediaStatus};
use louver_core::error::{ErrorCode, LouverError};
use louver_core::logging::LogTarget;
use louver_core::media::cache::{estimate_disk, media_hash};
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

/// Record each dropped file. Nothing is read, probed or encoded here.
///
/// This is the whole of what pressing 영상 추가 costs. A file becomes a row —
/// its path, its name, its size — and the playlist can draw it. Everything
/// that needs to look inside the file happens afterwards, on a background
/// thread, because looking inside a 90-minute file is not something a person
/// should sit through to see the name they just picked.
#[tauri::command]
pub fn import_media(state: State<'_, AppState>, paths: Vec<String>) -> CmdResult<ImportResult> {
    let started = std::time::Instant::now();
    let mut imported = Vec::new();
    let mut failed = Vec::new();

    for p in paths {
        let path = PathBuf::from(&p);
        if !is_supported_extension(&path) {
            let e = LouverError::new(ErrorCode::MediaUnsupported);
            failed.push(ImportFailure { path: p, code: e.code_str, message: e.message });
            continue;
        }
        match register_one(&state, &path) {
            Ok(m) => imported.push(m),
            Err(e) => failed.push(ImportFailure { path: p, code: e.code_str, message: e.message }),
        }
    }

    state.logger.info(
        LogTarget::App,
        &format!(
            "MEDIA_REGISTER files={} failed={} took={:.0}ms",
            imported.len(),
            failed.len(),
            started.elapsed().as_secs_f64() * 1000.0,
        ),
    );
    Ok(ImportResult { imported, failed })
}

/// One row, from the file's name and size alone.
///
/// `fs::metadata` is the only touch: it proves the file is there and gives a
/// size to show. No ffprobe, no hash, no read of the contents. The metadata
/// columns stay at zero and `media_hash` stays empty until the analysis step
/// fills them — `upsert_media` keys on `source_path`, so an empty hash here
/// cannot collide with anything.
fn register_one(state: &AppState, path: &Path) -> CmdResult<Media> {
    let meta = std::fs::metadata(path)
        .map_err(|_| LouverError::with_detail(ErrorCode::MediaFileMissing, path.display().to_string()))?;

    // Re-adding a file already in the library keeps what is known about it
    // rather than throwing the analysis away and starting again.
    let existing = state.db.find_media_by_path(&path.to_string_lossy())?;
    if let Some(m) = existing {
        return Ok(m);
    }

    let media = Media {
        id: 0,
        source_path: path.to_string_lossy().into_owned(),
        display_name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        status: MediaStatus::Imported,
        media_hash: String::new(),
        normalized_path: None,
        normalized_profile: None,
        normalized_duration_secs: None,
        duration_secs: 0.0,
        width: 0,
        height: 0,
        fps: 0.0,
        video_codec: String::new(),
        audio_codec: None,
        pixel_format: None,
        is_hdr: false,
        file_size: meta.len(),
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

/// Prepare files that an earlier run left unfinished. Returns at once.
///
/// The retry path: analysis and preparation go to the same background worker
/// `add_media` uses, so there is one implementation of the work and one place
/// progress comes from. Re-analysing a row that already has its metadata
/// costs a container read, which is cheaper than a second code path.
#[tauri::command]
pub fn prepare_media(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    media_ids: Vec<i64>,
) -> CmdResult<usize> {
    let queued = media_ids.len();
    if queued > 0 {
        spawn_analysis(&app, &state, media_ids);
    }
    Ok(queued)
}

/// Add files to the library and start looking at them. Returns at once.
///
/// The user picks videos and they appear. That is the whole of this call: the
/// rows are written from the file names and sizes, and a background thread is
/// handed the ids to work through afterwards. There is still no second button
/// — analysis and preparation start on their own — but they no longer happen
/// inside the click.
///
/// This used to call `optimize_media` inline. A 90-minute source that needs
/// re-encoding takes about 26 minutes to prepare at the measured 3.45x, and
/// every second of that was spent with the playlist empty and the caller
/// blocked. Registration is now 1-2 ms per file and the rest is watchable.
#[tauri::command]
pub fn add_media(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    paths: Vec<String>,
) -> CmdResult<AddResult> {
    let result = import_media(state.clone(), paths)?;
    let pending: Vec<i64> =
        result.imported.iter().filter(|m| !m.status.is_broadcast_ready()).map(|m| m.id).collect();

    if !pending.is_empty() {
        spawn_analysis(&app, &state, pending.clone());
    }
    Ok(AddResult { imported: result.imported, failed: result.failed, analysing: pending.len() })
}

#[derive(Serialize)]
pub struct AddResult {
    pub imported: Vec<Media>,
    pub failed: Vec<ImportFailure>,
    /// How many are being looked at in the background.
    pub analysing: usize,
}

/// Everything the background worker needs, none of it borrowed from a command.
struct Worker {
    app: tauri::AppHandle,
    db: louver_core::database::Database,
    cache: louver_core::media::cache::MediaCache,
    logger: std::sync::Arc<louver_core::logging::Logger>,
    builder: louver_core::streaming::ffmpeg::FfmpegCommandBuilder,
    profile: louver_core::OutputProfile,
    cancel: CancelToken,
    queue: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<i64>>>,
    running: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Tell the UI this one row changed, so it can redraw without polling.
fn emit_media_changed(app: &tauri::AppHandle, id: i64) {
    let _ = app.emit("louver://media", serde_json::json!({ "media_id": id }));
}

/// Look at the files, then prepare them — on a thread of their own.
///
/// One thread for the whole batch, not one per file: preparation is bound by
/// the encoder or the disk, and running several at once makes each slower
/// without finishing the batch sooner.
fn spawn_analysis(app: &tauri::AppHandle, state: &AppState, ids: Vec<i64>) {
    use std::sync::atomic::Ordering;

    state.media_queue.lock().unwrap().extend(ids);

    // Already draining? The ids are in the queue and will be picked up. Two
    // workers would mean two encoders competing for the same cores.
    if state.media_worker_running.swap(true, Ordering::SeqCst) {
        return;
    }

    let cancel = CancelToken::new();
    *state.normalize_cancel.lock().unwrap() = Some(cancel.clone());

    let w = Worker {
        app: app.clone(),
        db: state.db.clone(),
        cache: state.cache.clone(),
        logger: state.logger.clone(),
        builder: state.builder(),
        profile: state.profile(),
        cancel,
        queue: state.media_queue.clone(),
        running: state.media_worker_running.clone(),
    };

    std::thread::spawn(move || {
        // Drain until the queue is empty, so files added while this is running
        // join the same batch instead of starting a rival one.
        loop {
            let batch: Vec<i64> = std::mem::take(&mut *w.queue.lock().unwrap()).into();
            if batch.is_empty() || w.cancel.is_cancelled() {
                break;
            }

            // Stage 1 for every file first. Reading a container header takes
            // about a tenth of a second, so the whole batch has its durations
            // and resolutions filled in before the first long encode begins.
            let mut ready: Vec<(i64, louver_core::media::probe::MediaInfo, String)> = Vec::new();
            for id in &batch {
                if w.cancel.is_cancelled() {
                    break;
                }
                match w.analyse(*id) {
                    Ok(Some(found)) => ready.push(found),
                    Ok(None) => {}
                    Err(e) => w.fail(*id, &e),
                }
            }

            // Stage 2: prepare. The ones that need no encoder go first, so a
            // batch that is mostly conformant turns broadcastable almost at
            // once instead of waiting behind one slow encode.
            let mut jobs: Vec<Job> = ready
                .into_iter()
                .filter_map(|(id, info, hash)| {
                    let source = w.db.get_media(id).ok().flatten()?.source_path;
                    let plan = plan_for(&w.builder, Path::new(&source), &info, w.profile);
                    Some(Job { id, source, hash, info, plan })
                })
                .collect();
            jobs.sort_by_key(|j| !j.plan.is_remux());
            w.prepare_all(jobs);
        }
        w.running.store(false, Ordering::SeqCst);
    });
}

struct Job {
    id: i64,
    source: String,
    hash: String,
    info: louver_core::media::probe::MediaInfo,
    plan: louver_core::media::probe::TranscodePlan,
}

impl Worker {
    /// Read the container header and write what it says into the row.
    ///
    /// Returns `None` for a file already prepared in an earlier run: a cache
    /// hit costs one sidecar read and skips the rest entirely.
    fn analyse(&self, id: i64) -> CmdResult<Option<(i64, louver_core::media::probe::MediaInfo, String)>> {
        let Some(mut m) = self.db.get_media(id)? else { return Ok(None) };
        let path = PathBuf::from(&m.source_path);

        let t0 = std::time::Instant::now();
        let info = probe(&self.builder, &path)?;
        let probe_ms = t0.elapsed().as_secs_f64() * 1000.0;

        let t1 = std::time::Instant::now();
        let hash = media_hash(&path)?;
        let hash_ms = t1.elapsed().as_secs_f64() * 1000.0;

        m.media_hash = hash.clone();
        m.duration_secs = info.duration_secs;
        m.width = info.width;
        m.height = info.height;
        m.fps = info.fps;
        m.video_codec = info.video_codec.clone();
        m.audio_codec = info.audio_codec.clone();
        m.pixel_format = Some(info.pixel_format.clone());
        m.is_hdr = info.is_hdr;
        m.file_size = info.file_size;
        self.db.update_media_metadata(id, &m)?;

        let cached = self.cache.lookup(&hash, self.profile);
        let hit = cached.is_some();
        if let Some(c) = cached {
            self.db.update_media_status(
                id,
                MediaStatus::Normalized,
                Some(&self.cache.normalized_path(&hash, self.profile).to_string_lossy()),
                Some(&c.profile),
                Some(c.duration_secs),
                None,
            )?;
        } else {
            self.db.update_media_status(id, MediaStatus::OptimizationRequired, None, None, None, None)?;
        }

        self.logger.info(
            LogTarget::App,
            &format!(
                "MEDIA_ANALYSE id={id} dur={:.0}s {}x{}@{:.2} probe={probe_ms:.0}ms hash={hash_ms:.0}ms cache={}",
                info.duration_secs,
                info.width,
                info.height,
                info.fps,
                if hit { "hit" } else { "miss" },
            ),
        );
        emit_media_changed(&self.app, id);
        Ok(if hit { None } else { Some((id, info, hash)) })
    }

    fn fail(&self, id: i64, e: &LouverError) {
        self.logger.warn(LogTarget::App, &format!("MEDIA_ANALYSE_FAIL id={id} {}", e.message));
        let _ = self.db.update_media_status(id, MediaStatus::Missing, None, None, None, Some(&e.message));
        emit_media_changed(&self.app, id);
    }

    /// Prepare every job, reporting progress across the batch.
    fn prepare_all(&self, jobs: Vec<Job>) {
        if jobs.is_empty() {
            nothing_left(&self.app);
            return;
        }
        // The disk check moved here from the command, and is better for it:
        // at the moment of the click nothing has been probed and every
        // duration is still zero, so the estimate would have been zero too.
        // Here the durations are real.
        let sizes: Vec<(f64, u64)> = jobs.iter().map(|j| (j.info.duration_secs, j.info.file_size)).collect();
        let estimate = estimate_disk(&sizes, self.profile, available_disk_bytes(self.cache.root()));
        if !estimate.has_enough_space {
            let detail = format!(
                "필요 {} / 여유 {}",
                louver_core::system::format_bytes(estimate.estimated_bytes),
                louver_core::system::format_bytes(estimate.available_bytes),
            );
            self.logger.error(LogTarget::App, &format!("MEDIA_PREPARE_ABORT 저장 공간 부족 {detail}"));
            let msg = LouverError::with_detail(ErrorCode::StorageInsufficientSpace, detail).message;
            for j in &jobs {
                let _ = self.db.update_media_status(j.id, MediaStatus::Failed, None, None, None, Some(&msg));
                emit_media_changed(&self.app, j.id);
            }
            nothing_left(&self.app);
            return;
        }

        let total = jobs.len();
        let total_media_secs: f64 = jobs.iter().map(|j| j.info.duration_secs.max(0.0)).sum();
        let batch_started = std::time::Instant::now();
        let mut media_secs_done = 0.0f64;

        self.logger.info(
            LogTarget::App,
            &format!(
                "MEDIA_PREPARE_START files={total} remux={} partial={} full={} engine={}",
                jobs.iter().filter(|j| j.plan.is_remux()).count(),
                jobs.iter().filter(|j| j.plan.is_partial()).count(),
                jobs.iter().filter(|j| !j.plan.video.is_copy() && !j.plan.audio.is_copy()).count(),
                self.builder.encoder(),
            ),
        );

        for (idx, job) in jobs.iter().enumerate() {
            if self.cancel.is_cancelled() {
                break;
            }
            let this_duration = job.info.duration_secs.max(0.0);
            let mode = mode_label_ko(&job.plan).to_string();
            let engine = if job.plan.video.is_copy() {
                engine_label_ko("copy")
            } else {
                engine_label_ko(self.builder.encoder())
            }
            .to_string();

            let emit = {
                let app = self.app.clone();
                let name = job.source.rsplit(['/', '\\']).next().unwrap_or("").to_string();
                let id = job.id;
                let mode = mode.clone();
                let engine = engine.clone();
                move |percent: f64| {
                    let elapsed = batch_started.elapsed().as_secs_f64();
                    let processed = media_secs_done + this_duration * (percent / 100.0);
                    let rate = if elapsed > 0.5 && processed > 0.0 { processed / elapsed } else { 0.0 };
                    let remaining = (total_media_secs - processed).max(0.0);
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
                            eta_secs: if rate > 0.0 { remaining / rate } else { -1.0 },
                            engine_label: engine.clone(),
                        },
                    );
                }
            };

            match normalize_one(
                &self.builder,
                &self.cache,
                Path::new(&job.source),
                &job.hash,
                &job.info,
                self.profile,
                &self.cancel,
                emit,
            ) {
                Ok(out) => {
                    self.logger.info(LogTarget::App, &out.summary(&job.info, self.profile));
                    let _ = self.db.update_media_status(
                        job.id,
                        MediaStatus::Normalized,
                        Some(&out.output_path.to_string_lossy()),
                        Some(self.profile.id()),
                        Some(out.duration_secs),
                        None,
                    );
                }
                Err(e) if e.code == ErrorCode::MediaNormalizeCancelled => break,
                Err(e) => {
                    self.logger.warn(
                        LogTarget::App,
                        &format!("MEDIA_PREPARE_FAIL mode={} {}", job.plan.label(), e.message),
                    );
                    let _ = self.db.update_media_status(
                        job.id,
                        MediaStatus::Failed,
                        None,
                        None,
                        None,
                        Some(&e.message),
                    );
                }
            }
            emit_media_changed(&self.app, job.id);
            media_secs_done += this_duration;
        }

        self.logger.info(
            LogTarget::App,
            &format!("MEDIA_PREPARE_END took={:.1}s", batch_started.elapsed().as_secs_f64()),
        );
        nothing_left(&self.app);
    }
}

/// Clear the progress bar: the batch is over, however it ended.
fn nothing_left(app: &tauri::AppHandle) {
    let _ = app.emit(
        "louver://normalize",
        NormalizeProgress {
            media_id: 0,
            file_name: String::new(),
            percent: 100.0,
            files_done: 0,
            files_total: 0,
            remaining_files: 0,
            estimated_cache_bytes: 0,
            mode_label: String::new(),
            speed_x: 0.0,
            eta_secs: -1.0,
            engine_label: String::new(),
        },
    );
}

/// The 중단 button (§9).
#[tauri::command]
pub fn cancel_optimization(state: State<'_, AppState>) -> CmdResult<()> {
    if let Some(c) = state.normalize_cancel.lock().unwrap().as_ref() {
        c.cancel();
    }
    Ok(())
}
