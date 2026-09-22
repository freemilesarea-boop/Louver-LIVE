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
use louver_core::media::normalize::{normalize_one, readiness_for, CancelToken};
use louver_core::media::probe::{
    check_compatibility, plan_transcode, probe, probe_max_keyframe_gap, Readiness,
};
use louver_core::streaming::ffmpeg::FfmpegTools;
use louver_core::streaming::manifest::write_manifest;
use std::path::{Path, PathBuf};

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
            &b,
            &cache,
            src,
            &format!("fixture{i}"),
            &info,
            PROFILE,
            &CancelToken::new(),
            |_| {},
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
            info.video_codec,
            info.width,
            info.height,
            info.fps,
            info.pixel_format,
            info.time_base,
            info.audio_codec.clone().unwrap_or_default(),
            info.audio_sample_rate.unwrap_or(0),
        ));
    }
    assert_eq!(
        signatures.len(),
        1,
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
        let out =
            normalize_one(&b, &cache, src, &format!("loop{i}"), &info, PROFILE, &CancelToken::new(), |_| {})
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
    assert_eq!(
        v.len(),
        v.iter().map(|f| (f * 1000.0) as i64).collect::<std::collections::HashSet<_>>().len(),
        "duplicate video timestamps in the looped output"
    );

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
fn dominant_colour(
    tools: &louver_core::streaming::ffmpeg::FfmpegTools,
    file: &std::path::Path,
    t: f64,
) -> char {
    let out = std::process::Command::new(&tools.ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-ss",
            &format!("{t:.3}"),
            "-i",
            &file.to_string_lossy(),
            "-frames:v",
            "1",
            "-vf",
            "scale=1:1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-",
        ])
        .output()
        .expect("colour sample failed");
    let px = out.stdout;
    assert!(px.len() >= 3, "no pixel returned at t={t}");
    let (r, g, bl) = (px[0], px[1], px[2]);
    if r >= g && r >= bl {
        'R'
    } else if g >= bl {
        'G'
    } else {
        'B'
    }
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
        &FixtureSpec {
            name: "case_a",
            duration: 4.0,
            size: "1280x720",
            fps: 30,
            sample_rate: 48000,
            channels: 2,
            tone_hz: 440,
            pattern: "testsrc2",
        },
    );
    let info = probe(&b, &src).unwrap();
    let n = normalize_one(&b, &cache, &src, "case_a", &info, PROFILE, &CancelToken::new(), |_| {}).unwrap();

    let files = vec![n.output_path.clone(), n.output_path.clone(), n.output_path.clone()];
    let manifest = work.path().join("a.txt");
    write_manifest(&manifest, &files).unwrap();

    let out = work.path().join("a.flv");
    let (code, stderr) =
        run_ffmpeg(&tools, &b.build_dry_run_args(&manifest, &out, StreamMode::StreamCopy, None, false));
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
        FixtureSpec {
            name: "case_b1",
            duration: 3.4,
            size: "854x480",
            fps: 25,
            sample_rate: 44100,
            channels: 2,
            tone_hz: 300,
            pattern: "testsrc2",
        },
        FixtureSpec {
            name: "case_b2",
            duration: 5.75,
            size: "1920x1080",
            fps: 50,
            sample_rate: 22050,
            channels: 1,
            tone_hz: 500,
            pattern: "smptebars",
        },
        FixtureSpec {
            name: "case_b3",
            duration: 2.2,
            size: "640x360",
            fps: 15,
            sample_rate: 48000,
            channels: 2,
            tone_hz: 700,
            pattern: "testsrc",
        },
    ];
    let sources = make_fixtures(&tools, &fixture_dir(), &specs);

    let mut files = Vec::new();
    let mut total = 0.0;
    for (i, s) in sources.iter().enumerate() {
        let info = probe(&b, s).unwrap();
        let n =
            normalize_one(&b, &cache, s, &format!("caseb{i}"), &info, PROFILE, &CancelToken::new(), |_| {})
                .unwrap();
        total += n.duration_secs;
        files.push(n.output_path);
    }

    let manifest = work.path().join("b.txt");
    write_manifest(&manifest, &files).unwrap();
    let out = work.path().join("b.flv");
    let (code, stderr) =
        run_ffmpeg(&tools, &b.build_dry_run_args(&manifest, &out, StreamMode::StreamCopy, None, false));

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
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=1280x720:rate=30:duration=5",
                "-itsoffset",
                "0.7",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=44100:duration=5",
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-muxdelay",
                "0.9",
            ])
            .arg(&skewed)
            .status()
            .unwrap();
        assert!(st.success());
    }

    let normal = make_fixture(
        &tools,
        &dir,
        &FixtureSpec {
            name: "case_d2",
            duration: 4.0,
            size: "1280x720",
            fps: 30,
            sample_rate: 48000,
            channels: 2,
            tone_hz: 880,
            pattern: "smptebars",
        },
    );

    let mut files = Vec::new();
    let mut total = 0.0;
    for (i, s) in [skewed.clone(), normal].iter().enumerate() {
        let info = probe(&b, s).unwrap();
        let n =
            normalize_one(&b, &cache, s, &format!("cased{i}"), &info, PROFILE, &CancelToken::new(), |_| {})
                .unwrap();
        total += n.duration_secs;
        files.push(n.output_path);
    }

    let manifest = work.path().join("d.txt");
    write_manifest(&manifest, &files).unwrap();
    let out = work.path().join("d.flv");
    let (code, stderr) =
        run_ffmpeg(&tools, &b.build_dry_run_args(&manifest, &out, StreamMode::StreamCopy, None, false));

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
        let n =
            normalize_one(&b, &cache, s, &format!("fixture{i}"), &info, PROFILE, &CancelToken::new(), |_| {})
                .unwrap();
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

// ---------------------------------------------------------------------------
// §14 — an optimized file is never optimized again
// ---------------------------------------------------------------------------

/// Re-optimizing must reuse the cache, and only a changed source may invalidate it.
///
/// This matters for real users: a 100-video library takes hours to optimize,
/// and silently redoing that work on every launch would make the product
/// unusable. Proven by file identity, not by trusting the status field.
#[test]
fn an_optimized_video_is_not_re_encoded_and_only_a_changed_source_invalidates_it() {
    let tools = require_ffmpeg!();
    let b = builder(tools.clone(), PROFILE);
    let work = tempfile::tempdir().unwrap();
    let cache = MediaCache::new(work.path().join("cache"));

    // A source that genuinely needs optimizing.
    let src = work.path().join("source.mp4");
    let make_source = |path: &std::path::Path, pattern: &str| {
        let st = std::process::Command::new(&tools.ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                &format!("{pattern}=size=640x480:rate=24:duration=3"),
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=44100:duration=3",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ])
            .arg(path)
            .status()
            .unwrap();
        assert!(st.success());
    };
    make_source(&src, "testsrc2");

    let hash1 = louver_core::media::cache::media_hash(&src).unwrap();
    let info = probe(&b, &src).unwrap();

    // First pass: a real encode.
    let first = normalize_one(&b, &cache, &src, &hash1, &info, PROFILE, &CancelToken::new(), |_| {}).unwrap();
    assert!(!first.from_cache, "the first pass must actually encode");
    let produced = first.output_path.clone();
    let stamp1 = std::fs::metadata(&produced).unwrap().modified().unwrap();
    let bytes1 = std::fs::metadata(&produced).unwrap().len();

    // Second pass: must be a cache hit, with the file untouched.
    std::thread::sleep(std::time::Duration::from_millis(1100)); // mtime granularity
    let second =
        normalize_one(&b, &cache, &src, &hash1, &info, PROFILE, &CancelToken::new(), |_| {}).unwrap();
    assert!(second.from_cache, "an already-optimized video was re-encoded (§14)");
    assert_eq!(second.output_path, produced);
    assert_eq!(second.duration_secs, first.duration_secs);
    let stamp2 = std::fs::metadata(&produced).unwrap().modified().unwrap();
    assert_eq!(stamp1, stamp2, "the cached file was rewritten instead of reused");
    assert_eq!(bytes1, std::fs::metadata(&produced).unwrap().len());

    // Replacing the source changes its identity, which invalidates the cache.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    make_source(&src, "smptebars");
    let hash2 = louver_core::media::cache::media_hash(&src).unwrap();
    assert_ne!(hash1, hash2, "a replaced source must change its cache identity");
    assert!(cache.lookup(&hash2, PROFILE).is_none(), "the new content must miss the cache");
    // ...while the old entry is still a hit for the old identity.
    assert!(cache.lookup(&hash1, PROFILE).is_some());

    let info2 = probe(&b, &src).unwrap();
    let third =
        normalize_one(&b, &cache, &src, &hash2, &info2, PROFILE, &CancelToken::new(), |_| {}).unwrap();
    assert!(!third.from_cache, "a changed source must be re-encoded");
    assert_ne!(third.output_path, produced, "the new encode must be a separate cache entry");

    // A different profile is a separate entry too, not a spurious re-encode.
    assert!(cache.lookup(&hash2, OutputProfile::P1080p30).is_none());
}

/// Merely reading a file must not invalidate its cache.
#[test]
fn touching_a_source_without_changing_it_keeps_the_cache_valid() {
    let tools = require_ffmpeg!();
    let b = builder(tools.clone(), PROFILE);
    let work = tempfile::tempdir().unwrap();
    let cache = MediaCache::new(work.path().join("cache"));

    let src = work.path().join("stable.mp4");
    std::process::Command::new(&tools.ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=24:duration=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100:duration=2",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ])
        .arg(&src)
        .status()
        .unwrap();

    let hash = louver_core::media::cache::media_hash(&src).unwrap();
    let info = probe(&b, &src).unwrap();
    normalize_one(&b, &cache, &src, &hash, &info, PROFILE, &CancelToken::new(), |_| {}).unwrap();

    // Read it, the way probing or playback would.
    let _ = std::fs::read(&src).unwrap();
    assert_eq!(
        louver_core::media::cache::media_hash(&src).unwrap(),
        hash,
        "reading a file must not change its cache identity"
    );
    assert!(cache.lookup(&hash, PROFILE).is_some());
}

/// A remuxed file and an encoded one in the same playlist, stream-copied (§12).
///
/// This is what the copy fast path risks: the live command is `concat` plus
/// `-c copy`, which needs every entry to agree on codec, geometry and
/// timescale. A file that skipped the encoder has to come out of the
/// normalizer just as joinable as one that did not, or the whole optimization
/// is a way of breaking broadcasts quietly.
#[test]
fn a_copied_file_and_an_encoded_one_concatenate_and_stream_copy_together() {
    let tools = require_ffmpeg!();
    let b = builder(tools.clone(), PROFILE);
    let work = tempfile::tempdir().unwrap();
    let cache = MediaCache::new(work.path().join("cache"));

    // Already exactly what we broadcast: this one must take the copy path.
    let ready = work.path().join("ready.mp4");
    let (code, err) = run_ffmpeg(
        &tools,
        &[
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1280x720:rate=30:duration=3",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=3",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-profile:v",
            "high",
            "-level",
            "3.1",
            "-pix_fmt",
            "yuv420p",
            "-g",
            "60",
            "-keyint_min",
            "60",
            "-sc_threshold",
            "0",
            "-b:v",
            "3000k",
            "-r",
            "30",
            "-fps_mode",
            "cfr",
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
            ready.to_str().unwrap(),
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect::<Vec<_>>(),
    );
    assert_eq!(code, 0, "fixture build failed: {err}");

    // Nothing like it: wrong size, wrong rate, wrong audio.
    let odd = make_fixture(
        &tools,
        &fixture_dir(),
        &FixtureSpec {
            name: "mixed_odd",
            duration: 3.0,
            size: "640x480",
            fps: 24,
            sample_rate: 44100,
            channels: 1,
            tone_hz: 330,
            pattern: "smptebars",
        },
    );

    let ready_info = probe(&b, &ready).unwrap();
    let odd_info = probe(&b, &odd).unwrap();

    let ready_plan = plan_transcode(&ready_info, PROFILE, probe_max_keyframe_gap(&b, &ready, 60));
    assert!(ready_plan.is_remux(), "the conformant fixture should not be encoded: {ready_plan:?}");
    let odd_plan = plan_transcode(&odd_info, PROFILE, None);
    assert!(!odd_plan.video.is_copy(), "the odd fixture must still be encoded: {odd_plan:?}");

    let a =
        normalize_one(&b, &cache, &ready, "mixed_ready", &ready_info, PROFILE, &CancelToken::new(), |_| {})
            .unwrap();
    let c = normalize_one(&b, &cache, &odd, "mixed_odd", &odd_info, PROFILE, &CancelToken::new(), |_| {})
        .unwrap();
    assert_eq!(a.video_encoder, "copy", "the ready file went through the encoder anyway");
    assert_ne!(c.video_encoder, "copy");

    // The two cache entries must agree on everything concat cares about.
    for field in ["codec_name", "width", "height", "pix_fmt", "time_base", "r_frame_rate"] {
        assert_eq!(
            stream_field(&tools, &a.output_path, "v:0", field),
            stream_field(&tools, &c.output_path, "v:0", field),
            "copied and encoded entries disagree on {field}"
        );
    }
    for field in ["codec_name", "sample_rate", "channels"] {
        assert_eq!(
            stream_field(&tools, &a.output_path, "a:0", field),
            stream_field(&tools, &c.output_path, "a:0", field),
            "copied and encoded entries disagree on audio {field}"
        );
    }

    // And they have to survive the real live command, interleaved and looped.
    let files =
        vec![a.output_path.clone(), c.output_path.clone(), a.output_path.clone(), c.output_path.clone()];
    let manifest = work.path().join("mixed.txt");
    write_manifest(&manifest, &files).unwrap();
    let out = work.path().join("mixed.flv");
    let (code, stderr) =
        run_ffmpeg(&tools, &b.build_dry_run_args(&manifest, &out, StreamMode::StreamCopy, None, false));
    assert_eq!(code, 0, "{stderr}");
    assert!(timestamp_faults(&stderr).is_empty(), "mixed playlist faults:\n{stderr}");

    let want = a.duration_secs * 2.0 + c.duration_secs * 2.0;
    assert!(
        (format_duration(&tools, &out) - want).abs() < 0.3,
        "mixed playlist ran {}s, expected {want}s",
        format_duration(&tools, &out)
    );
}

/// Build a file that already matches the broadcast profile exactly.
///
/// This is the file §4 is about: something downloaded from YouTube in H.264 /
/// AAC that happens to match what we broadcast, down to the timescale.
fn conformant_fixture(tools: &FfmpegTools, at: &Path, seconds: &str, pattern: &str) -> PathBuf {
    let args: Vec<String> = [
        "-y",
        "-f",
        "lavfi",
        "-i",
        &format!("{pattern}=size=1280x720:rate=30:duration={seconds}"),
        "-f",
        "lavfi",
        "-i",
        &format!("sine=frequency=440:sample_rate=48000:duration={seconds}"),
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-profile:v",
        "high",
        "-level",
        "3.1",
        "-pix_fmt",
        "yuv420p",
        "-g",
        "60",
        "-keyint_min",
        "60",
        "-sc_threshold",
        "0",
        "-b:v",
        "3000k",
        "-r",
        "30",
        "-fps_mode",
        "cfr",
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
        at.to_str().unwrap(),
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    let (code, err) = run_ffmpeg(tools, &args);
    assert_eq!(code, 0, "conformant fixture build failed: {err}");
    at.to_path_buf()
}

/// §4, §6 and §9 together: the cheap path, and why it is not cheaper still.
///
/// A source that already matches the broadcast profile in every respect takes
/// the copy path — no decode, no scale, no fps filter, no H.264 encode, no AAC
/// encode. What it does *not* get is a way around the normalizer, and this
/// test is the reason. Concatenated straight from the user's file, a perfectly
/// conformant source produces non-monotonic DTS at every join; the same file
/// after a packet-copy remux does not. The difference is the whole-frame cut
/// and the zeroed start timestamps the normalizer applies, which no source
/// file has of its own accord.
///
/// If someone later decides the remux is a waste and points the manifest at
/// the original, this fails.
#[test]
fn a_source_file_used_untouched_breaks_the_loop() {
    let tools = require_ffmpeg!();
    let b = builder(tools.clone(), PROFILE);
    let work = tempfile::tempdir().unwrap();
    let cache = MediaCache::new(work.path().join("cache"));

    let raw = conformant_fixture(&tools, &work.path().join("asis.mp4"), "3", "testsrc2");
    let raw_info = probe(&b, &raw).unwrap();

    // It matches the profile on every axis the checker knows about.
    assert!(
        check_compatibility(&raw_info, PROFILE).is_compatible(),
        "fixture is not conformant, so this test would prove nothing: {:?}",
        check_compatibility(&raw_info, PROFILE).reasons(),
    );

    // So preparing it costs a remux and not an encode (§3 A, §4).
    let verdict = readiness_for(&b, &cache, &raw, "asis", &raw_info, PROFILE);
    assert_eq!(verdict.label(), "remux", "a conformant source must not be re-encoded");

    let prepared_raw =
        normalize_one(&b, &cache, &raw, "asis", &raw_info, PROFILE, &CancelToken::new(), |_| {}).unwrap();
    assert_eq!(prepared_raw.video_encoder, "copy", "the conformant source went through an encoder");

    // Asking again costs nothing at all (§8).
    assert_eq!(readiness_for(&b, &cache, &raw, "asis", &raw_info, PROFILE), Readiness::Cached);

    // A partner that genuinely needed work.
    let odd = make_fixture(
        &tools,
        &fixture_dir(),
        &FixtureSpec {
            name: "asis_partner",
            duration: 3.0,
            size: "640x480",
            fps: 24,
            sample_rate: 44100,
            channels: 1,
            tone_hz: 330,
            pattern: "smptebars",
        },
    );
    let odd_info = probe(&b, &odd).unwrap();
    let partner =
        normalize_one(&b, &cache, &odd, "asis_partner", &odd_info, PROFILE, &CancelToken::new(), |_| {})
            .unwrap();

    // Run the real live command twice over: once with the user's own file in
    // the manifest, once with the remux of that same file.
    let faults_for = |label: &str, first: &Path| -> Vec<String> {
        let files = vec![
            first.to_path_buf(),
            partner.output_path.clone(),
            first.to_path_buf(),
            partner.output_path.clone(),
        ];
        let manifest = work.path().join(format!("{label}.txt"));
        write_manifest(&manifest, &files).unwrap();
        let out = work.path().join(format!("{label}.flv"));
        let (code, stderr) =
            run_ffmpeg(&tools, &b.build_dry_run_args(&manifest, &out, StreamMode::StreamCopy, None, false));
        assert_eq!(code, 0, "{stderr}");
        timestamp_faults(&stderr)
    };

    let untouched = faults_for("untouched", &raw);
    assert!(
        !untouched.is_empty(),
        "the shortcut looks safe on this FFmpeg build — re-read the note on \
         MediaStatus::Compatible before trusting it",
    );

    let remuxed = faults_for("remuxed", &prepared_raw.output_path);
    assert!(remuxed.is_empty(), "a remuxed conformant source must join cleanly:\n{}", remuxed.join("\n"),);
}
