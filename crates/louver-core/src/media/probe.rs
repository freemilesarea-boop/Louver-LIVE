//! ffprobe-based media inspection (§6).

use crate::config::OutputProfile;
use crate::error::{ErrorCode, LouverError, Result};
use crate::streaming::ffmpeg::FfmpegCommandBuilder;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Everything §6 asks us to record about a source file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MediaInfo {
    pub container: String,
    pub duration_secs: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub video_codec: String,
    /// H.264 profile as ffprobe names it: "High", "Main", "Constrained Baseline".
    pub video_profile: String,
    /// H.264 level ×10, so 4.2 arrives as 42.
    pub video_level: Option<u32>,
    pub pixel_format: String,
    pub video_bitrate: Option<u64>,
    pub audio_codec: Option<String>,
    pub audio_sample_rate: Option<u32>,
    pub audio_channels: Option<u32>,
    pub audio_bitrate: Option<u64>,
    /// Video stream time base, e.g. "1/30000".
    pub time_base: String,
    /// Display-matrix rotation in degrees.
    pub rotation: i32,
    pub is_hdr: bool,
    pub file_size: u64,
    pub has_audio: bool,
}

impl MediaInfo {
    /// Whole video frames at the profile's frame rate, which is the duration a
    /// normalized copy will have (§13: every file must end on a frame boundary).
    ///
    /// Rounds *down*: asking FFmpeg for a frame the source does not have makes
    /// it hold the last frame, which shows up as a stutter at every loop seam.
    /// Losing up to one frame is invisible; a repeated hitch is not.
    pub fn snapped_duration(&self, profile: OutputProfile) -> f64 {
        let fps = f64::from(profile.fps());
        (self.duration_secs * fps).floor().max(1.0) / fps
    }
}

/// Parse `fps` from ffprobe's `"30000/1001"` rational form.
pub fn parse_rational(s: &str) -> Option<f64> {
    let (n, d) = s.split_once('/')?;
    let (n, d): (f64, f64) = (n.trim().parse().ok()?, d.trim().parse().ok()?);
    if d == 0.0 {
        return None;
    }
    Some(n / d)
}

/// HDR detection from colour metadata (§6). Anything PQ/HLG or BT.2020 is HDR
/// for our purposes: it needs tone mapping to the Rec.709 SDR output.
pub fn detect_hdr(color_transfer: &str, color_primaries: &str, pix_fmt: &str) -> bool {
    const HDR_TRC: [&str; 4] = ["smpte2084", "arib-std-b67", "smpte428", "bt2020-10"];
    HDR_TRC.contains(&color_transfer)
        || color_primaries == "bt2020"
        || pix_fmt.contains("p010")
        || pix_fmt.contains("yuv420p10")
}

/// Run ffprobe and turn its JSON into a [`MediaInfo`].
pub fn probe(builder: &FfmpegCommandBuilder, path: &Path) -> Result<MediaInfo> {
    if !path.is_file() {
        return Err(LouverError::with_detail(ErrorCode::MediaFileMissing, path.display().to_string()));
    }
    let args = builder.build_probe_args(path);
    let out = builder
        .probe_command(&args)
        .output()
        .map_err(|e| LouverError::with_detail(ErrorCode::MediaProbeFailed, e.to_string()))?;

    if !out.status.success() {
        return Err(LouverError::with_detail(
            ErrorCode::MediaProbeFailed,
            String::from_utf8_lossy(&out.stderr).trim(),
        ));
    }
    let json = String::from_utf8_lossy(&out.stdout);
    let mut info = parse_probe_json(&json)?;
    info.file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    Ok(info)
}

/// Parse ffprobe `-print_format json` output. Split out so it can be tested
/// against fixtures without running ffprobe.
pub fn parse_probe_json(json: &str) -> Result<MediaInfo> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| LouverError::with_detail(ErrorCode::MediaProbeFailed, e.to_string()))?;

    let streams = v["streams"].as_array().cloned().unwrap_or_default();
    let video = streams
        .iter()
        .find(|s| s["codec_type"] == "video")
        .ok_or_else(|| LouverError::new(ErrorCode::MediaNoVideoStream))?;
    let audio = streams.iter().find(|s| s["codec_type"] == "audio");

    let fmt = &v["format"];
    let duration = fmt["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| video["duration"].as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0.0);

    // avg_frame_rate is 0/0 for streams with no frames; r_frame_rate is the
    // more reliable fallback for a well-formed file.
    let fps = video["avg_frame_rate"]
        .as_str()
        .and_then(parse_rational)
        .filter(|f| *f > 0.0)
        .or_else(|| video["r_frame_rate"].as_str().and_then(parse_rational))
        .unwrap_or(0.0);

    let pix_fmt = video["pix_fmt"].as_str().unwrap_or_default().to_string();
    let rotation = video["side_data_list"]
        .as_array()
        .and_then(|l| l.iter().find_map(|d| d["rotation"].as_i64()))
        .or_else(|| video["tags"]["rotate"].as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0) as i32;

    Ok(MediaInfo {
        container: fmt["format_name"].as_str().unwrap_or_default().to_string(),
        duration_secs: duration,
        width: video["width"].as_u64().unwrap_or(0) as u32,
        height: video["height"].as_u64().unwrap_or(0) as u32,
        fps,
        video_codec: video["codec_name"].as_str().unwrap_or_default().to_string(),
        video_profile: video["profile"].as_str().unwrap_or_default().to_string(),
        // ffprobe reports the level as an integer, already ×10.
        video_level: video["level"].as_u64().filter(|l| *l > 0).map(|l| l as u32),
        video_bitrate: video["bit_rate"].as_str().and_then(|s| s.parse().ok()),
        is_hdr: detect_hdr(
            video["color_transfer"].as_str().unwrap_or_default(),
            video["color_primaries"].as_str().unwrap_or_default(),
            &pix_fmt,
        ),
        pixel_format: pix_fmt,
        time_base: video["time_base"].as_str().unwrap_or_default().to_string(),
        rotation,
        has_audio: audio.is_some(),
        audio_codec: audio.and_then(|a| a["codec_name"].as_str()).map(str::to_string),
        audio_sample_rate: audio.and_then(|a| a["sample_rate"].as_str()).and_then(|s| s.parse().ok()),
        audio_channels: audio.and_then(|a| a["channels"].as_u64()).map(|c| c as u32),
        audio_bitrate: audio.and_then(|a| a["bit_rate"].as_str()).and_then(|s| s.parse().ok()),
        file_size: 0,
    })
}

/// The longest gap between keyframes in ffprobe's packet listing.
///
/// Lines look like `1.234000,K__` — the timestamp, then the flags, where `K`
/// marks a keyframe. Returns `None` when fewer than two keyframes were seen,
/// which says nothing either way and must not be held against the file.
pub fn max_keyframe_gap(csv: &str) -> Option<f64> {
    let mut keys: Vec<f64> = Vec::new();
    for line in csv.lines() {
        let mut parts = line.trim().split(',');
        let (Some(ts), Some(flags)) = (parts.next(), parts.next()) else { continue };
        if !flags.contains('K') {
            continue;
        }
        if let Ok(t) = ts.trim().parse::<f64>() {
            keys.push(t);
        }
    }
    if keys.len() < 2 {
        return None;
    }
    keys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    keys.windows(2).map(|w| w[1] - w[0]).fold(None, |m: Option<f64>, g| Some(m.map_or(g, |m| m.max(g))))
}

/// Measure the source's keyframe spacing over its opening seconds.
///
/// Errors are not failures: an unreadable index means "not measured", and the
/// planner treats that as no objection rather than inventing one.
pub fn probe_max_keyframe_gap(builder: &FfmpegCommandBuilder, path: &Path, window_secs: u32) -> Option<f64> {
    let args = builder.build_keyframe_probe_args(path, window_secs);
    let out = builder.probe_command(&args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    max_keyframe_gap(&String::from_utf8_lossy(&out.stdout))
}

/// Result of checking a source against the broadcast profile (§7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Compatibility {
    /// Byte-for-byte broadcastable; no re-encode needed.
    Compatible,
    /// Needs a normalized cache file. Reasons are shown in the UI.
    OptimizationRequired { reasons: Vec<String> },
}

impl Compatibility {
    pub fn is_compatible(&self) -> bool {
        matches!(self, Self::Compatible)
    }
    pub fn reasons(&self) -> &[String] {
        match self {
            Self::Compatible => &[],
            Self::OptimizationRequired { reasons } => reasons,
        }
    }
}

/// What a stream needs before it can join the broadcast playlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamPlan {
    /// The bitstream is already right; copy the packets.
    Copy,
    /// It has to be decoded and encoded again.
    Encode,
}

impl StreamPlan {
    pub fn is_copy(self) -> bool {
        matches!(self, Self::Copy)
    }
}

/// How one file will be turned into a cache entry.
///
/// Every file is still rewritten into our container — the timescale, the
/// whole-frame duration and faststart are what let the concat demuxer join
/// the results without timestamp repair — but rewriting a container is I/O,
/// not encoding, and costs a fraction of a second where an encode costs
/// minutes. The plan says which of the two streams actually needs the encoder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscodePlan {
    pub video: StreamPlan,
    pub audio: StreamPlan,
    /// Why each stream is being encoded, for the log and the UI.
    pub video_reasons: Vec<String>,
    pub audio_reasons: Vec<String>,
}

impl TranscodePlan {
    /// Both streams encoded — what every file used to get, unconditionally.
    pub fn full_encode() -> Self {
        Self {
            video: StreamPlan::Encode,
            audio: StreamPlan::Encode,
            video_reasons: Vec::new(),
            audio_reasons: Vec::new(),
        }
    }

    /// Neither stream encoded.
    pub fn remux() -> Self {
        Self {
            video: StreamPlan::Copy,
            audio: StreamPlan::Copy,
            video_reasons: Vec::new(),
            audio_reasons: Vec::new(),
        }
    }

    /// Neither stream is encoded: a pure remux.
    pub fn is_remux(&self) -> bool {
        self.video.is_copy() && self.audio.is_copy()
    }

    /// One stream is copied and the other encoded.
    pub fn is_partial(&self) -> bool {
        self.video.is_copy() != self.audio.is_copy()
    }

    /// A short word for the log. Never shown to a user (§1, §10).
    pub fn label(&self) -> &'static str {
        match (self.video, self.audio) {
            (StreamPlan::Copy, StreamPlan::Copy) => "remux",
            (StreamPlan::Copy, StreamPlan::Encode) => "audio-only",
            (StreamPlan::Encode, StreamPlan::Copy) => "video-only",
            (StreamPlan::Encode, StreamPlan::Encode) => "full-transcode",
        }
    }
}

/// What adding a file to the library actually costs.
///
/// One answer for the whole application, so the page that says "ready" and the
/// code that prepares the file cannot disagree about a given source. They did:
/// [`check_compatibility`] never looked at keyframe spacing while
/// [`plan_transcode`] did, so a file with a two-minute gap between keyframes
/// could be called ready at import and go into a concat manifest untouched.
///
/// Note what is *not* here: an outcome meaning "use the user's file as it is".
/// There is no such outcome, and `a_source_file_used_untouched_breaks_the_loop`
/// is why. Every entry in a broadcast manifest has been through the normalizer,
/// even when both its streams were copied packet for packet, because the cut to
/// a whole-frame boundary and the zeroed start timestamps are what let the
/// concat demuxer join one file to the next. A file that matches the profile in
/// every respect still does not match it in those two, and a library is not a
/// place to find that out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Readiness {
    /// Already prepared and cached; nothing to do.
    Cached,
    /// Needs a cache entry. The plan says how much of that is an encode —
    /// `remux` when the streams are fine and only the container is not.
    Prepare(TranscodePlan),
}

impl Readiness {
    /// The word the log uses, keyed `mode=` (§10).
    pub fn label(&self) -> &'static str {
        match self {
            Self::Cached => "cached",
            Self::Prepare(p) => p.label(),
        }
    }
}

/// Why the video pixels have to be decoded and encoded again.
///
/// Only things a bitstream copy cannot change belong here. The container's
/// timescale is deliberately absent: the normalizer sets it on the muxer, which
/// works just as well when the packets are copied.
pub fn video_encode_reasons(info: &MediaInfo, profile: OutputProfile) -> Vec<String> {
    let mut r = Vec::new();
    if info.video_codec != "h264" {
        r.push(format!("영상 코덱이 H.264가 아닙니다 ({})", info.video_codec));
    }
    if info.width != profile.width() || info.height != profile.height() {
        r.push(format!(
            "해상도가 {}x{}가 아닙니다 ({}x{})",
            profile.width(),
            profile.height(),
            info.width,
            info.height
        ));
    }
    if (info.fps - f64::from(profile.fps())).abs() > 0.01 {
        r.push(format!("프레임레이트가 {}fps가 아닙니다 ({:.2}fps)", profile.fps(), info.fps));
    }
    if info.pixel_format != "yuv420p" {
        r.push(format!("픽셀 포맷이 yuv420p가 아닙니다 ({})", info.pixel_format));
    }
    if info.is_hdr {
        r.push("HDR 영상입니다. SDR로 변환이 필요합니다".into());
    }
    if info.rotation != 0 {
        r.push(format!("회전 메타데이터가 있습니다 ({}도)", info.rotation));
    }
    if !profile.allows_h264_profile(&info.video_profile) {
        r.push(format!("H.264 프로파일이 지원 범위 밖입니다 ({})", info.video_profile));
    }
    if let Some(level) = info.video_level {
        if level > profile.max_h264_level() {
            r.push(format!(
                "H.264 레벨이 {}를 넘습니다 ({})",
                fmt_level(profile.max_h264_level()),
                fmt_level(level)
            ));
        }
    }
    r
}

/// H.264 levels are reported as 31 for 3.1, 42 for 4.2.
fn fmt_level(level: u32) -> String {
    format!("{}.{}", level / 10, level % 10)
}

/// Why the audio has to be decoded and encoded again.
pub fn audio_encode_reasons(info: &MediaInfo, profile: OutputProfile) -> Vec<String> {
    let mut r = Vec::new();
    match (&info.audio_codec, info.audio_sample_rate, info.audio_channels) {
        (Some(c), Some(sr), Some(ch))
            if c == "aac" && sr == profile.audio_sample_rate() && ch == profile.audio_channels() => {}
        (None, _, _) => r.push("오디오 트랙이 없습니다".into()),
        _ => r.push("오디오가 48kHz 스테레오 AAC가 아닙니다".into()),
    }
    // A stream far above the profile is re-encoded so the cache stays a
    // predictable size; a stream below it is left alone, because encoding it
    // again would only lose more.
    if r.is_empty() {
        if let Some(bps) = info.audio_bitrate {
            let ceiling = u64::from(profile.audio_kbps()) * 1000 * 2;
            if bps > ceiling {
                r.push(format!("오디오 비트레이트가 너무 높습니다 ({} kbps)", bps / 1000));
            }
        }
    }
    r
}

/// Decide what has to be re-encoded, and what can simply be copied (§7, §8).
///
/// `max_gop_secs` is the longest gap between keyframes measured in the source,
/// when it has been measured. A copied video keeps whatever keyframe spacing it
/// arrived with, and a very long gap makes for a poor live stream, so past a
/// limit the video is re-encoded to restore a regular one. `None` means it was
/// not measured and is not held against the file.
pub fn plan_transcode(info: &MediaInfo, profile: OutputProfile, max_gop_secs: Option<f64>) -> TranscodePlan {
    let mut video_reasons = video_encode_reasons(info, profile);
    if video_reasons.is_empty() {
        if let Some(gop) = max_gop_secs {
            let limit = profile.max_copy_gop_secs();
            if gop > limit {
                video_reasons.push(format!("키프레임 간격이 너무 깁니다 ({gop:.1}초 > {limit:.1}초)"));
            }
        }
    }
    let audio_reasons = audio_encode_reasons(info, profile);
    TranscodePlan {
        video: if video_reasons.is_empty() { StreamPlan::Copy } else { StreamPlan::Encode },
        audio: if audio_reasons.is_empty() { StreamPlan::Copy } else { StreamPlan::Encode },
        video_reasons,
        audio_reasons,
    }
}

/// Decide whether a file can be concatenated and stream-copied as-is (§7, §14).
///
/// The bar is deliberately high: the concat demuxer needs every input to agree
/// on codec, geometry, frame rate, pixel format, time base and audio layout. A
/// file that is merely "close" produces the timestamp faults §14 tests for, so
/// anything short of an exact match is sent to the normalizer.
///
/// This answers a different question from [`plan_transcode`]: here the file
/// would be used *exactly* as it is, so the container's own timescale counts
/// against it. A file that fails only on timescale still needs a cache entry,
/// and that entry costs a remux rather than an encode.
pub fn check_compatibility(info: &MediaInfo, profile: OutputProfile) -> Compatibility {
    let mut r = video_encode_reasons(info, profile);
    // A mismatched timescale is exactly what breaks concat stream copy.
    let want_tb = format!("1/{}", profile.video_timescale());
    if info.time_base != want_tb {
        r.push(format!("타임베이스가 {want_tb}가 아닙니다 ({})", info.time_base));
    }
    r.extend(audio_encode_reasons(info, profile));

    if r.is_empty() {
        Compatibility::Compatible
    } else {
        Compatibility::OptimizationRequired { reasons: r }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file already produced by our own normalizer, for the planner tests.
    fn ready() -> MediaInfo {
        MediaInfo {
            container: "mov,mp4,m4a".into(),
            duration_secs: 60.0,
            width: 1920,
            height: 1080,
            fps: 30.0,
            video_codec: "h264".into(),
            video_profile: "High".into(),
            video_level: Some(40),
            pixel_format: "yuv420p".into(),
            video_bitrate: Some(6_000_000),
            audio_codec: Some("aac".into()),
            audio_sample_rate: Some(48_000),
            audio_channels: Some(2),
            audio_bitrate: Some(192_000),
            time_base: "1/30000".into(),
            rotation: 0,
            is_hdr: false,
            file_size: 1,
            has_audio: true,
        }
    }

    #[test]
    fn a_broadcast_ready_file_is_not_re_encoded_at_all() {
        let p = plan_transcode(&ready(), OutputProfile::P1080p30, Some(2.0));
        assert!(p.is_remux(), "a conformant file must cost a remux, not an encode: {p:?}");
        assert_eq!(p.label(), "remux");
    }

    #[test]
    fn only_the_stream_that_is_wrong_gets_encoded() {
        // Audio wrong, picture right.
        let mut i = ready();
        i.audio_sample_rate = Some(44_100);
        let p = plan_transcode(&i, OutputProfile::P1080p30, Some(2.0));
        assert!(p.video.is_copy(), "the picture was already right");
        assert!(!p.audio.is_copy());
        assert!(p.is_partial());
        assert_eq!(p.label(), "audio-only");

        // Picture wrong, audio right.
        let mut i = ready();
        i.width = 1280;
        i.height = 720;
        let p = plan_transcode(&i, OutputProfile::P1080p30, Some(2.0));
        assert!(!p.video.is_copy());
        assert!(p.audio.is_copy(), "the audio was already right");
        assert_eq!(p.label(), "video-only");
    }

    #[test]
    fn a_file_that_is_wrong_everywhere_still_gets_a_full_encode() {
        let mut i = ready();
        i.video_codec = "vp9".into();
        i.audio_codec = Some("opus".into());
        let p = plan_transcode(&i, OutputProfile::P1080p30, None);
        assert_eq!(p.label(), "full-transcode");
        assert!(!p.video_reasons.is_empty() && !p.audio_reasons.is_empty());
    }

    #[test]
    fn a_wrong_timescale_costs_a_remux_and_not_an_encode() {
        // The muxer sets the timescale, and it does that for copied packets
        // too — so this must never be grounds for re-encoding the pixels.
        let mut i = ready();
        i.time_base = "1/90000".into();
        assert!(
            plan_transcode(&i, OutputProfile::P1080p30, Some(2.0)).is_remux(),
            "a timescale difference is a container change, not a picture change"
        );
        // It still means the original file cannot be used as-is.
        assert!(!check_compatibility(&i, OutputProfile::P1080p30).is_compatible());
    }

    #[test]
    fn a_long_gap_between_keyframes_forces_the_video_to_be_re_encoded() {
        let i = ready();
        let limit = OutputProfile::P1080p30.max_copy_gop_secs();
        assert!(plan_transcode(&i, OutputProfile::P1080p30, Some(limit - 0.1)).video.is_copy());
        let far = plan_transcode(&i, OutputProfile::P1080p30, Some(limit + 0.1));
        assert!(!far.video.is_copy(), "a 10-second keyframe gap must not be copied into a live stream");
        assert!(far.video_reasons.iter().any(|r| r.contains("키프레임")));
    }

    #[test]
    fn an_unmeasured_keyframe_gap_is_not_held_against_the_file() {
        assert!(plan_transcode(&ready(), OutputProfile::P1080p30, None).video.is_copy());
    }

    /// §3 A–D, as one table: what the user's file is, and what it costs.
    ///
    /// A file that matches the profile in every respect is case A, and case A
    /// is a remux — not "nothing". The container work is never skipped; see
    /// [`Readiness`] and `a_source_file_used_untouched_breaks_the_loop`.
    #[test]
    fn every_kind_of_mismatch_costs_only_what_it_has_to() {
        let profile = OutputProfile::P1080p30;
        let gop = Some(2.0);

        // A. both streams already right: copied, and only the container is
        // rewritten. No decode, no scale, no fps filter, no encoder.
        let p = plan_transcode(&ready(), profile, gop);
        assert_eq!(p.label(), "remux");
        assert!(p.video.is_copy() && p.audio.is_copy());

        // A'. a wrong container is still only a remux, never an encode.
        let mut i = ready();
        i.time_base = "1/90000".into();
        assert_eq!(plan_transcode(&i, profile, gop).label(), "remux");

        // B. video right, audio wrong.
        let mut i = ready();
        i.audio_sample_rate = Some(44_100);
        assert_eq!(plan_transcode(&i, profile, gop).label(), "audio-only");

        // C. video wrong, audio right.
        let mut i = ready();
        i.width = 1280;
        i.height = 720;
        assert_eq!(plan_transcode(&i, profile, gop).label(), "video-only");

        // D. both wrong.
        let mut i = ready();
        i.video_codec = "vp9".into();
        i.audio_codec = Some("opus".into());
        assert_eq!(plan_transcode(&i, profile, gop).label(), "full-transcode");
    }

    /// The hole the keyframe check closes. `check_compatibility` never looked
    /// at keyframe spacing, so a file with a two-minute gap read as ready.
    #[test]
    fn a_ready_looking_file_with_far_apart_keyframes_is_still_re_encoded() {
        let i = ready();
        let far = OutputProfile::P1080p30.max_copy_gop_secs() + 0.1;
        assert!(
            check_compatibility(&i, OutputProfile::P1080p30).is_compatible(),
            "the compatibility check alone sees nothing wrong with this file",
        );
        let p = plan_transcode(&i, OutputProfile::P1080p30, Some(far));
        assert!(!p.video.is_copy(), "a 10-second keyframe gap must not be copied into a live stream");
        assert_eq!(p.label(), "video-only");
    }

    #[test]
    fn a_silent_source_always_gets_an_encoded_audio_track() {
        let mut i = ready();
        i.has_audio = false;
        i.audio_codec = None;
        i.audio_sample_rate = None;
        i.audio_channels = None;
        let p = plan_transcode(&i, OutputProfile::P1080p30, Some(2.0));
        assert!(!p.audio.is_copy(), "there is no audio to copy; one has to be made");
    }

    #[test]
    fn an_exotic_h264_profile_is_re_encoded() {
        let mut i = ready();
        i.video_profile = "High 4:4:4 Predictive".into();
        assert!(!plan_transcode(&i, OutputProfile::P1080p30, Some(2.0)).video.is_copy());
    }

    #[test]
    fn a_level_above_the_profile_is_re_encoded_but_one_below_is_not() {
        let mut i = ready();
        i.video_level = Some(51);
        assert!(!plan_transcode(&i, OutputProfile::P1080p30, Some(2.0)).video.is_copy());
        i.video_level = Some(31);
        assert!(plan_transcode(&i, OutputProfile::P1080p30, Some(2.0)).video.is_copy());
    }

    #[test]
    fn keyframe_gaps_are_read_out_of_ffprobe_packet_output() {
        // Real shape: pts_time,flags — K marks a keyframe.
        let csv = "0.000000,K__\n0.033333,__\n2.000000,K__\n2.033333,__\n7.000000,K__\n";
        assert_eq!(max_keyframe_gap(csv), Some(5.0));
    }

    #[test]
    fn one_keyframe_alone_says_nothing_about_spacing() {
        assert_eq!(max_keyframe_gap("0.000000,K__\n0.033333,__\n"), None);
        assert_eq!(max_keyframe_gap(""), None);
        assert_eq!(max_keyframe_gap("garbage\nlines\n"), None);
    }

    /// A file already produced by our own normalizer.
    fn perfect() -> MediaInfo {
        MediaInfo {
            container: "mov,mp4,m4a".into(),
            duration_secs: 60.0,
            width: 1920,
            height: 1080,
            fps: 30.0,
            video_codec: "h264".into(),
            video_profile: "High".into(),
            video_level: Some(40),
            pixel_format: "yuv420p".into(),
            video_bitrate: Some(10_000_000),
            audio_codec: Some("aac".into()),
            audio_sample_rate: Some(48_000),
            audio_channels: Some(2),
            audio_bitrate: Some(192_000),
            time_base: "1/30000".into(),
            rotation: 0,
            is_hdr: false,
            file_size: 1,
            has_audio: true,
        }
    }

    #[test]
    fn rational_parsing() {
        assert_eq!(parse_rational("30/1"), Some(30.0));
        assert!((parse_rational("30000/1001").unwrap() - 29.97).abs() < 0.01);
        assert_eq!(parse_rational("0/0"), None);
        assert_eq!(parse_rational("garbage"), None);
        assert_eq!(parse_rational("25"), None);
    }

    #[test]
    fn hdr_detection_covers_pq_hlg_and_10bit() {
        assert!(detect_hdr("smpte2084", "bt2020", "yuv420p10le"));
        assert!(detect_hdr("arib-std-b67", "", ""), "HLG");
        assert!(detect_hdr("", "", "p010le"));
        assert!(!detect_hdr("bt709", "bt709", "yuv420p"));
        assert!(!detect_hdr("", "", "yuv420p"));
    }

    #[test]
    fn a_normalized_file_is_reported_compatible() {
        assert_eq!(check_compatibility(&perfect(), OutputProfile::P1080p30), Compatibility::Compatible);
    }

    #[test]
    fn each_mismatch_is_reported_with_a_reason() {
        type Mutate = Box<dyn Fn(&mut MediaInfo)>;
        let cases: Vec<(&str, Mutate)> = vec![
            ("codec", Box::new(|i: &mut MediaInfo| i.video_codec = "hevc".into())),
            (
                "resolution",
                Box::new(|i: &mut MediaInfo| {
                    i.width = 1280;
                    i.height = 720;
                }),
            ),
            ("fps", Box::new(|i: &mut MediaInfo| i.fps = 29.97)),
            ("pix_fmt", Box::new(|i: &mut MediaInfo| i.pixel_format = "yuv422p".into())),
            ("hdr", Box::new(|i: &mut MediaInfo| i.is_hdr = true)),
            ("rotation", Box::new(|i: &mut MediaInfo| i.rotation = 90)),
            ("timebase", Box::new(|i: &mut MediaInfo| i.time_base = "1/15360".into())),
            ("audio codec", Box::new(|i: &mut MediaInfo| i.audio_codec = Some("mp3".into()))),
            ("audio rate", Box::new(|i: &mut MediaInfo| i.audio_sample_rate = Some(44_100))),
            ("audio channels", Box::new(|i: &mut MediaInfo| i.audio_channels = Some(1))),
            (
                "no audio",
                Box::new(|i: &mut MediaInfo| {
                    i.audio_codec = None;
                    i.has_audio = false;
                }),
            ),
        ];
        for (label, mutate) in cases {
            let mut i = perfect();
            mutate(&mut i);
            let c = check_compatibility(&i, OutputProfile::P1080p30);
            assert!(!c.is_compatible(), "{label} should require optimization");
            assert_eq!(c.reasons().len(), 1, "{label} produced {:?}", c.reasons());
        }
    }

    #[test]
    fn a_1080p_file_is_not_compatible_with_the_720p_profile() {
        let c = check_compatibility(&perfect(), OutputProfile::P720p30);
        assert!(!c.is_compatible());
        assert!(c.reasons()[0].contains("1280x720"));
    }

    #[test]
    fn near_misses_are_still_rejected_because_concat_would_break() {
        // 29.97 vs 30 is precisely the case that produces DTS faults (§14).
        let mut i = perfect();
        i.fps = 29.97;
        assert!(!check_compatibility(&i, OutputProfile::P1080p30).is_compatible());
    }

    #[test]
    fn duration_snaps_to_whole_frames() {
        let mut i = perfect();
        i.duration_secs = 4.017;
        assert!((i.snapped_duration(OutputProfile::P1080p30) - 4.0).abs() < 1e-9);
        i.duration_secs = 12.4838;
        // 12.4838 * 30 = 374.5 -> floor 374 frames -> 12.4666s
        assert!((i.snapped_duration(OutputProfile::P1080p30) - 374.0 / 30.0).abs() < 1e-9);
        // Never longer than the source, or the last frame gets held at the seam.
        for d in [4.017, 12.4838, 59.999, 3600.5] {
            i.duration_secs = d;
            assert!(i.snapped_duration(OutputProfile::P1080p30) <= d, "snapped past the end of {d}");
        }
        // never zero, even for a degenerate source
        i.duration_secs = 0.0;
        assert!(i.snapped_duration(OutputProfile::P1080p30) > 0.0);
    }

    // --- JSON parsing ------------------------------------------------------

    const SAMPLE: &str = r#"{
      "streams": [
        {"codec_type":"video","codec_name":"h264","width":1920,"height":1080,
         "pix_fmt":"yuv420p","avg_frame_rate":"30/1","r_frame_rate":"30/1",
         "time_base":"1/30000","bit_rate":"9800000","color_transfer":"bt709",
         "color_primaries":"bt709",
         "side_data_list":[{"side_data_type":"Display Matrix","rotation":-90}]},
        {"codec_type":"audio","codec_name":"aac","sample_rate":"48000",
         "channels":2,"bit_rate":"192000"}
      ],
      "format": {"format_name":"mov,mp4,m4a,3gp,3g2,mj2","duration":"124.500000"}
    }"#;

    #[test]
    fn probe_json_is_parsed_into_media_info() {
        let i = parse_probe_json(SAMPLE).unwrap();
        assert_eq!(i.width, 1920);
        assert_eq!(i.height, 1080);
        assert_eq!(i.fps, 30.0);
        assert_eq!(i.video_codec, "h264");
        assert_eq!(i.pixel_format, "yuv420p");
        assert_eq!(i.duration_secs, 124.5);
        assert_eq!(i.time_base, "1/30000");
        assert_eq!(i.rotation, -90);
        assert_eq!(i.video_bitrate, Some(9_800_000));
        assert_eq!(i.audio_codec.as_deref(), Some("aac"));
        assert_eq!(i.audio_sample_rate, Some(48_000));
        assert_eq!(i.audio_channels, Some(2));
        assert!(i.has_audio);
        assert!(!i.is_hdr);
        assert!(i.container.starts_with("mov,mp4"));
    }

    #[test]
    fn a_file_without_audio_parses_and_is_flagged() {
        let json = r#"{"streams":[{"codec_type":"video","codec_name":"h264","width":1920,
            "height":1080,"pix_fmt":"yuv420p","avg_frame_rate":"30/1","time_base":"1/30000"}],
            "format":{"format_name":"mp4","duration":"10.0"}}"#;
        let i = parse_probe_json(json).unwrap();
        assert!(!i.has_audio);
        assert_eq!(i.audio_codec, None);
        assert!(!check_compatibility(&i, OutputProfile::P1080p30).is_compatible());
    }

    #[test]
    fn a_file_with_no_video_stream_is_an_error() {
        let json = r#"{"streams":[{"codec_type":"audio","codec_name":"aac"}],"format":{}}"#;
        assert_eq!(parse_probe_json(json).unwrap_err().code, ErrorCode::MediaNoVideoStream);
    }

    #[test]
    fn malformed_json_is_a_probe_error_not_a_panic() {
        assert_eq!(parse_probe_json("{not json").unwrap_err().code, ErrorCode::MediaProbeFailed);
    }

    #[test]
    fn zero_avg_frame_rate_falls_back_to_r_frame_rate() {
        let json = r#"{"streams":[{"codec_type":"video","codec_name":"h264","width":640,
            "height":480,"pix_fmt":"yuv420p","avg_frame_rate":"0/0","r_frame_rate":"25/1",
            "time_base":"1/12800"}],"format":{"duration":"3.0"}}"#;
        assert_eq!(parse_probe_json(json).unwrap().fps, 25.0);
    }

    #[test]
    fn hdr_source_is_detected_from_json() {
        let json = r#"{"streams":[{"codec_type":"video","codec_name":"hevc","width":3840,
            "height":2160,"pix_fmt":"yuv420p10le","avg_frame_rate":"30/1",
            "color_transfer":"smpte2084","color_primaries":"bt2020","time_base":"1/30000"}],
            "format":{"duration":"10.0"}}"#;
        let i = parse_probe_json(json).unwrap();
        assert!(i.is_hdr);
        let c = check_compatibility(&i, OutputProfile::P1080p30);
        assert!(c.reasons().iter().any(|r| r.contains("HDR")));
    }
}
