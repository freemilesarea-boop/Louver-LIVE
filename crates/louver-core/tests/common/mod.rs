//! Shared helpers for the media integration tests.
//!
//! Fixtures are generated with FFmpeg at test time (§52); no copyrighted media
//! is ever committed to the repository.

#![allow(dead_code)]

use louver_core::config::OutputProfile;
use louver_core::streaming::ffmpeg::{FfmpegCommandBuilder, FfmpegTools};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Locate FFmpeg, or `None` when the machine has none (the tests then skip).
pub fn tools() -> Option<FfmpegTools> {
    let sidecar = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apps/desktop/src-tauri/binaries");
    FfmpegTools::discover(Some(&sidecar)).ok()
}

/// Skip the test body when FFmpeg is unavailable, printing why.
#[macro_export]
macro_rules! require_ffmpeg {
    () => {
        match $crate::common::tools() {
            Some(t) => t,
            None => {
                eprintln!("SKIP: no ffmpeg/ffprobe available (sidecar or PATH)");
                return;
            }
        }
    };
}

pub fn builder(t: FfmpegTools, p: OutputProfile) -> FfmpegCommandBuilder {
    FfmpegCommandBuilder::new(t, p)
}

/// A deliberately non-conforming source file, so normalization has real work
/// to do (§52: different colours and tones make file boundaries visible).
pub struct FixtureSpec {
    pub name: &'static str,
    pub duration: f64,
    pub size: &'static str,
    pub fps: u32,
    pub sample_rate: u32,
    pub channels: u32,
    pub tone_hz: u32,
    pub pattern: &'static str,
}

/// Three mutually different sources, matching the §52 fixture description.
pub fn default_fixtures() -> Vec<FixtureSpec> {
    vec![
        FixtureSpec {
            name: "video_a",
            duration: 10.0,
            size: "1280x720",
            fps: 25,
            sample_rate: 44100,
            channels: 2,
            tone_hz: 440,
            pattern: "testsrc2",
        },
        FixtureSpec {
            name: "video_b",
            duration: 12.0,
            size: "640x480",
            fps: 24,
            sample_rate: 48000,
            channels: 1,
            tone_hz: 660,
            pattern: "smptebars",
        },
        FixtureSpec {
            name: "video_c",
            duration: 8.0,
            size: "1920x1080",
            fps: 60,
            sample_rate: 32000,
            channels: 2,
            tone_hz: 880,
            pattern: "testsrc",
        },
    ]
}

/// Short fixtures for the loop-order test (§54).
pub fn loop_fixtures() -> Vec<FixtureSpec> {
    vec![
        FixtureSpec {
            name: "loop_a",
            duration: 2.0,
            size: "640x360",
            fps: 30,
            sample_rate: 48000,
            channels: 2,
            tone_hz: 300,
            pattern: "color=red",
        },
        FixtureSpec {
            name: "loop_b",
            duration: 2.0,
            size: "640x360",
            fps: 30,
            sample_rate: 48000,
            channels: 2,
            tone_hz: 600,
            pattern: "color=green",
        },
        FixtureSpec {
            name: "loop_c",
            duration: 2.0,
            size: "640x360",
            fps: 30,
            sample_rate: 48000,
            channels: 2,
            tone_hz: 900,
            pattern: "color=blue",
        },
    ]
}

/// Generate one fixture with FFmpeg. Returns the path.
///
/// Written to a private temporary name and renamed into place, so the file at
/// the shared path is either absent or complete and never half-written. The
/// obvious version — check `is_file()`, then have FFmpeg write straight to the
/// shared path — is a race between test binaries, which cargo runs in
/// parallel: one starts encoding `video_c.mp4`, another sees the file exists
/// and probes it, and gets `moov atom not found`, because the moov atom is
/// written last. It failed on Windows, where the encode is slow enough to lose,
/// and nothing but timing kept it passing elsewhere.
pub fn make_fixture(tools: &FfmpegTools, dir: &Path, s: &FixtureSpec) -> PathBuf {
    let out = dir.join(format!("{}.mp4", s.name));
    if out.is_file() {
        return out;
    }
    let tmp = dir.join(format!(
        // `.mp4` stays last: FFmpeg picks the muxer from the extension, and
        // a name ending in `.part` makes it refuse to open the file at all.
        ".{}-{}-{}.part.mp4",
        s.name,
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    ));
    let video_src = if s.pattern.starts_with("color=") {
        format!("{}:size={}:rate={}:duration={}", s.pattern, s.size, s.fps, s.duration)
    } else {
        format!("{}=size={}:rate={}:duration={}", s.pattern, s.size, s.fps, s.duration)
    };
    let status = Command::new(&tools.ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            &video_src,
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency={}:sample_rate={}:duration={}", s.tone_hz, s.sample_rate, s.duration),
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-ar",
            &s.sample_rate.to_string(),
            "-ac",
            &s.channels.to_string(),
            "-shortest",
        ])
        .arg(&tmp)
        .status()
        .expect("failed to run ffmpeg for fixture generation");
    assert!(status.success(), "fixture generation failed for {}", s.name);

    // Whoever gets there first wins; the loser throws its copy away rather
    // than replacing a file another test may already be reading. (Windows
    // refuses a rename onto an existing file, which is the same outcome.)
    if out.is_file() || std::fs::rename(&tmp, &out).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    assert!(out.is_file(), "fixture {} was not created", s.name);
    out
}

pub fn make_fixtures(tools: &FfmpegTools, dir: &Path, specs: &[FixtureSpec]) -> Vec<PathBuf> {
    std::fs::create_dir_all(dir).unwrap();
    specs.iter().map(|s| make_fixture(tools, dir, s)).collect()
}

// --- probing helpers ------------------------------------------------------

pub fn ffprobe_value(tools: &FfmpegTools, args: &[&str]) -> String {
    let out = Command::new(&tools.ffprobe).args(args).output().expect("ffprobe failed to run");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

pub fn stream_field(tools: &FfmpegTools, file: &Path, stream: &str, field: &str) -> String {
    ffprobe_value(
        tools,
        &[
            "-v",
            "error",
            "-select_streams",
            stream,
            "-show_entries",
            &format!("stream={field}"),
            "-of",
            "default=nw=1:nk=1",
            &file.to_string_lossy(),
        ],
    )
    .lines()
    .next()
    .unwrap_or_default()
    .to_string()
}

pub fn format_duration(tools: &FfmpegTools, file: &Path) -> f64 {
    ffprobe_value(
        tools,
        &[
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=nw=1:nk=1",
            &file.to_string_lossy(),
        ],
    )
    .parse()
    .unwrap_or(0.0)
}

/// All packet presentation timestamps of a stream, sorted.
///
/// Sorting matters: with B-frames, packet order is not display order, so an
/// unsorted list looks full of gaps that do not exist.
pub fn sorted_pts(tools: &FfmpegTools, file: &Path, stream: &str) -> Vec<f64> {
    let raw = ffprobe_value(
        tools,
        &[
            "-v",
            "error",
            "-select_streams",
            stream,
            "-show_entries",
            "packet=pts_time",
            "-of",
            "csv=p=0",
            &file.to_string_lossy(),
        ],
    );
    let mut v: Vec<f64> =
        raw.lines().filter_map(|l| l.trim().trim_end_matches(',').parse::<f64>().ok()).collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v
}

/// Run FFmpeg and return (exit code, stderr).
pub fn run_ffmpeg(tools: &FfmpegTools, args: &[String]) -> (i32, String) {
    let out = Command::new(&tools.ffmpeg).args(args).output().expect("ffmpeg failed to run");
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).to_string())
}

/// Lines of FFmpeg stderr that indicate a real timestamp or stream fault (§14).
pub fn timestamp_faults(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter(|l| {
            let l = l.to_lowercase();
            l.contains("non-monotonic")
                || l.contains("dts")
                || l.contains("invalid timestamp")
                || l.contains("timestamp discontinuity")
                || l.contains("error")
                || l.contains("corrupt")
        })
        .map(|s| s.to_string())
        .collect()
}
