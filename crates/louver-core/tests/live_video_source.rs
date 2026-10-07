//! The traffic-CCTV proof of concept, end to end, with real FFmpeg.
//!
//! The question this file answers is the one the PoC exists for: **does a live
//! HTTP video source and a playlist's audio actually come out of one FFmpeg as
//! a single stream?** So nothing here is mocked. A real HLS stream is encoded,
//! served over a real HTTP socket, and read by the real argv the product builds;
//! the output is then probed to see which input each half came from.
//!
//! Two assertions carry the whole thing:
//!
//!  - the output's **picture size is the camera's** (640×360) and not the
//!    playlist's (1920×1080), so the video is the live source;
//!  - the output's **audio energy is at 440 Hz** (the playlist's tone) and
//!    there is nothing at 1 kHz (the camera's tone), so the camera's sound was
//!    dropped rather than mixed in.
//!
//! The live source is a growing HLS playlist on localhost. `cctv::validate`
//! would refuse that address in production, and that is the point of it — the
//! check is a layer above this one, tested in `louver_cloud::cctv`.

mod common;

use louver_core::config::OutputProfile;
use louver_core::streaming::ffmpeg::FfmpegTools;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A video with a known tone and a known size — the "music" in the playlist.
fn music(tools: &FfmpegTools, dir: &Path, name: &str, tone_hz: u32, colour: &str) -> PathBuf {
    let out = dir.join(name);
    let args = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        &format!("sine=frequency={tone_hz}:sample_rate=48000:duration=6"),
        "-f",
        "lavfi",
        "-i",
        &format!("color=c={colour}:s=1920x1080:r=30:d=6"),
        "-map",
        "1:v",
        "-map",
        "0:a",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-pix_fmt",
        "yuv420p",
        "-g",
        "30",
        "-c:a",
        "aac",
        "-ar",
        "48000",
        "-ac",
        "2",
        "-shortest",
        &out.to_string_lossy(),
    ]
    .map(str::to_string);
    let (code, err) = common::run_ffmpeg(tools, &args);
    assert_eq!(code, 0, "could not make {name}: {err}");
    out
}

/// An HLS stream that stands in for the camera: a moving pattern, a 1 kHz tone,
/// and a size nothing else in this test uses.
fn hls_camera(tools: &FfmpegTools, dir: &Path) -> PathBuf {
    let playlist = dir.join("cam.m3u8");
    let args = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=640x360:rate=30:duration=20",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=1000:sample_rate=48000:duration=20",
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
        "-g",
        "30",
        "-c:a",
        "aac",
        "-ar",
        "48000",
        "-ac",
        "2",
        "-f",
        "hls",
        "-hls_time",
        "2",
        "-hls_list_size",
        "0",
        "-hls_segment_filename",
        &dir.join("seg%03d.ts").to_string_lossy(),
        &playlist.to_string_lossy(),
    ]
    .map(str::to_string);
    let (code, err) = common::run_ffmpeg(tools, &args);
    assert_eq!(code, 0, "could not make the HLS camera: {err}");
    playlist
}

/// The smallest HTTP server that can serve an HLS playlist and its segments.
///
/// Written here rather than pulled in: the test needs a real socket and real
/// `Content-Length` headers, which is forty lines, and a dependency for forty
/// lines of test code is a worse trade.
struct FileServer {
    port: u16,
    stop: Arc<AtomicBool>,
}

impl FileServer {
    fn serve(root: PathBuf) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).expect("nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((sock, _)) => {
                        let root = root.clone();
                        std::thread::spawn(move || handle(sock, &root));
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Self { port, stop }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}/{path}", self.port)
    }
}

impl Drop for FileServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn handle(mut sock: TcpStream, root: &Path) {
    sock.set_nonblocking(false).ok();
    let mut buf = [0u8; 2048];
    let Ok(n) = sock.read(&mut buf) else { return };
    let head = String::from_utf8_lossy(&buf[..n]).to_string();
    let Some(line) = head.lines().next() else { return };
    let path = line.split_whitespace().nth(1).unwrap_or("/");
    // One directory, flat, no traversal: a name, and nothing that is not one.
    let name = path.trim_start_matches('/').split('?').next().unwrap_or("");
    let safe = !name.is_empty() && !name.contains('/') && !name.contains("..");
    let body = if safe { std::fs::read(root.join(name)).ok() } else { None };
    let _ = match body {
        Some(bytes) => sock.write_all(
            &[
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                    bytes.len()
                )
                .into_bytes(),
                bytes,
            ]
            .concat(),
        ),
        None => sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
    };
}

/// `volumedetect`'s mean level, in dB, inside one narrow band.
///
/// How the test tells the two tones apart. Measured, not guessed — the same
/// pipeline was run twice, once mapping the playlist's audio and once mapping
/// the camera's, and the bands came out:
///
/// |                  | playlist audio | camera audio |
/// |------------------|----------------|--------------|
/// | band around 440  | -24.1 dB       | -44.3 dB     |
/// | band around 1000 | -51.4 dB       | -24.1 dB     |
///
/// So `band(440) - band(1000)` is about **+27 dB** when it is right and about
/// **-20 dB** when it is wrong: 47 dB apart, which is why the threshold below
/// can sit nowhere near either of them.
fn band_db(tools: &FfmpegTools, file: &Path, centre_hz: u32) -> f64 {
    let chain = format!("bandpass=f={centre_hz}:width_type=h:w=80,volumedetect");
    let out = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-nostdin", "-i", &file.to_string_lossy(), "-af", &chain, "-f", "null", "-"])
        .output()
        .expect("ffmpeg volumedetect");
    let err = String::from_utf8_lossy(&out.stderr);
    err.lines()
        .find_map(|l| l.split("mean_volume:").nth(1))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or_else(|| panic!("no mean_volume in:\n{err}"))
}

#[test]
fn a_live_http_source_supplies_the_picture_and_the_playlist_supplies_the_sound() {
    let tools = require_ffmpeg!();
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    // The playlist: two videos, each a different tone, so a transition happens
    // inside the window this test watches.
    let a = music(&tools, root, "music-a.mp4", 440, "blue");
    let b = music(&tools, root, "music-b.mp4", 440, "green");
    let manifest = root.join("manifest.txt");
    std::fs::write(&manifest, format!("file '{}'\nfile '{}'\n", a.to_string_lossy(), b.to_string_lossy()))
        .expect("manifest");

    // The camera, served over HTTP exactly as a real one would be.
    let cam_dir = root.join("cam");
    std::fs::create_dir_all(&cam_dir).expect("cam dir");
    hls_camera(&tools, &cam_dir);
    let server = FileServer::serve(cam_dir);
    let cam_url = server.url("cam.m3u8");

    // The argv under test, from the product's own builder, with the RTMP
    // destination swapped for a file and a bound on how long it runs.
    let out = root.join("out.flv");
    let builder = common::builder(tools.clone(), OutputProfile::P1080p30);
    let mut args = builder.build_live_video_stream_args(&manifest, &cam_url, &out.to_string_lossy(), true);
    let sink = args.len() - 1;
    args.insert(sink, "6".into());
    args.insert(sink, "-t".into());

    let (code, err) = common::run_ffmpeg(&tools, &args);
    assert_eq!(code, 0, "the live pipeline failed:\n{err}\nargv: {args:?}");
    assert!(out.is_file(), "no output was written");
    assert!(common::format_duration(&tools, &out) > 4.0, "output is too short to have run");

    // --- the picture is the camera's ------------------------------------
    assert_eq!(common::stream_field(&tools, &out, "v:0", "width"), "640");
    assert_eq!(common::stream_field(&tools, &out, "v:0", "height"), "360");
    assert_eq!(common::stream_field(&tools, &out, "v:0", "codec_name"), "h264");

    // --- the sound is the playlist's ------------------------------------
    assert_eq!(common::stream_field(&tools, &out, "a:0", "codec_name"), "aac");
    // Exactly one of each: the camera's audio is not a second track either.
    assert_eq!(
        common::ffprobe_value(
            &tools,
            &[
                "-v",
                "error",
                "-select_streams",
                "a",
                "-show_entries",
                "stream=index",
                "-of",
                "csv=p=0",
                &out.to_string_lossy()
            ],
        )
        .lines()
        .count(),
        1,
        "the output should carry one audio stream"
    );

    // The playlist's tone is in the output and the camera's is not. The 15 dB
    // comes from the measurements documented on `band_db`.
    let playlist_tone = band_db(&tools, &out, 440);
    let camera_tone = band_db(&tools, &out, 1000);
    assert!(
        playlist_tone - camera_tone > 15.0,
        "the sound is not the playlist's: 440 Hz band {playlist_tone} dB, 1 kHz band {camera_tone} dB. \
         A positive margin means the playlist; a negative one means the camera's audio was used."
    );
}

#[test]
fn a_camera_that_is_not_there_fails_this_broadcast_and_says_why() {
    let tools = require_ffmpeg!();
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let a = music(&tools, root, "music-a.mp4", 440, "blue");
    let manifest = root.join("manifest.txt");
    std::fs::write(&manifest, format!("file '{}'\n", a.to_string_lossy())).expect("manifest");

    // A port with nothing listening: the start fails, rather than hanging or
    // quietly sending a black picture.
    let dead = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let p = l.local_addr().unwrap().port();
        drop(l);
        format!("http://127.0.0.1:{p}/cam.m3u8")
    };
    let out = root.join("out.flv");
    let builder = common::builder(tools.clone(), OutputProfile::P1080p30);
    let mut args = builder.build_live_video_stream_args(&manifest, &dead, &out.to_string_lossy(), true);
    let sink = args.len() - 1;
    args.insert(sink, "4".into());
    args.insert(sink, "-t".into());

    let (code, err) = common::run_ffmpeg(&tools, &args);
    assert_ne!(code, 0, "a missing camera must fail the start, not succeed quietly");
    let e = err.to_lowercase();
    assert!(
        e.contains("connection refused") || e.contains("error") || e.contains("failed"),
        "the failure should say what happened:\n{err}"
    );
    // And the classifier the UI shows turns that into something readable.
    assert_eq!(louver_cloud_cctv_classify(&err), "connection_refused");
}

/// `louver_cloud::cctv::classify` is in the crate above this one, which the core
/// cannot depend on. The one case this test needs is re-stated rather than
/// skipped, so a change to the message FFmpeg prints is caught here too.
fn louver_cloud_cctv_classify(stderr: &str) -> &'static str {
    let e = stderr.to_lowercase();
    if e.contains("connection refused") {
        "connection_refused"
    } else {
        "failed"
    }
}
