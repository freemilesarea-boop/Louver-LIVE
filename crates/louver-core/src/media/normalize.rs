//! Converting a source file into the broadcast profile (§9).
//!
//! This is the only place heavy encoding happens. It runs at import/optimize
//! time, never during a live broadcast (§2).

use crate::config::OutputProfile;
use crate::error::{ErrorCode, LouverError, Result};
use crate::media::cache::{CacheMetadata, MediaCache};
use crate::media::probe::{plan_transcode, probe_max_keyframe_gap, MediaInfo, Readiness, TranscodePlan};
use crate::streaming::ffmpeg::{mask_secrets, FfmpegCommandBuilder};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// How much of a file to read when measuring its keyframe spacing.
///
/// Long enough to see several keyframes at any sane interval, short enough
/// that it is a seek and an index read rather than a scan of the whole file.
const KEYFRAME_WINDOW_SECS: u32 = 60;

/// Decide what this file needs, measuring keyframe spacing only when it could
/// change the answer (§2, §7, §8).
///
/// Public so the batch layer can ask the same question before it starts, to
/// show the user what is about to happen and to do the instant files first.
pub fn plan_for(
    builder: &FfmpegCommandBuilder,
    source: &Path,
    info: &MediaInfo,
    profile: OutputProfile,
) -> TranscodePlan {
    plan_transcode(info, profile, measured_gop(builder, source, info, profile))
}

/// The plan that keeps the source's own geometry and frame rate.
///
/// The fast path. Video is copied whenever a copy can produce a broadcastable
/// stream at all — see [`video_native_copy_reasons`] — and audio is still
/// brought to the profile's 48 kHz stereo AAC, because that is cheap and it
/// takes audio out of the question of whether two prepared files can be joined.
///
/// The one thing measured rather than read: the gap between keyframes. A copied
/// video keeps whatever spacing it arrived with, and YouTube needs a keyframe
/// every few seconds to let a viewer join and to cut between qualities. Past
/// [`OutputProfile::max_copy_gop_secs`] the video is re-encoded to restore a
/// regular one — which is the *only* reason a low frame rate can still end up
/// encoded, and it is a reason about seconds, not about frames.
pub fn plan_native(
    builder: &FfmpegCommandBuilder,
    source: &Path,
    info: &MediaInfo,
    profile: OutputProfile,
) -> TranscodePlan {
    let mut video_reasons = crate::media::probe::video_native_copy_reasons(info, profile);
    if video_reasons.is_empty() {
        if let Some(gop) = probe_max_keyframe_gap(builder, source, KEYFRAME_WINDOW_SECS) {
            let limit = profile.max_copy_gop_secs();
            if gop > limit {
                video_reasons.push(format!("키프레임 간격이 너무 깁니다 ({gop:.1}초 > {limit:.1}초)"));
            }
        }
    }
    let audio_reasons = crate::media::probe::audio_encode_reasons(info, profile);
    TranscodePlan {
        video: if video_reasons.is_empty() {
            crate::media::probe::StreamPlan::Copy
        } else {
            crate::media::probe::StreamPlan::Encode
        },
        audio: if audio_reasons.is_empty() {
            crate::media::probe::StreamPlan::Copy
        } else {
            crate::media::probe::StreamPlan::Encode
        },
        video_reasons,
        audio_reasons,
    }
}

/// Which of the four ways a file can be prepared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrepareMode {
    /// Nothing is encoded. The container is rewritten and that is all.
    Direct,
    /// The picture is copied; only the sound is converted.
    Hybrid,
    /// The picture is encoded again **at its own size and frame rate**, because
    /// something a copy cannot fix — in practice the keyframe spacing — makes it
    /// unusable on a live ingest as it stands.
    LiveNormalize,
    /// The full canonical conversion to the profile's format. What a file needs
    /// when the codec, the pixel format or the geometry is out of range, and
    /// what every item of a mixed playlist is converged onto.
    Canonical,
}

impl PrepareMode {
    /// The word the log and the database use.
    pub fn id(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Hybrid => "hybrid",
            Self::LiveNormalize => "live_normalize",
            Self::Canonical => "canonical",
        }
    }

    /// Is this the one format everything can be converged onto?
    ///
    /// Only the canonical conversion produces a file that is interchangeable
    /// with every other canonical file. The other three keep something of the
    /// source, so two of them agree only when their signatures do.
    pub fn is_canonical(self) -> bool {
        matches!(self, Self::Canonical)
    }
}

/// How one file will be prepared, and why.
#[derive(Debug, Clone)]
pub struct Preparation {
    pub mode: PrepareMode,
    pub plan: TranscodePlan,
    /// Set for [`PrepareMode::LiveNormalize`] only.
    pub video: Option<crate::streaming::ffmpeg::LiveVideoSpec>,
    /// The measured gap between keyframes, when it was measured.
    pub measured_gop_secs: Option<f64>,
}

impl Preparation {
    /// One line for the log: what is being done and why. No user content.
    pub fn summary(&self) -> String {
        let reason = self
            .plan
            .video_reasons
            .first()
            .or_else(|| self.plan.audio_reasons.first())
            .map(|r| format!(" reason={r}"))
            .unwrap_or_default();
        format!(
            "mode={} video={} audio={}{}",
            self.mode.id(),
            if self.plan.video.is_copy() { "copy" } else { "encode" },
            if self.plan.audio.is_copy() { "copy" } else { "encode" },
            reason,
        )
    }
}

/// Containers whose packets can be handed to the concat demuxer as they lie.
///
/// ffprobe reports one name for the whole MP4 family
/// (`mov,mp4,m4a,3gp,3g2,mj2`), which is the family this product's own
/// preparation writes and the only one it has ever fed to a live ingest.
/// Anything else — Matroska, WebM, MPEG-TS — may well stream-copy perfectly,
/// but it has never been on air here, so it keeps the remux it has always had.
/// The limit is deliberately about *proven* ground rather than what is
/// theoretically possible.
pub fn is_mp4_family(container: &str) -> bool {
    container.split(',').any(|c| matches!(c.trim(), "mp4" | "mov" | "m4a"))
}

/// Can this source be broadcast from exactly where it lies, with no copy of it?
///
/// [`PrepareMode::Direct`] already means every stream would be copied: the
/// codec, the pixel format, the geometry, the frame rate and the keyframe
/// spacing have all been checked and none of them needs an encoder. What the
/// remux was still buying at that point was the container — our own timescale,
/// whole-frame durations, faststart.
///
/// Of those three, only the timescale can affect a broadcast, and it does so
/// **between** items rather than within one: the concat demuxer needs its
/// inputs to agree, and agreement is what `prepared_signature` records and
/// `check_playlist_joinable` enforces — from a probe of the file that will
/// actually be read. A playlist whose items disagree is converged on the
/// canonical format exactly as before. So for a file already in our container
/// family the remux produces a second copy of the same bytes and changes
/// nothing about how it plays.
///
/// Whole-frame duration and faststart do not survive as reasons either: the
/// first matters to a muxer writing a file, and the second to a player seeking
/// over a network. Neither is what a local concat read does.
pub fn direct_source_is_broadcastable(info: &MediaInfo, prep: &Preparation) -> bool {
    prep.mode == PrepareMode::Direct
        && prep.plan.video.is_copy()
        && prep.plan.audio.is_copy()
        && is_mp4_family(&info.container)
}

/// Bitrate for a re-encode that is only fixing the keyframes.
///
/// The source is already a file somebody was happy to publish, so the target is
/// its own bitrate with a little room, not the profile's 1080p figure — which
/// on a 720p slideshow would be six times what the picture needs. Bounded below
/// so a badly-made source does not produce a worse copy, and above by what the
/// plan sells.
pub fn live_normalize_kbps(info: &MediaInfo, profile: OutputProfile) -> u32 {
    let ceiling = profile.video_kbps();
    let by_pixels = (u64::from(profile.video_kbps()) * u64::from(info.width) * u64::from(info.height)
        / (u64::from(profile.width()) * u64::from(profile.height())).max(1)) as u32;
    let target = match info.video_bitrate {
        Some(bps) => ((bps / 1000) as u32).saturating_mul(5) / 4,
        None => by_pixels,
    };
    target.clamp(1_000, ceiling)
}

/// Decide how to prepare one file: the whole of the policy, in one place.
///
/// In order, because the order is the policy:
///
/// 1. `canonical_only` — the caller has already decided (a playlist that mixes
///    formats converges on the canonical format, and nothing else will do).
/// 2. Anything a stream copy cannot fix — codec, pixel format, HDR, rotation,
///    profile/level, a picture larger than the profile, a frame rate above it —
///    is the canonical conversion.
/// 3. Otherwise the keyframe spacing is measured. Inside the limit, the picture
///    is copied and only the sound may need converting. Outside it, the picture
///    is encoded again *at its own size and rate* — the smallest encode that
///    makes the file usable live.
pub fn plan_preparation(
    builder: &FfmpegCommandBuilder,
    source: &Path,
    info: &MediaInfo,
    profile: OutputProfile,
    canonical_only: bool,
) -> Preparation {
    use crate::media::probe::{audio_native_copy_reasons, video_native_copy_reasons, StreamPlan};

    if canonical_only {
        let plan = plan_for(builder, source, info, profile);
        return Preparation { mode: PrepareMode::Canonical, plan, video: None, measured_gop_secs: None };
    }

    let blocking = video_native_copy_reasons(info, profile);
    if !blocking.is_empty() {
        let mut plan = plan_for(builder, source, info, profile);
        // The canonical path decides for itself what to encode; the reasons a
        // copy was impossible are what the log should show.
        plan.video_reasons = blocking;
        return Preparation { mode: PrepareMode::Canonical, plan, video: None, measured_gop_secs: None };
    }

    let audio_reasons = audio_native_copy_reasons(info, profile);
    let audio = if audio_reasons.is_empty() { StreamPlan::Copy } else { StreamPlan::Encode };

    let limit = profile.max_copy_gop_secs();
    let measured = probe_max_keyframe_gap(builder, source, KEYFRAME_WINDOW_SECS);
    if let Some(gop) = measured {
        if gop > limit {
            // The one thing a copy cannot fix that is not about the format: an
            // IDR has to be encoded. Everything else about the file stays.
            let keyframe_secs = limit / 2.0;
            let gop_frames = ((info.fps * keyframe_secs).round() as u32).max(1);
            return Preparation {
                mode: PrepareMode::LiveNormalize,
                plan: TranscodePlan {
                    video: StreamPlan::Encode,
                    audio,
                    video_reasons: vec![format!("키프레임 간격이 너무 깁니다 ({gop:.1}초 > {limit:.1}초)")],
                    audio_reasons,
                },
                video: Some(crate::streaming::ffmpeg::LiveVideoSpec {
                    keyframe_secs,
                    gop_frames,
                    kbps: live_normalize_kbps(info, profile),
                }),
                measured_gop_secs: measured,
            };
        }
    }

    Preparation {
        mode: if audio.is_copy() { PrepareMode::Direct } else { PrepareMode::Hybrid },
        plan: TranscodePlan { video: StreamPlan::Copy, audio, video_reasons: Vec::new(), audio_reasons },
        video: None,
        measured_gop_secs: measured,
    }
}

/// What adding this file costs: nothing if it is already cached, else a plan.
///
/// The question the library asks the moment a file is added (§2), and the same
/// question the preparation step asks, so the two cannot drift. A cache hit is
/// answered without reading a packet; everything else is planned from the
/// probe and the measured keyframe spacing.
pub fn readiness_for(
    builder: &FfmpegCommandBuilder,
    cache: &MediaCache,
    source: &Path,
    media_hash: &str,
    info: &MediaInfo,
    profile: OutputProfile,
) -> Readiness {
    if cache.lookup(media_hash, profile).is_some() {
        return Readiness::Cached;
    }
    Readiness::Prepare(plan_for(builder, source, info, profile))
}

/// The measured gap between keyframes — but only when it could change anything.
///
/// Reading packet flags costs an ffprobe pass over the first stretch of the
/// file. A video that is already going to be re-encoded for some other reason
/// gets a regular keyframe spacing out of that encode regardless, so the pass
/// is skipped and the answer is `None`.
fn measured_gop(
    builder: &FfmpegCommandBuilder,
    source: &Path,
    info: &MediaInfo,
    profile: OutputProfile,
) -> Option<f64> {
    if plan_transcode(info, profile, None).video.is_copy() {
        probe_max_keyframe_gap(builder, source, KEYFRAME_WINDOW_SECS)
    } else {
        None
    }
}

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
    /// What is being done to this file, in the user's language (§10).
    pub mode_label: String,
    /// Seconds of video produced per second of wall clock, so far.
    pub speed_x: f64,
    /// Estimated seconds left for the whole batch. Negative means unknown.
    pub eta_secs: f64,
    /// The encoder doing the work, for the advanced settings line (§7).
    pub engine_label: String,
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

/// Outcome of normalizing one file, with what it cost (§1).
#[derive(Debug, Clone)]
pub struct NormalizeOutcome {
    pub output_path: PathBuf,
    /// Whole-frame duration of the produced file.
    pub duration_secs: f64,
    pub bytes: u64,
    /// True when an existing cache entry was reused (§8).
    pub from_cache: bool,
    /// Which streams were copied and which were encoded.
    pub plan: TranscodePlan,
    /// Wall-clock time spent, including the probe.
    pub elapsed_secs: f64,
    /// What actually encoded the video: an encoder name, or "copy".
    pub video_encoder: String,
    /// Output seconds produced per second of wall clock. 1.0 is real time.
    pub speed_x: f64,
    /// Video frames written per second of wall clock.
    pub avg_encode_fps: f64,
}

/// What the user is told is happening to a file (§1, §10).
///
/// None of these say "최적화", "인코딩", "H.264" or "remux". The person adding
/// a video did not ask for a transcode; they asked to broadcast a file, and
/// what the program does to make that work is the program's business. The
/// words for the engineer are in the log line, keyed `mode=`.
pub fn mode_label_ko(plan: &TranscodePlan) -> &'static str {
    match (plan.video.is_copy(), plan.audio.is_copy()) {
        (true, true) => "바로 사용할 수 있는 영상입니다",
        (true, false) => "소리를 방송에 맞게 준비 중",
        (false, true) => "화면을 방송에 맞게 준비 중",
        (false, false) => "방송에 맞게 준비 중",
    }
}

/// The encoder in the words the Settings page uses (§4).
pub fn engine_label_ko(encoder: &str) -> &'static str {
    match encoder {
        "h264_nvenc" => "NVIDIA GPU",
        "h264_qsv" => "Intel Quick Sync",
        "h264_amf" => "AMD GPU",
        "h264_videotoolbox" => "Apple 하드웨어 가속",
        "copy" => "변환 없음",
        _ => "CPU",
    }
}

impl NormalizeOutcome {
    /// One line for the log: what was done, how fast, how big (§1).
    ///
    /// Carries no path and no user content — only the shape of the work — so
    /// it is safe to write to a log a user may send us.
    pub fn summary(&self, info: &MediaInfo, profile: OutputProfile) -> String {
        format!(
            "MEDIA_OPTIMIZE_DONE mode={} video={} audio={} in={}x{}@{:.2}fps/{} out={}x{}@{}fps \
             dur={:.1}s took={:.2}s speed={:.1}x fps={:.0} size={} encoder={}",
            self.plan.label(),
            if self.plan.video.is_copy() { "copy" } else { "encode" },
            if self.plan.audio.is_copy() { "copy" } else { "encode" },
            info.width,
            info.height,
            info.fps,
            if info.video_codec.is_empty() { "?" } else { &info.video_codec },
            profile.width(),
            profile.height(),
            profile.fps(),
            self.duration_secs,
            self.elapsed_secs,
            self.speed_x,
            self.avg_encode_fps,
            crate::system::format_bytes(self.bytes),
            self.video_encoder,
        )
    }
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
    on_progress: impl FnMut(f64),
) -> Result<NormalizeOutcome> {
    normalize_one_with(builder, cache, source, media_hash, info, profile, None, cancel, on_progress)
}

/// [`normalize_one`], with the plan decided by the caller.
///
/// The server decides between the canonical profile and the source's own
/// geometry, and that decision depends on things this function cannot see — a
/// playlist's other items, and whether an operator has forced a canonical
/// re-preparation. `None` keeps the original behaviour: plan it here.
#[allow(clippy::too_many_arguments)]
pub fn normalize_one_with(
    builder: &FfmpegCommandBuilder,
    cache: &MediaCache,
    source: &Path,
    media_hash: &str,
    info: &MediaInfo,
    profile: OutputProfile,
    prepared: Option<&Preparation>,
    cancel: &CancelToken,
    mut on_progress: impl FnMut(f64),
) -> Result<NormalizeOutcome> {
    let started = std::time::Instant::now();
    if let Some(meta) = cache.lookup(media_hash, profile) {
        on_progress(100.0);
        return Ok(NormalizeOutcome {
            output_path: cache.normalized_path(media_hash, profile),
            duration_secs: meta.duration_secs,
            bytes: std::fs::metadata(cache.normalized_path(media_hash, profile))
                .map(|m| m.len())
                .unwrap_or(0),
            from_cache: true,
            plan: TranscodePlan {
                video: crate::media::probe::StreamPlan::Copy,
                audio: crate::media::probe::StreamPlan::Copy,
                video_reasons: Vec::new(),
                audio_reasons: Vec::new(),
            },
            elapsed_secs: started.elapsed().as_secs_f64(),
            video_encoder: meta.encoder,
            speed_x: f64::INFINITY,
            avg_encode_fps: 0.0,
        });
    }
    if !source.is_file() {
        return Err(LouverError::with_detail(ErrorCode::MediaFileMissing, source.display().to_string()));
    }

    cache.prepare_entry(media_hash, profile)?;
    let final_path = cache.normalized_path(media_hash, profile);
    // Encode to a temporary name so an interrupted run never leaves a file that
    // looks like a valid cache entry.
    let tmp_path = final_path.with_extension("partial.mp4");
    let _ = std::fs::remove_file(&tmp_path);

    let target = info.snapped_duration(profile);

    // What actually has to be re-encoded (§2, §7, §8). The keyframe spacing is
    // only measured when the video would otherwise be copied, so a file headed
    // for a full encode does not pay for the extra probe.
    let plan = match prepared {
        Some(p) => p.plan.clone(),
        None => plan_for(builder, source, info, profile),
    };
    let args = match prepared.and_then(|p| p.video.as_ref()) {
        // The picture is being encoded, but only to fix what a copy cannot.
        Some(spec) => builder.build_live_normalize_args(
            source,
            &tmp_path,
            target,
            info.has_audio,
            !plan.audio.is_copy(),
            spec,
        ),
        None => builder.build_normalize_args(source, &tmp_path, target, info.has_audio, &plan),
    };

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

    let status =
        child.wait().map_err(|e| LouverError::with_detail(ErrorCode::MediaNormalizeFailed, e.to_string()))?;

    if cancel.is_cancelled() {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(LouverError::new(ErrorCode::MediaNormalizeCancelled));
    }
    if !status.success() {
        let detail = stderr_tail.lock().unwrap().join(" | ");
        let _ = std::fs::remove_file(&tmp_path);
        return Err(LouverError::with_detail(ErrorCode::MediaNormalizeFailed, detail));
    }

    let video_encoder = if plan.video.is_copy() { "copy".to_string() } else { builder.encoder().to_string() };

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
            encoder: video_encoder.clone(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
        },
        profile,
    )?;

    on_progress(100.0);
    let elapsed = started.elapsed().as_secs_f64();
    Ok(NormalizeOutcome {
        output_path: final_path,
        duration_secs: target,
        bytes,
        from_cache: false,
        plan,
        elapsed_secs: elapsed,
        video_encoder,
        speed_x: if elapsed > 0.0 { target / elapsed } else { f64::INFINITY },
        avg_encode_fps: if elapsed > 0.0 { target * f64::from(profile.fps()) / elapsed } else { 0.0 },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::probe::StreamPlan;

    /// A `Preparation` in the shape the caller checks, without a probe.
    fn prep(mode: PrepareMode, video: StreamPlan, audio: StreamPlan) -> Preparation {
        Preparation {
            mode,
            plan: TranscodePlan { video, audio, video_reasons: Vec::new(), audio_reasons: Vec::new() },
            video: None,
            measured_gop_secs: None,
        }
    }

    #[test]
    fn the_mp4_family_is_what_ffprobe_calls_it() {
        assert!(is_mp4_family("mov,mp4,m4a,3gp,3g2,mj2"));
        assert!(is_mp4_family("mp4"));
        assert!(!is_mp4_family("matroska,webm"));
        assert!(!is_mp4_family("mpegts"));
        assert!(!is_mp4_family(""));
    }

    #[test]
    fn a_direct_mp4_source_is_broadcast_where_it_lies() {
        let info = MediaInfo { container: "mov,mp4,m4a,3gp,3g2,mj2".into(), ..MediaInfo::default() };
        let p = prep(PrepareMode::Direct, StreamPlan::Copy, StreamPlan::Copy);
        assert!(direct_source_is_broadcastable(&info, &p));
    }

    #[test]
    fn anything_that_needs_an_encoder_is_still_converted() {
        let info = MediaInfo { container: "mov,mp4,m4a".into(), ..MediaInfo::default() };
        // Sound to fix.
        assert!(!direct_source_is_broadcastable(
            &info,
            &prep(PrepareMode::Hybrid, StreamPlan::Copy, StreamPlan::Encode)
        ));
        // Keyframes to fix.
        assert!(!direct_source_is_broadcastable(
            &info,
            &prep(PrepareMode::LiveNormalize, StreamPlan::Encode, StreamPlan::Copy)
        ));
        // Converging on the one interchangeable format.
        assert!(!direct_source_is_broadcastable(
            &info,
            &prep(PrepareMode::Canonical, StreamPlan::Encode, StreamPlan::Encode)
        ));
    }

    #[test]
    fn a_container_we_have_never_broadcast_still_gets_its_remux() {
        // Matroska with H.264 and AAC inside can stream-copy perfectly well,
        // and it still gets the copy it has always had: the saving is not
        // worth being the first to find out on somebody's live channel.
        let mkv = MediaInfo { container: "matroska,webm".into(), ..MediaInfo::default() };
        assert!(!direct_source_is_broadcastable(
            &mkv,
            &prep(PrepareMode::Direct, StreamPlan::Copy, StreamPlan::Copy)
        ));
    }

    #[test]
    fn a_mode_and_a_plan_that_disagree_are_not_trusted() {
        // Direct means both streams are copied. If a plan ever says otherwise,
        // the plan wins and a file is produced — the pair is checked rather
        // than the label, so a future change to one cannot quietly skip the
        // conversion.
        let info = MediaInfo { container: "mp4".into(), ..MediaInfo::default() };
        assert!(!direct_source_is_broadcastable(
            &info,
            &prep(PrepareMode::Direct, StreamPlan::Encode, StreamPlan::Copy)
        ));
    }

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
            &b,
            &cache,
            Path::new("/no/such/file.mp4"),
            "h",
            &MediaInfo::default(),
            OutputProfile::P1080p30,
            &CancelToken::new(),
            |_| {},
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
                    media_hash: "hh".into(),
                    source_path: "/v/a.mp4".into(),
                    profile: prof.id().into(),
                    duration_secs: 42.0,
                    created_at: String::new(),
                    encoder: "libx264".into(),
                    app_version: "1".into(),
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
            &b,
            &cache,
            Path::new("/no/such/file.mp4"),
            "hh",
            &MediaInfo::default(),
            prof,
            &CancelToken::new(),
            |p| last = p,
        )
        .expect("cache hit should succeed without ffmpeg");
        assert!(out.from_cache);
        assert_eq!(out.duration_secs, 42.0);
        assert_eq!(out.bytes, 4096);
        assert_eq!(last, 100.0);
    }
}
