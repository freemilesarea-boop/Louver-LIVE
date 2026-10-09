//! Harness for the tests that run a real FFmpeg.
//!
//! Everything here is local and owned by the test: a static HTTP server on a
//! loopback port, an RTMP receiver on another, and files in a temp directory.
//! Nothing reaches the internet and nothing reaches production.
//!
//! The HTTP server is written here rather than shelled out to, for one reason
//! the stall tests need: a source can be made to fail in **two distinguishable
//! ways** — stop producing new segments while still answering 200 (what an
//! ended broadcast looks like), or refuse connections entirely. A `python -m
//! http.server` child can only do the second.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub fn ffmpeg() -> &'static str {
    "/usr/bin/ffmpeg"
}
pub fn ffprobe() -> &'static str {
    "/usr/bin/ffprobe"
}

/// Skip rather than fail where FFmpeg is not installed, so this suite is
/// useful on a machine that cannot run it.
pub fn have_ffmpeg() -> bool {
    Path::new(ffmpeg()).exists() && Path::new(ffprobe()).exists()
}

/// A port nothing is listening on, by asking the kernel for one and letting go.
pub fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(ffmpeg()).args(args).output().expect("ffmpeg")
}

/// Two playlist items, each a solid **green** picture with a pure tone, and a
/// concat manifest naming them.
///
/// Green and 440/880 Hz so that a test can tell, from the received stream
/// alone, whether the picture and the sound came from where they should.
pub fn make_playlist(dir: &Path) -> PathBuf {
    let mut lines = String::new();
    for tone in [440, 880] {
        let out = dir.join(format!("pl_{tone}.mp4"));
        let o = run(&[
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=green:s=640x360:r=30:d=5",
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency={tone}:sample_rate=48000:duration=5"),
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-shortest",
            out.to_str().unwrap(),
        ]);
        assert!(o.status.success(), "playlist item: {}", String::from_utf8_lossy(&o.stderr));
        lines.push_str(&format!("file '{}'\n", out.display()));
    }
    let manifest = dir.join("manifest.txt");
    std::fs::write(&manifest, lines).unwrap();
    manifest
}

/// A finished HLS stream of a solid **red** picture with a 100 Hz tone.
///
/// Red and 100 Hz are the "wrong" answers: if either appears in the output, the
/// composition took the picture or the sound from the wrong input.
pub fn make_hls_vod(dir: &Path, size: &str, secs: u32) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let m3u8 = dir.join("live.m3u8");
    let o = run(&[
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        &format!("color=c=red:s={size}:r=30:d={secs}"),
        "-f",
        "lavfi",
        "-i",
        &format!("sine=frequency=100:sample_rate=48000:duration={secs}"),
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
        "-f",
        "hls",
        "-hls_time",
        "2",
        "-hls_playlist_type",
        "vod",
        "-hls_segment_filename",
        dir.join("s%03d.ts").to_str().unwrap(),
        m3u8.to_str().unwrap(),
    ]);
    assert!(o.status.success(), "hls vod: {}", String::from_utf8_lossy(&o.stderr));
    m3u8
}

/// An HLS stream produced **in real time**, so a reader cannot run ahead of it.
///
/// This is what makes a stall test meaningful: with a finished playlist on
/// loopback, FFmpeg downloads the whole thing in the first second and killing
/// the source afterwards proves nothing.
pub fn spawn_hls_realtime(dir: &Path, size: &str, secs: u32) -> Child {
    std::fs::create_dir_all(dir).unwrap();
    Command::new(ffmpeg())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-re",
            "-f",
            "lavfi",
            "-i",
            &format!("color=c=red:s={size}:r=30"),
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=100:sample_rate=48000",
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
            "-t",
            &secs.to_string(),
            "-f",
            "hls",
            "-hls_time",
            "2",
            "-hls_list_size",
            "3",
            "-hls_flags",
            "delete_segments+omit_endlist",
            "-hls_segment_filename",
            dir.join("s%03d.ts").to_str().unwrap(),
            dir.join("live.m3u8").to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("hls generator")
}

/// A static file server on loopback that the test can take away.
pub struct StaticServer {
    pub port: u16,
    shutdown: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StaticServer {
    pub fn start(dir: &Path) -> Self {
        let port = free_port();
        let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind server");
        let shutdown = Arc::new(AtomicBool::new(false));
        let root = dir.to_path_buf();
        let flag = Arc::clone(&shutdown);
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(s) = stream {
                    let root = root.clone();
                    std::thread::spawn(move || serve_one(s, &root));
                }
            }
            // The listener is dropped here, so later connections are refused
            // rather than accepted and ignored.
        });
        Self { port, shutdown, handle: Some(handle) }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}/{}", self.port, path.trim_start_matches('/'))
    }

    /// Stop answering, and stop listening: a later connection is refused.
    pub fn kill(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Unblock the accept loop so it notices the flag and drops the socket.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }

    pub fn is_up(&self) -> bool {
        TcpStream::connect_timeout(
            &format!("127.0.0.1:{}", self.port).parse().unwrap(),
            Duration::from_millis(500),
        )
        .is_ok()
    }
}

impl Drop for StaticServer {
    fn drop(&mut self) {
        if self.handle.is_some() {
            self.kill();
        }
    }
}

fn serve_one(mut s: TcpStream, root: &Path) {
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buf = [0u8; 2048];
    let n = match s.read(&mut buf) {
        Ok(n) if n > 0 => n,
        _ => return,
    };
    let head = String::from_utf8_lossy(&buf[..n]).to_string();
    let path = head.split_whitespace().nth(1).unwrap_or("/").split('?').next().unwrap_or("/").to_string();
    // No `..`, no absolute escape: this is a test server but it still serves a
    // directory and nothing above it.
    let name = Path::new(&path).file_name().and_then(|x| x.to_str()).unwrap_or("");
    let file = root.join(name);
    match std::fs::read(&file) {
        Ok(body) if !name.is_empty() => {
            let kind = if name.ends_with(".m3u8") { "application/vnd.apple.mpegurl" } else { "video/mp2t" };
            let _ = s.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            );
            let _ = s.write_all(&body);
        }
        _ => {
            let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    }
    let _ = s.flush();
}

/// An RTMP receiver that writes exactly what arrives, no re-encode.
pub fn spawn_rtmp_sink(port: u16, out: &Path, secs: u32) -> Child {
    Command::new(ffmpeg())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-listen",
            "1",
            "-timeout",
            &(secs + 20).to_string(),
            "-i",
            &format!("rtmp://127.0.0.1:{port}/live/test"),
            "-c",
            "copy",
            "-t",
            &secs.to_string(),
            out.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("rtmp sink")
}

/* ------------------------------------------------- reading what arrived */

/// `(codec, width, height)` of the first video stream.
pub fn video_of(file: &Path) -> Option<(String, u32, u32)> {
    let o = Command::new(ffprobe())
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,width,height",
            "-of",
            "csv=p=0",
            file.to_str().unwrap(),
        ])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() < 3 {
        return None;
    }
    Some((parts[0].to_string(), parts[1].parse().ok()?, parts[2].parse().ok()?))
}

pub fn stream_count(file: &Path, kind: &str) -> usize {
    let o = Command::new(ffprobe())
        .args([
            "-v",
            "error",
            "-select_streams",
            if kind == "video" { "v" } else { "a" },
            "-show_entries",
            "stream=index",
            "-of",
            "csv=p=0",
            file.to_str().unwrap(),
        ])
        .output()
        .expect("ffprobe");
    String::from_utf8_lossy(&o.stdout).lines().filter(|l| !l.trim().is_empty()).count()
}

/// Average colour of the first frame, as `(r, g, b)`.
pub fn first_frame_rgb(file: &Path) -> (f64, f64, f64) {
    let o = Command::new(ffmpeg())
        .args([
            "-v",
            "error",
            "-i",
            file.to_str().unwrap(),
            "-vframes",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-",
        ])
        .output()
        .expect("ffmpeg frame");
    let d = &o.stdout;
    assert!(!d.is_empty(), "no frame decoded from {}", file.display());
    let n = (d.len() / 3) as f64;
    let (mut r, mut g, mut b) = (0f64, 0f64, 0f64);
    for px in d.chunks_exact(3) {
        r += px[0] as f64;
        g += px[1] as f64;
        b += px[2] as f64;
    }
    (r / n, g / n, b / n)
}

/// Dominant frequency of the audio, by counting zero crossings.
///
/// Exact enough for a pure tone, which is all these fixtures contain, and it
/// needs no FFT dependency.
pub fn dominant_hz(file: &Path, seconds: u32) -> f64 {
    const RATE: u32 = 8000;
    let o = Command::new(ffmpeg())
        .args([
            "-v",
            "error",
            "-i",
            file.to_str().unwrap(),
            "-ac",
            "1",
            "-ar",
            &RATE.to_string(),
            "-f",
            "s16le",
            "-t",
            &seconds.to_string(),
            "-",
        ])
        .output()
        .expect("ffmpeg audio");
    let d = &o.stdout;
    assert!(d.len() > 4000, "almost no audio decoded from {}", file.display());
    let samples: Vec<i16> = d.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
    let crossings = samples.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count();
    let secs = samples.len() as f64 / RATE as f64;
    crossings as f64 / (2.0 * secs)
}

/// Last presentation timestamp of a stream, for telling "the picture stopped
/// but the sound did not" from "both ended".
pub fn last_pts(file: &Path, sel: &str) -> f64 {
    let o = Command::new(ffprobe())
        .args([
            "-v",
            "error",
            "-select_streams",
            sel,
            "-show_entries",
            "packet=pts_time",
            "-of",
            "csv=p=0",
            file.to_str().unwrap(),
        ])
        .output()
        .expect("ffprobe pts");
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| l.trim().parse::<f64>().ok())
        .fold(0.0, f64::max)
}

/// Wait for a predicate, polling, up to a limit.
pub fn wait_until(limit: Duration, mut f: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + limit;
    while Instant::now() < until {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}
