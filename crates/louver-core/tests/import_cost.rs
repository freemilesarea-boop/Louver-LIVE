//! What adding one video costs, stage by stage.
//!
//! Written because a 90-minute source took over twenty minutes to appear in
//! the playlist. The stages are timed separately so the answer is a number
//! against a name rather than a guess:
//!
//!   LOUVER_IMPORT_SAMPLE=/path/to/long.mp4 \
//!     cargo test -p louver-core --test import_cost -- --ignored --nocapture
//!
//! Without the variable it builds its own fixture, which is short and
//! therefore only shows the shape. Point it at a real long file for real
//! numbers.

mod common;

use common::*;
use louver_core::config::OutputProfile;
use louver_core::media::cache::{media_hash, MediaCache};
use louver_core::media::normalize::{normalize_one, plan_for, CancelToken};
use louver_core::media::probe::{probe, probe_max_keyframe_gap};
use std::path::PathBuf;
use std::time::Instant;

const PROFILE: OutputProfile = OutputProfile::P1080p30;

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

/// A sample to measure against: the caller's file, or a short built one.
fn sample(tools: &louver_core::streaming::ffmpeg::FfmpegTools) -> (PathBuf, bool) {
    if let Some(p) = std::env::var_os("LOUVER_IMPORT_SAMPLE") {
        let p = PathBuf::from(p);
        assert!(p.is_file(), "LOUVER_IMPORT_SAMPLE does not point at a file: {}", p.display());
        return (p, true);
    }
    let f = make_fixture(
        tools,
        &std::env::temp_dir().join("louver-import-cost"),
        &FixtureSpec {
            name: "import_cost",
            duration: 30.0,
            size: "1280x720",
            fps: 24,
            sample_rate: 44100,
            channels: 1,
            tone_hz: 330,
            pattern: "smptebars",
        },
    );
    (f, false)
}

#[test]
#[ignore = "measures real I/O and encoding; run explicitly"]
fn what_adding_one_video_costs() {
    let tools = require_ffmpeg!();
    let b = builder(tools.clone(), PROFILE);
    let (path, is_real) = sample(&tools);
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

    println!(
        "\nsample: {} ({:.2} GB){}",
        path.display(),
        size as f64 / 1e9,
        if is_real { "" } else { "  [built fixture — short]" }
    );
    println!("{:<44} {:>12}", "stage", "took");
    println!("{}", "-".repeat(58));

    // --- what pressing 영상 추가 does now -------------------------------
    let t = Instant::now();
    let meta = std::fs::metadata(&path).unwrap();
    let register_ms = ms(t);
    println!(
        "{:<44} {:>9.1} ms   <- 영상 추가 returns here",
        "1. register (fs::metadata + INSERT)", register_ms
    );
    assert!(meta.len() > 0);

    // --- what the background thread does afterwards ----------------------
    let t = Instant::now();
    let info = probe(&b, &path).expect("probe");
    let probe_ms = ms(t);
    println!("{:<44} {:>9.1} ms", "2. probe (container header)", probe_ms);

    let t = Instant::now();
    let hash = media_hash(&path).expect("hash");
    let hash_ms = ms(t);
    println!("{:<44} {:>9.1} ms", "3. media_hash (head+tail 256 KB)", hash_ms);

    let t = Instant::now();
    let gap = probe_max_keyframe_gap(&b, &path, 60);
    let gap_ms = ms(t);
    println!(
        "{:<44} {:>9.1} ms  (gap {:?})",
        "4. keyframe probe (first 60 s)",
        gap_ms,
        gap.map(|g| (g * 100.0).round() / 100.0)
    );

    let t = Instant::now();
    let plan = plan_for(&b, &path, &info, PROFILE);
    let plan_ms = ms(t);
    println!("{:<44} {:>9.1} ms  (mode {})", "5. plan (compatibility)", plan_ms, plan.label());

    let analysis = probe_ms + hash_ms + plan_ms;
    println!("{}", "-".repeat(58));
    println!("{:<44} {:>9.1} ms", "add -> visible in playlist", register_ms);
    println!("{:<44} {:>9.1} ms", "add -> metadata known", register_ms + probe_ms + hash_ms);
    println!("{:<44} {:>9.1} ms", "add -> compatibility known", register_ms + analysis);

    // --- the long one ----------------------------------------------------
    let dir = tempfile::tempdir().unwrap();
    let cache = MediaCache::new(dir.path().join("cache"));
    let t = Instant::now();
    let out = normalize_one(&b, &cache, &path, &hash, &info, PROFILE, &CancelToken::new(), |_| {})
        .expect("prepare");
    let prep = t.elapsed().as_secs_f64();
    println!(
        "{:<44} {:>9.1} s   ({} at {:.2}x realtime)",
        "6. prepare (background, cancellable)",
        prep,
        plan.label(),
        info.duration_secs / prep.max(1e-9),
    );
    println!(
        "\nadding {} s of video: {:.0} ms to appear, {:.1} s to become broadcastable.\n",
        info.duration_secs.round(),
        register_ms,
        prep,
    );
    assert!(out.output_path.is_file());
}
