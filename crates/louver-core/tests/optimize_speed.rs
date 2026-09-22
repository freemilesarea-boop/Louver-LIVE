//! What optimizing actually costs, measured rather than asserted (§1, §11).
//!
//! Three samples stand for the three cases a user's library contains:
//!
//!   A  already broadcast-ready            → nothing to encode
//!   B  right picture, wrong audio         → one stream to encode
//!   C  wrong everything                   → both streams to encode
//!
//! Run with `cargo test -p louver-core --test optimize_speed -- --ignored
//! --nocapture` to print the table. Ignored by default because it encodes real
//! video and takes minutes on a slow machine.
mod common;

use louver_core::config::OutputProfile;
use louver_core::media::cache::MediaCache;
use louver_core::media::normalize::{normalize_one, CancelToken};
use louver_core::media::probe::{plan_transcode, probe, probe_max_keyframe_gap, TranscodePlan};
use louver_core::streaming::ffmpeg::{FfmpegCommandBuilder, FfmpegTools};
use std::path::{Path, PathBuf};
use std::process::Command;

const PROFILE: OutputProfile = OutputProfile::P1080p30;

fn tools() -> FfmpegTools {
    FfmpegTools::new("ffmpeg", "ffprobe")
}

fn builder() -> FfmpegCommandBuilder {
    let t = tools();
    let enc = t.detect_encoder();
    FfmpegCommandBuilder::new(t, PROFILE).with_encoder(&enc)
}

/// A file that already matches the broadcast profile in every respect.
fn sample_a(dir: &Path, secs: u32) -> PathBuf {
    let out = dir.join("a_conformant.mp4");
    run(&[
        "-y",
        "-f",
        "lavfi",
        "-i",
        &format!("testsrc2=size=1920x1080:rate=30:duration={secs}"),
        "-f",
        "lavfi",
        "-i",
        &format!("sine=frequency=440:sample_rate=48000:duration={secs}"),
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-profile:v",
        "high",
        "-level",
        "4.0",
        "-pix_fmt",
        "yuv420p",
        "-g",
        "60",
        "-keyint_min",
        "60",
        "-sc_threshold",
        "0",
        "-b:v",
        "6000k",
        "-c:a",
        "aac",
        "-b:a",
        "192k",
        "-ar",
        "48000",
        "-ac",
        "2",
        "-video_track_timescale",
        "30000",
        "-movflags",
        "+faststart",
        out.to_str().unwrap(),
    ]);
    out
}

/// Conformant picture, but the audio is 44.1 kHz MP3.
fn sample_b(dir: &Path, secs: u32) -> PathBuf {
    let out = dir.join("b_audio_only.mp4");
    run(&[
        "-y",
        "-f",
        "lavfi",
        "-i",
        &format!("testsrc2=size=1920x1080:rate=30:duration={secs}"),
        "-f",
        "lavfi",
        "-i",
        &format!("sine=frequency=440:sample_rate=44100:duration={secs}"),
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-profile:v",
        "high",
        "-level",
        "4.0",
        "-pix_fmt",
        "yuv420p",
        "-g",
        "60",
        "-keyint_min",
        "60",
        "-sc_threshold",
        "0",
        "-b:v",
        "6000k",
        "-c:a",
        "libmp3lame",
        "-ar",
        "44100",
        "-ac",
        "2",
        "-video_track_timescale",
        "30000",
        "-movflags",
        "+faststart",
        out.to_str().unwrap(),
    ]);
    out
}

/// Wrong resolution, wrong frame rate, wrong audio layout.
fn sample_c(dir: &Path, secs: u32) -> PathBuf {
    let out = dir.join("c_full.mp4");
    run(&[
        "-y",
        "-f",
        "lavfi",
        "-i",
        &format!("testsrc2=size=1280x720:rate=24:duration={secs}"),
        "-f",
        "lavfi",
        "-i",
        &format!("sine=frequency=440:sample_rate=44100:duration={secs}"),
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-pix_fmt",
        "yuv420p",
        "-b:v",
        "3000k",
        "-c:a",
        "aac",
        "-ar",
        "44100",
        "-ac",
        "1",
        "-movflags",
        "+faststart",
        out.to_str().unwrap(),
    ]);
    out
}

fn run(args: &[&str]) {
    let out = Command::new("ffmpeg").args(args).output().expect("ffmpeg must be on PATH");
    assert!(out.status.success(), "fixture build failed: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
#[ignore = "encodes real video; run explicitly"]
fn measure_each_case() {
    let dir = tempfile::tempdir().unwrap();
    let secs = 60;
    let b = builder();
    println!("\nencoder selected by probe: {}\n", b.encoder());
    println!("{:<26} {:>8} {:>9} {:>8} {:>9} {:>10}", "sample", "dur(s)", "took(s)", "speed", "size", "mode");

    for (name, path) in [
        ("A already conformant", sample_a(dir.path(), secs)),
        ("B audio only", sample_b(dir.path(), secs)),
        ("C full transcode", sample_c(dir.path(), secs)),
    ] {
        let cache = MediaCache::new(dir.path().join(format!("cache-{}", name.len())));
        let info = probe(&b, &path).expect("probe");
        let out = normalize_one(&b, &cache, &path, name, &info, PROFILE, &CancelToken::new(), |_| {})
            .expect("normalize");
        println!(
            "{:<26} {:>8.1} {:>9.2} {:>7.1}x {:>9} {:>10}",
            name,
            out.duration_secs,
            out.elapsed_secs,
            out.speed_x,
            louver_core::system::format_bytes(out.bytes),
            out.plan.label(),
        );
        if !out.plan.video_reasons.is_empty() {
            println!("      video: {}", out.plan.video_reasons.join("; "));
        }
        if !out.plan.audio_reasons.is_empty() {
            println!("      audio: {}", out.plan.audio_reasons.join("; "));
        }
    }
}

/// The same files through the argv this release replaced: both streams
/// encoded, every time, whatever the source already was.
#[test]
#[ignore = "encodes real video; run explicitly"]
fn before_and_after() {
    let dir = tempfile::tempdir().unwrap();
    let secs = 60;
    let b = builder();
    println!("\n{:<26} {:>10} {:>10} {:>9}  new mode", "sample", "old(s)", "new(s)", "faster");

    for (name, path) in [
        ("A already conformant", sample_a(dir.path(), secs)),
        ("B audio only", sample_b(dir.path(), secs)),
        ("C full transcode", sample_c(dir.path(), secs)),
    ] {
        let info = probe(&b, &path).expect("probe");
        let target = info.snapped_duration(PROFILE);

        let old_args = b.build_normalize_args(
            &path,
            &dir.path().join("old.mp4"),
            target,
            info.has_audio,
            &TranscodePlan::full_encode(),
        );
        let old = time_ffmpeg(&b, &old_args);

        let plan = plan_transcode(&info, PROFILE, probe_max_keyframe_gap(&b, &path, 60));
        let new_args =
            b.build_normalize_args(&path, &dir.path().join("new.mp4"), target, info.has_audio, &plan);
        let new = time_ffmpeg(&b, &new_args);

        println!(
            "{:<26} {:>9.2}s {:>9.2}s {:>8.1}x  {}",
            name,
            old,
            new,
            if new > 0.0 { old / new } else { f64::INFINITY },
            plan.label()
        );
    }
}

fn time_ffmpeg(b: &FfmpegCommandBuilder, args: &[String]) -> f64 {
    let t = std::time::Instant::now();
    let out = b.command(args).output().expect("spawn ffmpeg");
    assert!(out.status.success(), "ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr));
    t.elapsed().as_secs_f64()
}
