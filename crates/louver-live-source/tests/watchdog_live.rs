//! The fault the investigation measured, and the watchdog that answers it.
//!
//! PHASE 1 measured this against a real HLS source: when a live source goes
//! away, FFmpeg does not exit. The picture froze at 17.4s while the sound ran
//! to 60.0s, and the process was still alive 60s after the cut. The playlist
//! input is looped, so the audio never ends, so the process never ends, so a
//! supervisor waiting for an exit waits for ever.
//!
//! These tests reproduce that and assert the worker now notices. They use a
//! **real-time** HLS source: with a finished playlist on loopback, FFmpeg
//! downloads the whole thing in the first second, and killing the source
//! afterwards would prove nothing — which is how the first attempt at this
//! measurement fooled itself.
//!
//! The fake resolver hands back a loopback URL on purpose. In production the
//! SSRF check lives inside [`louver_live_source::YtDlpResolver`], which is the
//! boundary that takes untrusted input; a test fake stands in for that boundary
//! and so is allowed to name 127.0.0.1. `resolver.rs`'s own tests cover the
//! case where a resolver tries to return an internal address.

mod common;

use common::*;
use louver_core::streaming::ffmpeg::FfmpegTools;
use louver_live_source::{
    resolver::{LiveSourceResolver, ResolvedSource},
    state::{Phase, StateStore},
    worker::{LiveWorker, Outcome, WorkerConfig},
    Result,
};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Stands in for yt-dlp: always resolves to one fixed stream.
struct FixedSource(String);

impl LiveSourceResolver for FixedSource {
    fn resolve(&self, _watch_url: &str) -> Result<ResolvedSource> {
        Ok(ResolvedSource {
            manifest_url: self.0.clone(),
            is_live: true,
            width: Some(1280),
            height: Some(720),
            title: Some("테스트 라이브".into()),
        })
    }
}

const WATCH: &str = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";

struct Rig {
    _dir: tempfile::TempDir,
    state_dir: std::path::PathBuf,
    store: StateStore,
    handle: Option<std::thread::JoinHandle<Result<Outcome>>>,
    sink: std::process::Child,
}

/// Start a source, a sink and a worker, and hand back the pieces a test pokes.
fn start(dir: tempfile::TempDir, hls: &Path, server: &StaticServer, max_restarts: u32) -> Rig {
    let manifest = make_playlist(dir.path());
    let state_dir = dir.path().join("state");
    let port = free_port();
    let sink = spawn_rtmp_sink(port, &dir.path().join("received.flv"), 120);
    std::thread::sleep(Duration::from_millis(1200));
    let _ = hls;

    let mut cfg =
        WorkerConfig::new("wd1", WATCH, &manifest, format!("rtmp://127.0.0.1:{port}/live/test"), &state_dir);
    cfg.tools = FfmpegTools::new(ffmpeg(), ffprobe());
    // Short on purpose: the real defaults are 12s and 20s, and a test that
    // waited those out three times over would take minutes.
    cfg.stall_after = Duration::from_secs(3);
    cfg.grace = Duration::from_secs(6);
    cfg.max_restarts = max_restarts;

    let store = StateStore::new(&state_dir, "wd1");
    let resolver = Arc::new(FixedSource(server.url("live.m3u8")));
    let handle = std::thread::spawn(move || {
        let mut w = LiveWorker::new(cfg, resolver);
        w.run()
    });
    Rig { _dir: dir, state_dir, store, handle: Some(handle), sink }
}

impl Rig {
    fn phase(&self) -> Option<Phase> {
        self.store.load().ok().map(|s| s.phase)
    }
    fn frames(&self) -> u64 {
        self.store.load().map(|s| s.frames).unwrap_or(0)
    }
    fn restarts(&self) -> u32 {
        self.store.load().map(|s| s.restarts).unwrap_or(0)
    }
    fn verdict(&self) -> String {
        self.store.load().map(|s| s.last_verdict).unwrap_or_default()
    }
    fn last_error(&self) -> String {
        self.store.load().ok().and_then(|s| s.last_error).unwrap_or_default()
    }
}

#[test]
#[ignore = "runs a real-time HLS source for a minute; run explicitly"]
fn a_source_that_stops_producing_segments_is_noticed_and_retried_then_given_up_on() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let hls = dir.path().join("hls");
    let mut generator = spawn_hls_realtime(&hls, "1280x720", 120);
    // Let a few segments exist before anyone reads the playlist.
    std::thread::sleep(Duration::from_secs(7));
    let server = StaticServer::start(&hls);
    let mut rig = start(dir, &hls, &server, 1);

    // 1. It gets going and real frames leave the encoder.
    assert!(
        wait_until(Duration::from_secs(40), || rig.phase() == Some(Phase::Sending) && rig.frames() > 0),
        "worker never started sending (phase={:?} frames={})",
        rig.phase(),
        rig.frames()
    );

    // 2. The source stops producing segments. The URL still answers 200 — this
    //    is what an ended broadcast looks like, and it is the case that hung.
    let _ = generator.kill();
    let _ = generator.wait();
    assert!(server.is_up(), "the endpoint must stay up: that is the point of this case");

    // 3. The watchdog fires and the worker restarts rather than sending a
    //    frozen frame for ever.
    assert!(
        wait_until(Duration::from_secs(60), || rig.restarts() >= 1),
        "the stall was never noticed (phase={:?} verdict={} err={})",
        rig.phase(),
        rig.verdict(),
        rig.last_error()
    );
    assert!(
        rig.last_error().contains("영상 정지") || rig.last_error().contains("연결이 끊어졌습니다"),
        "the recorded reason should name the fault, got {:?}",
        rig.last_error()
    );

    // 4. Attempts are bounded. With max_restarts = 1 it gives up rather than
    //    spinning on a stream that is not coming back.
    let handle = rig.handle.take().expect("worker thread");
    let outcome = handle.join().expect("worker thread").expect("run");
    match outcome {
        Outcome::GaveUp { restarts, reason } => {
            assert_eq!(restarts, 1, "bounded at max_restarts");
            assert!(!reason.is_empty());
        }
        other => panic!("expected GaveUp, got {other:?}"),
    }
    assert_eq!(rig.phase(), Some(Phase::GaveUp));
    let _ = rig.sink.kill();
    let _ = rig.sink.wait();
    // The state file is the worker's own, in the worker's own directory.
    assert!(rig.state_dir.join("worker-wd1.json").exists());
}

#[test]
#[ignore = "runs a real-time HLS source for a minute; run explicitly"]
fn an_endpoint_that_refuses_connections_is_also_noticed() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let hls = dir.path().join("hls");
    let mut generator = spawn_hls_realtime(&hls, "1280x720", 120);
    std::thread::sleep(Duration::from_secs(7));
    let mut server = StaticServer::start(&hls);
    let mut rig = start(dir, &hls, &server, 1);

    assert!(
        wait_until(Duration::from_secs(40), || rig.phase() == Some(Phase::Sending) && rig.frames() > 0),
        "worker never started sending"
    );

    // The other failure mode: the endpoint goes away entirely.
    server.kill();
    let _ = generator.kill();
    let _ = generator.wait();

    assert!(
        wait_until(Duration::from_secs(60), || rig.restarts() >= 1),
        "a refused endpoint was never noticed (phase={:?} err={})",
        rig.phase(),
        rig.last_error()
    );
    let handle = rig.handle.take().expect("worker thread");
    let outcome = handle.join().expect("worker thread").expect("run");
    assert!(matches!(outcome, Outcome::GaveUp { .. }), "expected GaveUp, got {outcome:?}");
    let _ = rig.sink.kill();
    let _ = rig.sink.wait();
}

#[test]
#[ignore = "runs a real-time HLS source for a minute; run explicitly"]
fn a_healthy_source_is_left_alone_for_the_whole_run() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    // The other half of the watchdog's job: not firing. A false positive would
    // restart a working broadcast, which is worse than the fault it guards.
    let dir = tempfile::tempdir().unwrap();
    let hls = dir.path().join("hls");
    let mut generator = spawn_hls_realtime(&hls, "1280x720", 90);
    std::thread::sleep(Duration::from_secs(7));
    let server = StaticServer::start(&hls);

    let manifest = make_playlist(dir.path());
    let state_dir = dir.path().join("state");
    let port = free_port();
    let mut sink = spawn_rtmp_sink(port, &dir.path().join("received.flv"), 40);
    std::thread::sleep(Duration::from_millis(1200));

    let mut cfg =
        WorkerConfig::new("ok1", WATCH, &manifest, format!("rtmp://127.0.0.1:{port}/live/test"), &state_dir);
    cfg.tools = FfmpegTools::new(ffmpeg(), ffprobe());
    cfg.stall_after = Duration::from_secs(3);
    cfg.grace = Duration::from_secs(6);
    cfg.run_for = Some(Duration::from_secs(30));
    let resolver = Arc::new(FixedSource(server.url("live.m3u8")));
    let mut w = LiveWorker::new(cfg, resolver);
    let outcome = w.run().expect("run");

    match outcome {
        Outcome::Stopped { frames, restarts } => {
            assert_eq!(restarts, 0, "a healthy source must not be restarted");
            assert!(frames > 100, "a 30s run at 30fps should send many frames, got {frames}");
        }
        other => panic!("expected Stopped, got {other:?}"),
    }
    let _ = generator.kill();
    let _ = generator.wait();
    let _ = sink.kill();
    let _ = sink.wait();
}
