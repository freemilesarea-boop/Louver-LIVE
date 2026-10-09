//! What actually comes out of the pipe.
//!
//! These tests do not assert on an argv or an exit code. They run a real
//! FFmpeg against a real HLS source, receive the result over a real RTMP
//! connection, and then decode it: the colour of a frame says which input the
//! picture came from, and the frequency of the audio says which input the sound
//! came from.
//!
//! The fixtures are chosen so a wrong answer is unmistakable:
//!
//! | input | picture | sound |
//! |---|---|---|
//! | playlist (input 0) | green | 440 Hz, then 880 Hz |
//! | live source (input 1) | red | 100 Hz |
//!
//! So a correct output is **red** with **440 Hz**. Green would mean the picture
//! came from the playlist; 100 Hz would mean the source's own audio was sent.

mod common;

use common::*;
use louver_core::streaming::ffmpeg::{FfmpegCommandBuilder, FfmpegTools};
use louver_core::OutputProfile;
use louver_live_source::args::capped_live_args;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Run the worker's own argv end to end and return the received file.
fn send_and_receive(dir: &Path, source_size: &str, seconds: u32) -> std::path::PathBuf {
    let manifest = make_playlist(dir);
    let hls_dir = dir.join("hls");
    make_hls_vod(&hls_dir, source_size, 40);
    let server = StaticServer::start(&hls_dir);
    assert!(server.is_up(), "the test source server did not come up");

    let port = free_port();
    let received = dir.join("received.flv");
    let mut sink = spawn_rtmp_sink(port, &received, seconds);
    std::thread::sleep(Duration::from_millis(1200));

    let builder = FfmpegCommandBuilder::new(FfmpegTools::new(ffmpeg(), ffprobe()), OutputProfile::P1080p30);
    let args = capped_live_args(
        &builder,
        &manifest,
        &server.url("live.m3u8"),
        &format!("rtmp://127.0.0.1:{port}/live/test"),
        true,
    );
    let mut sender = Command::new(ffmpeg())
        .args(&args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("sender");

    // The sink stops itself at `seconds`; the sender is then torn down.
    let _ = sink.wait();
    let _ = sender.kill();
    let _ = sender.wait();
    received
}

#[test]
fn the_picture_comes_from_the_live_source_and_the_sound_from_the_playlist() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let got = send_and_receive(dir.path(), "1280x720", 8);

    // Exactly one of each: a second audio track would mean the source's sound
    // was carried as well as replaced.
    assert_eq!(stream_count(&got, "video"), 1, "one video track");
    assert_eq!(stream_count(&got, "audio"), 1, "one audio track");

    // Red, not green.
    let (r, g, b) = first_frame_rgb(&got);
    assert!(
        r > g + 40.0 && r > b + 40.0,
        "picture should be the live source's red, got ({r:.0},{g:.0},{b:.0})"
    );

    // 440 Hz, not 100 Hz. A generous window, because the measurement is a
    // zero-crossing estimate over a re-encoded AAC stream.
    let hz = dominant_hz(&got, 3);
    assert!((hz - 440.0).abs() < 60.0, "sound should be the playlist's 440Hz, measured {hz:.0}Hz");
    assert!(
        (hz - 100.0).abs() > 150.0,
        "the source's own 100Hz must not be in the output, measured {hz:.0}Hz"
    );
}

#[test]
fn a_4k_source_is_sent_at_1080p() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // 3840x2160 in; the cap must bring it down. Without the cap this arrives as
    // 2160p encoded at the profile's 6000 kbps, which was the gap the
    // investigation found.
    let got = send_and_receive(dir.path(), "3840x2160", 10);
    let (codec, w, h) = video_of(&got).expect("a video stream");
    assert_eq!(codec, "h264");
    assert_eq!((w, h), (1920, 1080), "a 4K source must be capped to 1080p");
}

#[test]
fn a_source_below_the_cap_is_not_scaled_up() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let got = send_and_receive(dir.path(), "1280x720", 8);
    let (_, w, h) = video_of(&got).expect("a video stream");
    assert_eq!((w, h), (1280, 720), "720p must stay 720p — upscaling costs CPU and adds nothing");
}
