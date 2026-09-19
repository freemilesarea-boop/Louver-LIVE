//! Converting a source file into the broadcast profile (§9).
//!
//! This is the only place heavy encoding happens. It runs at import/optimize
//! time, never during a live broadcast (§2).

use crate::config::OutputProfile;
use crate::error::{ErrorCode, LouverError, Result};
use crate::media::cache::{CacheMetadata, MediaCache};
use crate::media::probe::MediaInfo;
use crate::streaming::ffmpeg::{mask_secrets, FfmpegCommandBuilder};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Progress for one file, and for the batch as a whole (§9).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizeProgress {
    pub media_id: i64,
    pub file_name: String,
    /// 0–100 for the current file.
    pub percent: f64,
    pub files_done: usize,
    pub files_total: usize,
    pub remaining_files: usize,
    /// Rough estimate of the bytes the whole batch will add to the cache.
    pub estimated_cache_bytes: u64,
}

/// Cancellation token for the "중단" button (§9).
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Turn FFmpeg's `out_time_ms` into a percentage of the expected duration.
pub fn percent_from_out_time(out_time_us: u64, total_secs: f64) -> f64 {
    if total_secs <= 0.0 {
        return 0.0;
    }
    ((out_time_us as f64 / 1_000_000.0) / total_secs * 100.0).clamp(0.0, 100.0)
}

/// Outcome of normalizing one file.
#[derive(Debug, Clone)]
pub struct NormalizeOutcome {
    pub output_path: PathBuf,
    /// Whole-frame duration of the produced file.
    pub duration_secs: f64,
    pub bytes: u64,
    /// True when an existing cache entry was reused (§8).
    pub from_cache: bool,
}

/// Normalize one file into the cache, reusing an existing entry when valid.
///
/// `on_progress` is called with 0–100 for this file.
#[allow(clippy::too_many_arguments)]
pub fn normalize_one(
    builder: &FfmpegCommandBuilder,
    cache: &MediaCache,
    source: &Path,
    media_hash: &str,
    info: &MediaInfo,
    profile: OutputProfile,
    cancel: &CancelToken,
    mut on_progress: impl FnMut(f64),
) -> Result<NormalizeOutcome> {
    if let Some(meta) = cache.lookup(media_hash, profile) {
        on_progress(100.0);
        return Ok(NormalizeOutcome {
            output_path: cache.normalized_path(media_hash, profile),
            duration_secs: meta.duration_secs,
            bytes: std::fs::metadata(cache.normalized_path(media_hash, profile))
                .map(|m| m.len())
                .unwrap_or(0),
            from_cache: true,
        });
    }
    if !source.is_file() {
        return Err(LouverError::with_detail(
            ErrorCode::MediaFileMissing,
            source.display().to_string(),
        ));
    }

    cache.prepare_entry(media_hash, profile)?;
    let final_path = cache.normalized_path(media_hash, profile);
    // Encode to a temporary name so an interrupted run never leaves a file that
    // looks like a valid cache entry.
    let tmp_path = final_path.with_extension("partial.mp4");
    let _ = std::fs::remove_file(&tmp_path);

    let target = info.snapped_duration(profile);
    let args = builder.build_normalize_args(source, &tmp_path, target, info.has_audio);

    let mut child = builder
        .command(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| LouverError::with_detail(ErrorCode::MediaNormalizeFailed, e.to_string()))?;

    // Drain stderr on a thread so a chatty encoder cannot fill the pipe buffer
    // and deadlock us.
    let stderr_tail = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    if let Some(err) = child.stderr.take() {
        let tail = Arc::clone(&stderr_tail);
        std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(std::result::Result::ok) {
                let mut t = tail.lock().unwrap();
                t.push(mask_secrets(&line));
                if t.len() > 20 {
                    t.remove(0);
                }
            }
        });
    }

    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines().map_while(std::result::Result::ok) {
            if cancel.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&tmp_path);
                return Err(LouverError::new(ErrorCode::MediaNormalizeCancelled));
            }
            if let Some(v) = line.strip_prefix("out_time_ms=") {
                if let Ok(us) = v.trim().parse::<u64>() {
                    on_progress(percent_from_out_time(us, target));
                }
            }
        }
    }

    let status = child
        .wait()
        .map_err(|e| LouverError::with_detail(ErrorCode::MediaNormalizeFailed, e.to_string()))?;

    if cancel.is_cancelled() {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(LouverError::new(ErrorCode::MediaNormalizeCancelled));
    }
    if !status.success() {
        let detail = stderr_tail.lock().unwrap().join(" | ");
        let _ = std::fs::remove_file(&tmp_path);
        return Err(LouverError::with_detail(ErrorCode::MediaNormalizeFailed, detail));
    }

    std::fs::rename(&tmp_path, &final_path)?;
    let bytes = std::fs::metadata(&final_path).map(|m| m.len()).unwrap_or(0);
    if bytes == 0 {
        let _ = std::fs::remove_file(&final_path);
        return Err(LouverError::with_detail(
            ErrorCode::MediaNormalizeFailed,
            "encoder produced an empty file",
        ));
    }

    cache.write_metadata(
        &CacheMetadata {
            media_hash: media_hash.to_string(),
            source_path: source.to_string_lossy().into_owned(),
            profile: profile.id().to_string(),
            duration_secs: target,
            created_at: chrono::Utc::now().to_rfc3339(),
            encoder: builder.encoder().to_string(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
        },
        profile,
    )?;

    on_progress(100.0);
    Ok(NormalizeOutcome { output_path: final_path, duration_secs: target, bytes, from_cache: false })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_tracks_out_time() {
        assert_eq!(percent_from_out_time(0, 10.0), 0.0);
        assert_eq!(percent_from_out_time(5_000_000, 10.0), 50.0);
        assert_eq!(percent_from_out_time(10_000_000, 10.0), 100.0);
    }

    #[test]
    fn percent_is_clamped_and_safe_for_zero_duration() {
        // FFmpeg can briefly report past the end.
        assert_eq!(percent_from_out_time(12_000_000, 10.0), 100.0);
        assert_eq!(percent_from_out_time(1_000_000, 0.0), 0.0);
        assert_eq!(percent_from_out_time(1_000_000, -5.0), 0.0);
    }

    #[test]
    fn cancel_token_is_shared_between_clones() {
        let t = CancelToken::new();
        let t2 = t.clone();
        assert!(!t.is_cancelled());
        t2.cancel();
        assert!(t.is_cancelled(), "cancelling a clone must cancel the original");
    }

    #[test]
    fn a_missing_source_fails_before_spawning_ffmpeg() {
        let d = tempfile::tempdir().unwrap();
        let cache = MediaCache::new(d.path());
        let b = FfmpegCommandBuilder::new(
            crate::streaming::ffmpeg::FfmpegTools::new("/nonexistent/ffmpeg", "/nonexistent/ffprobe"),
            OutputProfile::P1080p30,
        );
        let err = normalize_one(
            &b, &cache, Path::new("/no/such/file.mp4"), "h", &MediaInfo::default(),
            OutputProfile::P1080p30, &CancelToken::new(), |_| {},
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::MediaFileMissing);
    }

    #[test]
    fn a_valid_cache_entry_short_circuits_the_encode() {
        let d = tempfile::tempdir().unwrap();
        let cache = MediaCache::new(d.path().join("cache"));
        let prof = OutputProfile::P1080p30;
        cache.prepare_entry("hh", prof).unwrap();
        std::fs::write(cache.normalized_path("hh", prof), vec![7u8; 4096]).unwrap();
        cache
            .write_metadata(
                &CacheMetadata {
                    media_hash: "hh".into(), source_path: "/v/a.mp4".into(),
                    profile: prof.id().into(), duration_secs: 42.0,
                    created_at: String::new(), encoder: "libx264".into(), app_version: "1".into(),
                },
                prof,
            )
            .unwrap();

        // The ffmpeg path is deliberately bogus: a cache hit must not spawn it.
        let b = FfmpegCommandBuilder::new(
            crate::streaming::ffmpeg::FfmpegTools::new("/nonexistent/ffmpeg", "/nonexistent/ffprobe"),
            prof,
        );
        let mut last = 0.0;
        let out = normalize_one(
            &b, &cache, Path::new("/no/such/file.mp4"), "hh", &MediaInfo::default(),
            prof, &CancelToken::new(), |p| last = p,
        )
        .expect("cache hit should succeed without ffmpeg");
        assert!(out.from_cache);
        assert_eq!(out.duration_secs, 42.0);
        assert_eq!(out.bytes, 4096);
        assert_eq!(last, 100.0);
    }
}
