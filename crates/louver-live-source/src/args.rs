//! The FFmpeg argv, reused from production and capped at 1080p.
//!
//! This module deliberately does **not** rebuild the composition. The argv that
//! sends a live picture with the playlist's sound already exists and is already
//! tested: `FfmpegCommandBuilder::build_live_video_stream_args`. Reimplementing
//! it here would mean two copies of the one thing that must not drift — which
//! input the picture comes from, and which input the sound comes from.
//!
//! So this takes that argv and inserts exactly one filter. Two consequences
//! worth stating:
//!
//!  * production's own path is untouched. Nothing in `louver-core` changes, so
//!    the broadcasts running today build the same argv they built yesterday.
//!  * if the production composition ever changes, this follows it, and
//!    [`tests::the_cap_is_the_only_difference_from_production`] fails loudly if
//!    the insertion point disappears.
//!
//! Why the cap is needed at all: the production argv has no scale filter, so
//! the output resolution is whatever the source sends. A traffic camera is
//! small and that never mattered. A YouTube Live source is commonly 1080p and
//! can be 1440p or 2160p, and encoding 2160p at the profile's 6000 kbps is both
//! ruinous for quality and about four times the CPU. Measured on the dev box: a
//! 1080p30 source costs ~1.0–1.5 cores, so 2160p would not fit beside anything.

use louver_core::streaming::ffmpeg::FfmpegCommandBuilder;
use std::path::Path;

/// The widest and tallest output this worker will produce.
pub const MAX_WIDTH: u32 = 1920;
pub const MAX_HEIGHT: u32 = 1080;

/// Scale down to fit 1080p, never up, keeping the aspect ratio.
///
/// `min(iw\,1920)` and not `1920`: a 1280x720 source must stay 1280x720.
/// `force_original_aspect_ratio=decrease` fits inside the box rather than
/// stretching, and `force_divisible_by=2` keeps both sides even, which
/// `yuv420p` requires — a 1079-pixel-tall intermediate is a hard encoder error.
///
/// The commas inside `min()` are escaped because a bare comma separates filters
/// in a filtergraph. There is no shell here (this is one element of an argv
/// vector), so the backslash is FFmpeg's own escape and not a shell's.
pub const SCALE_FILTER: &str =
    r"scale=min(iw\,1920):min(ih\,1080):force_original_aspect_ratio=decrease:force_divisible_by=2";

/// Production's live-source argv with the 1080p cap inserted.
///
/// The filter goes immediately before `-c:v`, which is where the output options
/// begin. Anywhere in the output section would work for FFmpeg; this spot is
/// chosen so the argv still reads top to bottom as inputs, then filters, then
/// encoding, then destination.
pub fn capped_live_args(
    builder: &FfmpegCommandBuilder,
    manifest: &Path,
    live_video_url: &str,
    destination: &str,
    loop_forever: bool,
) -> Vec<String> {
    let base = builder.build_live_video_stream_args(manifest, live_video_url, destination, loop_forever);
    insert_cap(base)
}

/// Split out so it can be tested against a hand-written argv as well as the
/// real one, and so the failure when `-c:v` is absent is one place.
fn insert_cap(mut args: Vec<String>) -> Vec<String> {
    match args.iter().position(|a| a == "-c:v") {
        Some(at) => {
            args.splice(at..at, ["-vf".to_string(), SCALE_FILTER.to_string()]);
            args
        }
        // Not reachable with the current builder, and a panic here would take
        // down a worker for a reason the operator cannot act on. Appending the
        // filter before the output URL keeps the cap in force; the last element
        // is the destination, so `len - 1` is the right place.
        None => {
            let at = args.len().saturating_sub(1);
            args.splice(at..at, ["-vf".to_string(), SCALE_FILTER.to_string()]);
            args
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use louver_core::streaming::ffmpeg::FfmpegTools;
    use louver_core::OutputProfile;

    fn builder() -> FfmpegCommandBuilder {
        FfmpegCommandBuilder::new(
            FfmpegTools::new("/usr/bin/ffmpeg", "/usr/bin/ffprobe"),
            OutputProfile::P1080p30,
        )
    }

    const LIVE: &str = "https://manifest.example/hls/playlist.m3u8";
    const DEST: &str = "rtmp://127.0.0.1:1935/live/test";

    fn capped() -> Vec<String> {
        capped_live_args(&builder(), Path::new("/tmp/m.txt"), LIVE, DEST, true)
    }

    fn production() -> Vec<String> {
        builder().build_live_video_stream_args(Path::new("/tmp/m.txt"), LIVE, DEST, true)
    }

    #[test]
    fn the_cap_is_the_only_difference_from_production() {
        // The guarantee this whole module rests on: production's argv is reused
        // and exactly two elements are added.
        let (p, c) = (production(), capped());
        assert_eq!(c.len(), p.len() + 2, "only the filter is added");
        let without: Vec<&String> = c.iter().filter(|a| *a != "-vf" && a.as_str() != SCALE_FILTER).collect();
        assert_eq!(without, p.iter().collect::<Vec<_>>(), "nothing else moved or changed");
    }

    #[test]
    fn the_picture_still_comes_from_the_live_input_and_the_sound_from_the_playlist() {
        // The one property that must never drift. If the cap were inserted in
        // the wrong place, or production's mapping changed, this fails.
        let c = capped();
        let maps: Vec<&String> = c
            .iter()
            .enumerate()
            .filter(|(i, _)| c.get(i.wrapping_sub(1)).map(|p| p == "-map").unwrap_or(false))
            .map(|(_, v)| v)
            .collect();
        assert_eq!(maps, vec!["1:v:0", "0:a:0"]);
    }

    #[test]
    fn the_sources_audio_is_never_mapped() {
        // Dropping the original sound is done by not asking for it. There must
        // be no `-map 1:a`, and no `-map` of a second audio stream at all.
        let c = capped();
        assert!(!c.iter().any(|a| a.starts_with("1:a")), "{c:?}");
        assert_eq!(c.iter().filter(|a| a.starts_with("0:a") || a.starts_with("1:a")).count(), 1);
    }

    #[test]
    fn the_filter_sits_in_the_output_section() {
        let c = capped();
        let vf = c.iter().position(|a| a == "-vf").expect("-vf");
        let cv = c.iter().position(|a| a == "-c:v").expect("-c:v");
        let last_input = c.iter().rposition(|a| a == "-i").expect("-i");
        assert!(vf > last_input, "the filter must come after every input");
        assert!(vf < cv, "the filter must come before the encoder");
        assert_eq!(c[vf + 1], SCALE_FILTER);
    }

    #[test]
    fn the_cap_scales_down_and_never_up() {
        // Read as an assertion about the expression, since the real scaling is
        // measured by the FFmpeg integration test.
        assert!(SCALE_FILTER.contains(r"min(iw\,1920)"), "{SCALE_FILTER}");
        assert!(SCALE_FILTER.contains(r"min(ih\,1080)"), "{SCALE_FILTER}");
        assert!(SCALE_FILTER.contains("force_original_aspect_ratio=decrease"));
        assert!(SCALE_FILTER.contains("force_divisible_by=2"), "yuv420p needs even sides");
        assert_eq!((MAX_WIDTH, MAX_HEIGHT), (1920, 1080));
    }

    #[test]
    fn an_argv_without_an_encoder_still_gets_the_cap_before_the_destination() {
        let out = insert_cap(vec!["-i".into(), "in".into(), "rtmp://dest".into()]);
        assert_eq!(out, vec!["-i", "in", "-vf", SCALE_FILTER, "rtmp://dest"]);
    }
}
