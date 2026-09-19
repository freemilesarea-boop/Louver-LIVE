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

/// Decide whether a file can be concatenated and stream-copied as-is (§7, §14).
///
/// The bar is deliberately high: the concat demuxer needs every input to agree
/// on codec, geometry, frame rate, pixel format, time base and audio layout. A
/// file that is merely "close" produces the timestamp faults §14 tests for, so
/// anything short of an exact match is sent to the normalizer.
pub fn check_compatibility(info: &MediaInfo, profile: OutputProfile) -> Compatibility {
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
    // A mismatched timescale is exactly what breaks concat stream copy.
    let want_tb = format!("1/{}", profile.video_timescale());
    if info.time_base != want_tb {
        r.push(format!("타임베이스가 {want_tb}가 아닙니다 ({})", info.time_base));
    }
    match (&info.audio_codec, info.audio_sample_rate, info.audio_channels) {
        (Some(c), Some(sr), Some(ch))
            if c == "aac" && sr == profile.audio_sample_rate() && ch == profile.audio_channels() => {}
        (None, _, _) => r.push("오디오 트랙이 없습니다".into()),
        _ => r.push("오디오가 48kHz 스테레오 AAC가 아닙니다".into()),
    }

    if r.is_empty() {
        Compatibility::Compatible
    } else {
        Compatibility::OptimizationRequired { reasons: r }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file already produced by our own normalizer.
    fn perfect() -> MediaInfo {
        MediaInfo {
            container: "mov,mp4,m4a".into(),
            duration_secs: 60.0,
            width: 1920,
            height: 1080,
            fps: 30.0,
            video_codec: "h264".into(),
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
