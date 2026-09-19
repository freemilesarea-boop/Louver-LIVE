//! Media pipeline integration tests (§14, §53, §54, §74).
//!
//! These are the tests that decide the product's architecture. §74 asks for one
//! thing to be proven before anything else is built:
//!
//! > Can several different videos, normalized to one profile, be broadcast
//! > sequentially and repeatedly from a single continuous FFmpeg RTMP session
//! > without re-encoding?
//!
//! Everything here runs real FFmpeg against real generated media. When FFmpeg
//! is unavailable the tests skip loudly rather than passing silently.

mod common;

use common::*;
use louver_core::config::{OutputProfile, StreamMode};
use louver_core::media::cache::MediaCache;
use louver_core::media::normalize::{normalize_one, CancelToken};
use louver_core::media::probe::{check_compatibility, probe};
use louver_core::streaming::manifest::write_manifest;
use std::path::PathBuf;

const PROFILE: OutputProfile = OutputProfile::P720p30;

/// Fixtures are cached between runs so the suite stays fast.
fn fixture_dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    std::fs::create_dir_all(&d).unwrap();
    d
}

// ---------------------------------------------------------------------------
// §53 — the full pipeline: raw -> normalize -> manifest -> concat -> copy
// ---------------------------------------------------------------------------

#[test]
fn full_pipeline_normalizes_concatenates_and_stream_copies() {
    let tools = require_ffmpeg!();
    let b = builder(tools.clone(), PROFILE);
    let work = tempfile::tempdir().unwrap();
    let cache = MediaCache::new(work.path().join("cache"));

    let specs = default_fixtures();
    let sources = make_fixtures(&tools, &fixture_dir(), &specs);

    // The sources must genuinely differ, or the test proves nothing (§52).
    let mut geometries = std::collections::HashSet::new();
    for s in &sources {
        geometries.insert((
            stream_field(&tools, s, "v:0", "width"),
            stream_field(&tools, s, "v:0", "height"),
            stream_field(&tools, s, "v:0", "r_frame_rate"),
        ));
    }
    assert_eq!(geometries.len(), 3, "fixtures should be mutually different");

    // --- normalize ---------------------------------------------------------
    let mut normalized = Vec::new();
    let mut expected_total = 0.0;
    for (i, src) in sources.iter().enumerate() {
        let info = probe(&b, src).expect("probe failed");
        assert!(
            !check_compatibility(&info, PROFILE).is_compatible(),
            "fixture {i} was already compatible; it would not exercise the normalizer"
        );
        let out = normalize_one(
            &b, &cache, src, &format!("fixture{i}"), &info, PROFILE, &CancelToken::new(), |_| {},
        )
        .expect("normalize failed");
        assert!(!out.from_cache);
        expected_total += out.duration_secs;
        normalized.push(out.output_path);
    }

    // --- every normalized file must be byte-compatible with the others -----
    let mut signatures = std::collections::HashSet::new();
    for n in &normalized {
        let info = probe(&b, n).expect("probe of normalized file failed");
        assert!(
            check_compatibility(&info, PROFILE).is_compatible(),
            "normalizer produced a non-conforming file: {:?}",
            check_compatibility(&info, PROFILE).reasons()
        );
        signatures.insert(format!(
            "{}|{}x{}|{}|{}|{}|{}|{}",
            info.video_codec, info.width, info.height, info.fps, info.pixel_format,
            info.time_base,
            info.audio_codec.clone().unwrap_or_default(),
            info.audio_sample_rate.unwrap_or(0),
        ));
    }
    assert_eq!(
        signatures.len(), 1,
        "normalized files disagree on codec/geometry/timebase, so concat cannot stream-copy: {signatures:?}"
    );

    // --- manifest + concat + STREAM COPY -----------------------------------
    let manifest = work.path().join("manifest.txt");
    write_manifest(&manifest, &normalized).unwrap();

    let out = work.path().join("output.flv");
    let args = b.build_dry_run_args(&manifest, &out, StreamMode::StreamCopy, None, false);

    // §41: the live argv must contain no video encoder whatsoever.
    for forbidden in ["libx264", "h264_nvenc", "h264_qsv", "h264_amf", "h264_videotoolbox"] {
        assert!(!args.iter().any(|a| a == forbidden), "stream copy invoked an encoder");
    }

    let (code, stderr) = run_ffmpeg(&tools, &args);
    assert_eq!(code, 0, "ffmpeg exited {code}\n{stderr}");

    let faults = timestamp_faults(&stderr);
    assert!(faults.is_empty(), "concat stream copy reported faults (§14):\n{}", faults.join("\n"));

    // --- verify the output (§53) -------------------------------------------
    assert_eq!(stream_field(&tools, &out, "v:0", "codec_name"), "h264");
    assert_eq!(stream_field(&tools, &out, "a:0", "codec_name"), "aac");
    assert_eq!(stream_field(&tools, &out, "v:0", "width"), PROFILE.width().to_string());
    assert_eq!(stream_field(&tools, &out, "v:0", "height"), PROFILE.height().to_string());
    assert_eq!(stream_field(&tools, &out, "v:0", "r_frame_rate"), "30/1");

    let dur = format_duration(&tools, &out);
    assert!(
        (dur - expected_total).abs() < 0.5,
        "output is {dur:.3}s but the playlist totals {expected_total:.3}s"
    );

    // Frame count must match the duration exactly: nothing was dropped.
    let v = sorted_pts(&tools, &out, "v:0");
    let expected_frames = (expected_total * 30.0).round() as usize;
    assert_eq!(v.len(), expected_frames, "expected {expected_frames} frames, got {}", v.len());
}

// ---------------------------------------------------------------------------
// §54 — loop test: A B C A B C A B C
// ---------------------------------------------------------------------------

#[test]
fn infinite_loop_repeats_the_playlist_in_order() {
    let tools = require_ffmpeg!();
    let b = builder(tools.clone(), PROFILE);
    let work = tempfile::tempdir().unwrap();
    let cache = MediaCache::new(work.path().join("cache"));

    let specs = loop_fixtures();
    let sources = make_fixtures(&tools, &fixture_dir(), &specs);

    let mut normalized = Vec::new();
    let mut cycle = 0.0;
    for (i, src) in sources.iter().enumerate() {
        let info = probe(&b, src).unwrap();
        let out = normalize_one(
            &b, &cache, src, &format!("loop{i}"), &info, PROFILE, &CancelToken::new(), |_| {},
        )
        .unwrap();
        cycle += out.duration_secs;
        normalized.push(out.output_path);
    }
    assert!((cycle - 6.0).abs() < 0.1, "the cycle should be ~6s, got {cycle}");

    let manifest = work.path().join("m.txt");
    write_manifest(&manifest, &normalized).unwrap();

    // 18 seconds = three full cycles.
    let out = work.path().join("loop.flv");
    let args = b.build_dry_run_args(&manifest, &out, StreamMode::StreamCopy, Some(18.0), true);
    assert!(args.windows(2).any(|w| w == ["-stream_loop", "-1"]), "loop flag missing");

    let (code, stderr) = run_ffmpeg(&tools, &args);
    assert_eq!(code, 0, "ffmpeg exited {code}\n{stderr}");
    assert!(timestamp_faults(&stderr).is_empty(), "loop produced faults:\n{stderr}");

    let dur = format_duration(&tools, &out);
    assert!((dur - 18.0).abs() < 0.5, "expected ~18s of looped output, got {dur:.3}");

    // Three cycles' worth of frames, continuous, with no duplicated timestamps.
    let v = sorted_pts(&tools, &out, "v:0");
    assert!(v.len() >= 530, "expected ~540 frames for 18s at 30fps, got {}", v.len());
    assert_eq!(v.len(), v.iter().map(|f| (f * 1000.0) as i64).collect::<std::collections::HashSet<_>>().len(),
        "duplicate video timestamps in the looped output");

    // §54 asks the order to be verified by the file boundaries. The fixtures
    // are solid red/green/blue, so the average frame colour identifies which
    // clip is playing; sample the middle of each expected slot.
    let order: Vec<char> = (0..9)
        .map(|slot| {
            let t = slot as f64 * 2.0 + 1.0; // middle of each 2s clip
            dominant_colour(&tools, &out, t)
        })
        .collect();
    assert_eq!(
        order,
        vec!['R', 'G', 'B', 'R', 'G', 'B', 'R', 'G', 'B'],
        "loop did not play A B C A B C A B C"
    );
}

/// Sample one frame at `t` seconds and classify its dominant colour.
fn dominant_colour(tools: &louver_core::streaming::ffmpeg::FfmpegTools, file: &std::path::Path, t: f64) -> char {
    let out = std::process::Command::new(&tools.ffmpeg)
        .args([
            "-hide_banner", "-loglevel", "error",
            "-ss", &format!("{t:.3}"), "-i", &file.to_string_lossy(),
            "-frames:v", "1", "-vf", "scale=1:1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-",
        ])
        .output()
        .expect("colour sample failed");
    let px = out.stdout;
    assert!(px.len() >= 3, "no pixel returned at t={t}");
    let (r, g, bl) = (px[0], px[1], px[2]);
    if r >= g && r >= bl { 'R' } else if g >= bl { 'G' } else { 'B' }
}

// ---------------------------------------------------------------------------
// §14 — stream-copy compatibility matrix
// ---------------------------------------------------------------------------

/// Case A: three copies of one identical 1080p30 source.
#[test]
fn concat_case_a_identical_sources() {
    let tools = require_ffmpeg!();
    let b = builder(tools.clone(), PROFILE);
    let work = tempfile::tempdir().unwrap();
    let cache = MediaCache::new(work.path().join("cache"));

    let src = make_fixture(
        &tools,
        &fixture_dir(),
        &FixtureSpec { name: "case_a", duration: 4.0, size: "1280x720", fps: 30, sample_rate: 48000, channels: 2, tone_hz: 440, pattern: "testsrc2" },
    );
    let info = probe(&b, &src).unwrap();
    let n = normalize_one(&b, &cache, &src, "case_a", &info, PROFILE, &CancelToken::new(), |_| {}).unwrap();

    let files = vec![n.output_path.clone(), n.output_path.clone(), n.output_path.clone()];
    let manifest = work.path().join("a.txt");
    write_manifest(&manifest, &files).unwrap();

    let out = work.path().join("a.flv");
    let (code, stderr) = run_ffmpeg(&tools, &b.build_dry_run_args(&manifest, &out, StreamMode::StreamCopy, None, false));
    assert_eq!(code, 0, "{stderr}");
    assert!(timestamp_faults(&stderr).is_empty(), "case A faults:\n{stderr}");
    assert!((format_duration(&tools, &out) - n.duration_secs * 3.0).abs() < 0.3);
}

/// Case B + C: different sources, and different durations, normalized together.
#[test]
fn concat_case_b_and_c_different_sources_and_durations() {
    let tools = require_ffmpeg!();
    let b = builder(tools.clone(), PROFILE);
    let work = tempfile::tempdir().unwrap();
    let cache = MediaCache::new(work.path().join("cache"));

    // Durations are deliberately unequal and not whole seconds.
    let specs = vec![
        FixtureSpec { name: "case_b1", duration: 3.4,  size: "854x480",   fps: 25, sample_rate: 44100, channels: 2, tone_hz: 300, pattern: "testsrc2" },
        FixtureSpec { name: "case_b2", duration: 5.75, size: "1920x1080", fps: 50, sample_rate: 22050, channels: 1, tone_hz: 500, pattern: "smptebars" },
        FixtureSpec { name: "case_b3", duration: 2.2,  size: "640x360",   fps: 15, sample_rate: 48000, channels: 2, tone_hz: 700, pattern: "testsrc" },
    ];
    let sources = make_fixtures(&tools, &fixture_dir(), &specs);

    let mut files = Vec::new();
    let mut total = 0.0;
    for (i, s) in sources.iter().enumerate() {
        let info = probe(&b, s).unwrap();
        let n = normalize_one(&b, &cache, s, &format!("caseb{i}"), &info, PROFILE, &CancelToken::new(), |_| {}).unwrap();
        total += n.duration_secs;
        files.push(n.output_path);
    }

    let manifest = work.path().join("b.txt");
    write_manifest(&manifest, &files).unwrap();
    let out = work.path().join("b.flv");
    let (code, stderr) = run_ffmpeg(&tools, &b.build_dry_run_args(&manifest, &out, StreamMode::StreamCopy, None, false));

    assert_eq!(code, 0, "{stderr}");
    assert!(timestamp_faults(&stderr).is_empty(), "case B/C faults:\n{stderr}");
    assert!((format_duration(&tools, &out) - total).abs() < 0.3, "duration drift");

    // No black-frame gap or stall at the seams: frame spacing stays ~33ms.
    let v = sorted_pts(&tools, &out, "v:0");
    let big_gaps: Vec<f64> = v.windows(2).map(|w| w[1] - w[0]).filter(|d| *d > 0.100).collect();
    assert!(big_gaps.is_empty(), "video stalled at a boundary: {big_gaps:?}");
}

/// Case D: a source whose audio and video start at different timestamps.
#[test]
fn concat_case_d_skewed_timestamps_are_repaired_by_normalization() {
    let tools = require_ffmpeg!();
    let b = builder(tools.clone(), PROFILE);
    let work = tempfile::tempdir().unwrap();
    let cache = MediaCache::new(work.path().join("cache"));
    let dir = fixture_dir();

    // Build a file whose audio lags the video by 700ms and whose video starts
    // at a non-zero PTS — the kind of file that breaks naive concatenation.
    let skewed = dir.join("case_d.mp4");
    if !skewed.is_file() {
        let st = std::process::Command::new(&tools.ffmpeg)
            .args([
                "-hide_banner", "-loglevel", "error", "-y",
                "-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=30:duration=5",
                "-itsoffset", "0.7", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100:duration=5",
                "-map", "0:v", "-map", "1:a",
                "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
                "-c:a", "aac", "-muxdelay", "0.9",
            ])
            .arg(&skewed)
            .status()
            .unwrap();
        assert!(st.success());
    }

    let normal = make_fixture(
        &tools, &dir,
        &FixtureSpec { name: "case_d2", duration: 4.0, size: "1280x720", fps: 30, sample_rate: 48000, channels: 2, tone_hz: 880, pattern: "smptebars" },
    );

    let mut files = Vec::new();
    let mut total = 0.0;
    for (i, s) in [skewed.clone(), normal].iter().enumerate() {
        let info = probe(&b, s).unwrap();
        let n = normalize_one(&b, &cache, s, &format!("cased{i}"), &info, PROFILE, &CancelToken::new(), |_| {}).unwrap();
        total += n.duration_secs;
        files.push(n.output_path);
    }

    let manifest = work.path().join("d.txt");
    write_manifest(&manifest, &files).unwrap();
    let out = work.path().join("d.flv");
    let (code, stderr) = run_ffmpeg(&tools, &b.build_dry_run_args(&manifest, &out, StreamMode::StreamCopy, None, false));

    assert_eq!(code, 0, "{stderr}");
    assert!(
        timestamp_faults(&stderr).is_empty(),
        "normalization failed to repair a skewed source (§14 case D):\n{stderr}"
    );
    assert!((format_duration(&tools, &out) - total).abs() < 0.4);

    // Both streams must start together after normalization.
    let v = sorted_pts(&tools, &out, "v:0");
    let a = sorted_pts(&tools, &out, "a:0");
    assert!((v[0] - a[0]).abs() < 0.15, "A/V still skewed at the start: v={} a={}", v[0], a[0]);
}

// ---------------------------------------------------------------------------
// §74 — the load-bearing claim, measured rather than asserted
// ---------------------------------------------------------------------------

/// Long-ish looped stream copy, checking that A/V sync does not drift.
///
/// This is the property a 24-hour broadcast depends on: if each loop seam added
/// even 30ms of skew, a day of looping would end up seconds out of sync.
#[test]
fn looped_stream_copy_does_not_accumulate_av_drift() {
    let tools = require_ffmpeg!();
    let b = builder(tools.clone(), PROFILE);
    let work = tempfile::tempdir().unwrap();
    let cache = MediaCache::new(work.path().join("cache"));

    let specs = default_fixtures();
    let sources = make_fixtures(&tools, &fixture_dir(), &specs);
    let mut files = Vec::new();
    let mut cycle = 0.0;
    for (i, s) in sources.iter().enumerate() {
        let info = probe(&b, s).unwrap();
        let n = normalize_one(&b, &cache, s, &format!("fixture{i}"), &info, PROFILE, &CancelToken::new(), |_| {}).unwrap();
        cycle += n.duration_secs;
        files.push(n.output_path);
    }

    let manifest = work.path().join("drift.txt");
    write_manifest(&manifest, &files).unwrap();

    // Ten cycles, i.e. 30 file boundaries.
    let seconds = cycle * 10.0;
    let out = work.path().join("drift.flv");
    let (code, stderr) = run_ffmpeg(
        &tools,
        &b.build_dry_run_args(&manifest, &out, StreamMode::StreamCopy, Some(seconds), true),
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(timestamp_faults(&stderr).is_empty(), "faults during a 10-cycle run:\n{stderr}");

    let v = sorted_pts(&tools, &out, "v:0");
    let a = sorted_pts(&tools, &out, "a:0");
    assert!(!v.is_empty() && !a.is_empty());

    // Compare the audio/video skew early and late. If concatenation accumulated
    // error, the late skew would be visibly larger than the early one.
    let skew_at = |frac: f64| -> f64 {
        let t = v[((v.len() - 1) as f64 * frac) as usize];
        let j = a.partition_point(|x| *x < t).min(a.len() - 1);
        (a[j] - t).abs()
    };
    let (early, late) = (skew_at(0.05), skew_at(0.95));
    assert!(early < 0.100, "unexpected skew at the start: {early:.3}s");
    assert!(late < 0.100, "A/V drifted to {late:.3}s after {seconds:.0}s of looping");
    assert!(
        late - early < 0.050,
        "skew grew from {early:.3}s to {late:.3}s: drift accumulates across loop seams"
    );

    // Frame cadence stays at 30fps throughout — no dropped or doubled frames.
    let deltas: Vec<f64> = v.windows(2).map(|w| w[1] - w[0]).collect();
    let stalls = deltas.iter().filter(|d| **d > 0.100).count();
    assert_eq!(stalls, 0, "video stalled {stalls} times across 30 loop boundaries");
}

/// The fallback path still has to work when stream copy is unsuitable (§14).
#[test]
fn compatibility_mode_transcodes_the_same_playlist() {
    let tools = require_ffmpeg!();
    let b = builder(tools.clone(), PROFILE);
    let work = tempfile::tempdir().unwrap();
    let cache = MediaCache::new(work.path().join("cache"));

    let sources = make_fixtures(&tools, &fixture_dir(), &loop_fixtures());
    let mut files = Vec::new();
    for (i, s) in sources.iter().enumerate() {
        let info = probe(&b, s).unwrap();
        files.push(
            normalize_one(&b, &cache, s, &format!("loop{i}"), &info, PROFILE, &CancelToken::new(), |_| {})
                .unwrap()
                .output_path,
        );
    }
    let manifest = work.path().join("compat.txt");
    write_manifest(&manifest, &files).unwrap();

    let out = work.path().join("compat.flv");
    let args = b.build_dry_run_args(&manifest, &out, StreamMode::CompatibilityEncode, Some(6.0), true);
    assert!(args.windows(2).any(|w| w == ["-c:v", "libx264"]), "compat mode must encode");

    let (code, stderr) = run_ffmpeg(&tools, &args);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stream_field(&tools, &out, "v:0", "codec_name"), "h264");
    assert!((format_duration(&tools, &out) - 6.0).abs() < 0.5);
}
