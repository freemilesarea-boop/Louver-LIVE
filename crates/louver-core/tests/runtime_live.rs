//! The runtime driving a real FFmpeg, end to end (§30, §64).
//!
//! `runtime_scheduling.rs` proves the loop's decisions with a fake process;
//! this proves the same code path actually produces a broadcast. It builds real
//! media, normalizes it through the real normalizer, resolves it through the
//! real playlist engine, and runs the real supervisor against real FFmpeg —
//! with a local file standing in for YouTube, which is exactly what the app's
//! Dry Run does.

mod common;

use common::*;
use louver_core::clock::{Clock, SystemClock};
use louver_core::config::OutputProfile;
use louver_core::database::models::EventLevel;
use louver_core::database::models::{Media, MediaStatus};
use louver_core::database::Database;
use louver_core::media::cache::MediaCache;
use louver_core::media::normalize::{normalize_one, CancelToken};
use louver_core::media::probe::probe;
use louver_core::runtime::{
    BroadcastRuntime, FfmpegLauncher, NullEvents, RuntimeEvents, StartOptions, StartReason, StreamLauncher,
};
use louver_core::security::{MemorySecretStore, StreamKeyStore};
use louver_core::session::SessionStore;
use louver_core::streaming::ffmpeg::FfmpegCommandBuilder;
use louver_core::streaming::state::StreamState;
use louver_core::system::NoopSleepPreventer;
use louver_core::PlaybackMode;
use std::sync::Mutex;

/// Captures the runtime's own log so a failure explains itself.
#[derive(Default)]
struct LogRecorder {
    lines: Mutex<Vec<String>>,
}

impl RuntimeEvents for LogRecorder {
    fn on_status(&self, _s: &louver_core::runtime::RuntimeStatus) {}
    fn on_log(&self, level: EventLevel, message: &str) {
        self.lines.lock().unwrap().push(format!("[{level:?}] {message}"));
    }
}

impl LogRecorder {
    fn dump(&self) -> String {
        self.lines.lock().unwrap().join("\n")
    }
}

/// Shared buffer for FFmpeg's own output, for the same reason.
type LogSink = Arc<Mutex<Vec<String>>>;

fn dump(sink: &LogSink) -> String {
    sink.lock().unwrap().join("\n")
}
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

const PROFILE: OutputProfile = OutputProfile::P720p30;

fn fixture_dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn wait_until(mut f: impl FnMut() -> bool, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Build a database with a playlist of three genuinely normalized videos.
fn seeded_db(b: &FfmpegCommandBuilder, cache: &MediaCache, dir: &std::path::Path) -> (Database, i64) {
    let db = Database::open(&dir.join("louver.db")).unwrap();
    let playlist = db.create_playlist("Night Jazz", PlaybackMode::Sequential, PROFILE).unwrap();

    let specs = loop_fixtures();
    let sources = make_fixtures(&FfmpegTools::clone(b.tools()), &fixture_dir(), &specs);

    for (i, src) in sources.iter().enumerate() {
        let info = probe(b, src).expect("probe failed");
        let out =
            normalize_one(b, cache, src, &format!("live{i}"), &info, PROFILE, &CancelToken::new(), |_| {})
                .expect("normalize failed");

        let id = db
            .upsert_media(&Media {
                id: 0,
                source_path: src.to_string_lossy().into_owned(),
                display_name: format!("night{:02}.mp4", i + 1),
                status: MediaStatus::Normalized,
                media_hash: format!("live{i}"),
                normalized_path: Some(out.output_path.to_string_lossy().into_owned()),
                normalized_profile: Some(PROFILE.id().into()),
                duration_secs: info.duration_secs,
                normalized_duration_secs: Some(out.duration_secs),
                width: PROFILE.width(),
                height: PROFILE.height(),
                fps: 30.0,
                video_codec: "h264".into(),
                audio_codec: Some("aac".into()),
                pixel_format: Some("yuv420p".into()),
                is_hdr: false,
                file_size: out.bytes,
                added_at: String::new(),
                last_error: None,
            })
            .unwrap();
        db.add_playlist_item(playlist, id).unwrap();
    }
    (db, playlist)
}

use louver_core::streaming::ffmpeg::FfmpegTools;

fn runtime_with(
    db: Database,
    b: FfmpegCommandBuilder,
    dir: &std::path::Path,
    events: Arc<dyn RuntimeEvents>,
    ffmpeg_log: LogSink,
) -> BroadcastRuntime {
    let launcher: Arc<dyn StreamLauncher> = Arc::new(FfmpegLauncher {
        program: b.tools().ffmpeg.clone(),
        log: Arc::new(move |l: &str| ffmpeg_log.lock().unwrap().push(l.to_string())),
    });
    let keys = Arc::new(StreamKeyStore::new(Arc::new(MemorySecretStore::new())));
    keys.set("abcd-efgh-ijkl-mnop").unwrap();

    BroadcastRuntime::new(
        db,
        b,
        launcher,
        Arc::new(SystemClock) as Arc<dyn Clock>,
        keys,
        Arc::new(NoopSleepPreventer::default()),
        events,
        SessionStore::new(dir.join("session.json")),
        dir.join("manifest.txt"),
        dir.join("dry-run"),
    )
}

#[test]
fn the_runtime_drives_real_ffmpeg_and_produces_a_playable_broadcast() {
    let tools = require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let b = builder(tools.clone(), PROFILE);
    let cache = MediaCache::new(dir.path().join("cache"));
    let (db, playlist) = seeded_db(&b, &cache, dir.path());

    let ffmpeg_log: LogSink = Arc::new(Mutex::new(Vec::new()));
    let mut rt =
        runtime_with(db.clone(), b.clone(), dir.path(), Arc::new(NullEvents), Arc::clone(&ffmpeg_log));

    // A local test publishes to a local RTMP ingest when one is listening and
    // to a file when none is. This test is about the file, so it says so
    // rather than depending on what happens to be listening on this machine.
    db.set_setting(louver_core::settings_keys::LOCAL_TEST_URL, "").unwrap();

    rt.start(StartOptions {
        playlist_id: playlist,
        reason: StartReason::Manual,
        dry_run: true, // a local file instead of RTMPS (§30)
        scheduled_end: None,
        occurrence: None,
        order_seed: Some(7),
    })
    .expect("the runtime failed to start a broadcast");

    // The plan is real: three normalized files, in order, with a manifest.
    let plan = rt.plan().expect("no session plan").clone();
    assert_eq!(plan.items.len(), 3);
    assert_eq!(plan.items[0].display_name, "night01.mp4");
    let manifest = std::fs::read_to_string(&plan.manifest_path).unwrap();
    assert!(manifest.starts_with("ffconcat version 1.0"));
    assert_eq!(manifest.lines().count(), 4);

    // Let the real FFmpeg run, ticking the loop as the app does.
    assert!(
        wait_until(
            || {
                rt.tick();
                rt.state() == StreamState::Live
            },
            Duration::from_secs(30),
        ),
        "the broadcast never reached LIVE (state: {})\nffmpeg:\n{}",
        rt.state(),
        dump(&ffmpeg_log)
    );

    // The supervisor sees a real pid, and progress is really advancing.
    let status = rt.status();
    assert!(status.supervisor.pid.is_some(), "no ffmpeg pid");
    assert_eq!(status.item_count, 3);
    assert!(status.current_item.is_some());
    assert!(status.next_item.is_some());

    // Run past one full cycle so the loop seam is actually exercised. The
    // playlist is 6s and `-re` paces at wall-clock speed.
    let ran_until = Instant::now() + Duration::from_secs(9);
    while Instant::now() < ran_until {
        std::thread::sleep(Duration::from_millis(250));
        rt.tick();
        assert_eq!(rt.state(), StreamState::Live, "broadcast dropped out mid-run");
    }
    // Stream copy encodes nothing, so FFmpeg reports no frame count. Bytes and
    // media time are the real signals that data is flowing.
    let p = rt.status().supervisor.progress;
    assert_eq!(p.frames, 0, "stream copy should not be encoding frames");
    assert!(p.total_bytes > 0, "ffmpeg pushed no bytes: {p:?}");
    assert!(p.out_time_ms > 0, "ffmpeg reported no media time: {p:?}");
    assert!(p.bitrate_kbps > 0.0, "no bitrate reported: {p:?}");

    // The session was recorded, and the state file says it is live.
    assert_eq!(db.unfinished_sessions().unwrap().len(), 1);
    let saved = SessionStore::new(dir.path().join("session.json")).load().expect("no state file");
    assert_eq!(saved.playlist_id, playlist);
    assert_eq!(saved.order_seed, 7);

    rt.stop(true).unwrap();
    assert_eq!(rt.state(), StreamState::Stopped);
    assert!(db.unfinished_sessions().unwrap().is_empty(), "session left open");
    assert!(SessionStore::new(dir.path().join("session.json")).load().is_none());

    // And the output is a real, playable stream with the expected properties.
    let out = dir.path().join("dry-run").join("dry-run.flv");
    assert!(out.is_file(), "no output file was produced");
    assert!(std::fs::metadata(&out).unwrap().len() > 50_000, "output is suspiciously small");
    assert_eq!(stream_field(&tools, &out, "v:0", "codec_name"), "h264");
    assert_eq!(stream_field(&tools, &out, "a:0", "codec_name"), "aac");
    assert_eq!(stream_field(&tools, &out, "v:0", "width"), PROFILE.width().to_string());
    assert_eq!(stream_field(&tools, &out, "v:0", "height"), PROFILE.height().to_string());

    // It looped: the playlist is 6s and we ran for ~9s, so the output must
    // contain more than one pass through the playlist.
    let dur = format_duration(&tools, &out);
    assert!(dur > 6.5, "output is {dur:.2}s — the playlist did not loop past its 6s cycle");
}

#[test]
fn a_broadcast_that_loses_its_ffmpeg_comes_back_on_its_own() {
    let tools = require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let b = builder(tools.clone(), PROFILE);
    let cache = MediaCache::new(dir.path().join("cache"));
    let (db, playlist) = seeded_db(&b, &cache, dir.path());

    let recorder = Arc::new(LogRecorder::default());
    let ffmpeg_log: LogSink = Arc::new(Mutex::new(Vec::new()));
    let mut rt = runtime_with(
        db,
        b,
        dir.path(),
        Arc::clone(&recorder) as Arc<dyn RuntimeEvents>,
        Arc::clone(&ffmpeg_log),
    );
    rt.start(StartOptions {
        playlist_id: playlist,
        reason: StartReason::Manual,
        dry_run: true,
        scheduled_end: None,
        occurrence: None,
        order_seed: Some(1),
    })
    .unwrap();

    assert!(wait_until(
        || {
            rt.tick();
            rt.state() == StreamState::Live
        },
        Duration::from_secs(30)
    ));
    let first_pid = rt.ffmpeg_pid().expect("no pid");

    // Kill FFmpeg the way the developer menu's crash simulation does (§59).
    rt.simulate_crash().expect("could not kill ffmpeg");

    assert!(
        wait_until(
            || {
                rt.tick();
                rt.state() == StreamState::Reconnecting
            },
            Duration::from_secs(15)
        ),
        "a killed ffmpeg did not put the runtime into RECONNECTING (state: {})\nffmpeg:\n{}",
        rt.state(),
        dump(&ffmpeg_log)
    );

    // The backoff is real (2s); skip it rather than sleeping through it.
    rt.force_restart_due();
    assert!(
        wait_until(
            || {
                rt.tick();
                rt.state() == StreamState::Live
            },
            Duration::from_secs(30)
        ),
        "the broadcast did not recover (state: {})\nruntime log:\n{}\n\nffmpeg:\n{}\n\nlast error: {:?}",
        rt.state(),
        recorder.dump(),
        dump(&ffmpeg_log),
        rt.status().supervisor.last_error,
    );

    let second_pid = rt.ffmpeg_pid().expect("no pid after recovery");
    assert_ne!(second_pid, first_pid, "a new ffmpeg should be running");
    assert!(rt.status().supervisor.restart_count >= 1);

    rt.stop(true).unwrap();
    assert!(
        wait_until(
            || !louver_core::system::MetricsCollector::new().is_ffmpeg_process(second_pid),
            Duration::from_secs(10),
        ),
        "ffmpeg survived the stop"
    );
}
