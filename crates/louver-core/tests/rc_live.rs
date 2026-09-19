//! Release-candidate broadcast harness (§5, §11, §31).
//!
//! These tests are `#[ignore]`d: they take minutes to hours and need an ingest
//! endpoint, so they never run in `npm run verify`. They are driven by
//! environment variables and are the same harness for both cases the release
//! needs:
//!
//! * a **local RTMP sink**, which is the default, exercising the real RTMP
//!   handshake, socket and reconnect path without a YouTube account; and
//! * **real YouTube**, by setting `LOUVER_TEST_RTMPS_URL` to the ingest URL and
//!   `LOUVER_TEST_STREAM_KEY` to the key — no code change (§4).
//!
//! The stream key is read from the environment straight into the in-memory
//! secret store and is never printed, written to the CSV, or put in the
//! summary: the destination is masked everywhere it is reported (§17).
//!
//! ```bash
//! # local RTMP, 30 minutes
//! node scripts/rtmp-sink.mjs --port 1935 --out rc-results/ingest &
//! LOUVER_RC_DURATION_SECS=1800 \
//!   cargo test -p louver-core --test rc_live -- --ignored --nocapture rc_broadcast
//!
//! # real YouTube (private/unlisted stream)
//! LOUVER_TEST_RTMPS_URL=rtmps://a.rtmps.youtube.com/live2 \
//! LOUVER_TEST_STREAM_KEY=… LOUVER_RC_DURATION_SECS=1800 \
//!   cargo test -p louver-core --test rc_live -- --ignored --nocapture rc_broadcast
//! ```

mod common;

// `common` is reached through the require_ffmpeg! macro and helper paths.

use louver_core::clock::{Clock, SystemClock};
use louver_core::config::OutputProfile;
use louver_core::database::models::{EventLevel, Media, MediaStatus};
use louver_core::database::Database;
use louver_core::media::cache::MediaCache;
use louver_core::media::normalize::{normalize_one, CancelToken};
use louver_core::media::probe::probe;
use louver_core::runtime::{
    BroadcastRuntime, FfmpegLauncher, RuntimeEvents, RuntimeStatus, StartOptions, StartReason, StreamLauncher,
};
use louver_core::security::{build_ingest_url, MemorySecretStore, StreamKeyStore};
use louver_core::session::SessionStore;
use louver_core::streaming::ffmpeg::{mask_secrets, FfmpegCommandBuilder, FfmpegTools};
use louver_core::streaming::state::StreamState;
use louver_core::system::{MetricsCollector, NoopSleepPreventer};
use louver_core::PlaybackMode;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PROFILE: OutputProfile = OutputProfile::P1080p30;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn results_dir() -> PathBuf {
    let d =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(env_or("LOUVER_RC_OUT", "rc-results"));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn rc_fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/rc")
}

/// Records everything the runtime says, so a failure is explainable.
#[derive(Default)]
struct Recorder {
    logs: Mutex<Vec<String>>,
    states: Mutex<Vec<(String, StreamState)>>,
}

impl RuntimeEvents for Recorder {
    fn on_status(&self, s: &RuntimeStatus) {
        let mut st = self.states.lock().unwrap();
        if st.last().map(|(_, p)| *p) != Some(s.supervisor.state) {
            st.push((now_stamp(), s.supervisor.state));
        }
    }
    fn on_log(&self, level: EventLevel, message: &str) {
        // Masked again on the way in, so nothing can reach the transcript.
        self.logs.lock().unwrap().push(format!("[{}] [{level:?}] {}", now_stamp(), mask_secrets(message)));
    }
}

fn now_stamp() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// The five RC fixtures, normalized once into a cache.
fn seed(b: &FfmpegCommandBuilder, cache: &MediaCache, db: &Database) -> (i64, f64) {
    let playlist = db.create_playlist("RC Playlist", PlaybackMode::Sequential, PROFILE).unwrap();
    let mut names: Vec<PathBuf> = std::fs::read_dir(rc_fixture_dir())
        .expect("run the RC fixture builder first (see TESTING.md)")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "mp4"))
        .collect();
    names.sort();
    assert!(names.len() >= 5, "§5 needs at least 5 source videos, found {}", names.len());

    let mut cycle = 0.0;
    for (i, src) in names.iter().enumerate() {
        let info = probe(b, src).expect("probe failed");
        eprintln!(
            "  normalizing {} ({:.0}s, {}x{}, {:.2}fps)…",
            src.file_name().unwrap().to_string_lossy(),
            info.duration_secs,
            info.width,
            info.height,
            info.fps
        );
        let out =
            normalize_one(b, cache, src, &format!("rc{i}"), &info, PROFILE, &CancelToken::new(), |_| {})
                .expect("normalize failed");
        cycle += out.duration_secs;

        let id = db
            .upsert_media(&Media {
                id: 0,
                source_path: src.to_string_lossy().into_owned(),
                display_name: src.file_name().unwrap().to_string_lossy().into_owned(),
                status: MediaStatus::Normalized,
                media_hash: format!("rc{i}"),
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
    (playlist, cycle)
}

struct Harness {
    rt: BroadcastRuntime,
    recorder: Arc<Recorder>,
    ffmpeg_log: Arc<Mutex<Vec<String>>>,
    playlist: i64,
    cycle_secs: f64,
    _dir: tempfile::TempDir,
}

fn harness(tools: FfmpegTools) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let b = FfmpegCommandBuilder::new(tools, PROFILE);
    let cache = MediaCache::new(dir.path().join("cache"));
    let db = Database::open(&dir.path().join("louver.db")).unwrap();
    let (playlist, cycle_secs) = seed(&b, &cache, &db);

    let recorder = Arc::new(Recorder::default());
    let ffmpeg_log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&ffmpeg_log);
    let launcher: Arc<dyn StreamLauncher> = Arc::new(FfmpegLauncher {
        program: b.tools().ffmpeg.clone(),
        log: Arc::new(move |l: &str| {
            let mut v = sink.lock().unwrap();
            v.push(l.to_string());
            if v.len() > 4000 {
                v.remove(0);
            }
        }),
    });

    // The key comes from the environment and goes straight into the in-memory
    // store. It is never logged (§4, §17).
    let keys = Arc::new(StreamKeyStore::new(Arc::new(MemorySecretStore::new())));
    keys.set(&env_or("LOUVER_TEST_STREAM_KEY", "rc-test")).unwrap();
    db.set_setting(
        louver_core::settings_keys::RTMPS_URL,
        &env_or("LOUVER_TEST_RTMPS_URL", "rtmp://127.0.0.1:1935/live"),
    )
    .unwrap();

    let rt = BroadcastRuntime::new(
        db,
        b,
        launcher,
        Arc::new(SystemClock) as Arc<dyn Clock>,
        keys,
        Arc::new(NoopSleepPreventer::default()),
        Arc::clone(&recorder) as Arc<dyn RuntimeEvents>,
        SessionStore::new(dir.path().join("session.json")),
        dir.path().join("manifest.txt"),
        dir.path().join("dry-run"),
    );

    Harness { rt, recorder, ffmpeg_log, playlist, cycle_secs, _dir: dir }
}

/// Where we are publishing, with any key masked.
fn destination_label() -> String {
    let url = env_or("LOUVER_TEST_RTMPS_URL", "rtmp://127.0.0.1:1935/live");
    let key = env_or("LOUVER_TEST_STREAM_KEY", "rc-test");
    mask_secrets(&build_ingest_url(&url, &key))
}

struct Sample {
    elapsed: f64,
    app_rss: u64,
    app_cpu: f32,
    ff_rss: u64,
    ff_cpu: f32,
    pid: Option<u32>,
    state: StreamState,
    reconnects: u32,
    bytes: u64,
}

fn write_csv(path: &Path, samples: &[Sample]) {
    let mut f = std::fs::File::create(path).unwrap();
    writeln!(f, "elapsed_s,app_rss_bytes,app_cpu_pct,ffmpeg_rss_bytes,ffmpeg_cpu_pct,ffmpeg_pid,state,reconnects,bytes_sent").unwrap();
    for s in samples {
        writeln!(
            f,
            "{:.0},{},{:.2},{},{:.2},{},{},{},{}",
            s.elapsed,
            s.app_rss,
            s.app_cpu,
            s.ff_rss,
            s.ff_cpu,
            s.pid.map(|p| p.to_string()).unwrap_or_default(),
            s.state,
            s.reconnects,
            s.bytes
        )
        .unwrap();
    }
}

/// Steady-state growth: the second half only, so buffer warm-up is excluded.
fn steady_growth_pct(values: &[u64]) -> f64 {
    let half = &values[values.len() / 2..];
    match (half.first(), half.last()) {
        (Some(&a), Some(&b)) if a > 0 => ((b as f64 - a as f64) / a as f64) * 100.0,
        _ => 0.0,
    }
}

/// The main release-candidate broadcast run (§5, §11).
///
/// Duration comes from `LOUVER_RC_DURATION_SECS` (default 1800 = 30 minutes).
#[test]
#[ignore = "release-candidate harness: needs an ingest endpoint and takes minutes to hours"]
fn rc_broadcast() {
    let tools = require_ffmpeg!();
    let duration = Duration::from_secs(
        env_or("LOUVER_RC_DURATION_SECS", "1800").parse().expect("bad LOUVER_RC_DURATION_SECS"),
    );
    let sample_every = Duration::from_secs(
        env_or("LOUVER_RC_SAMPLE_SECS", "300").parse().expect("bad LOUVER_RC_SAMPLE_SECS"),
    );
    let label = env_or("LOUVER_RC_LABEL", "rc-broadcast");
    let out_dir = results_dir();

    eprintln!("\n=== Louver Live RC broadcast ===");
    eprintln!("destination : {}", destination_label());
    eprintln!("duration    : {}s", duration.as_secs());
    eprintln!("profile     : {}", PROFILE.id());
    eprintln!("preparing fixtures…");

    let mut h = harness(tools.clone());
    eprintln!(
        "playlist    : 5 videos, cycle {:.1}s ({:.1} loops expected)",
        h.cycle_secs,
        duration.as_secs_f64() / h.cycle_secs
    );

    h.rt.start(StartOptions {
        playlist_id: h.playlist,
        reason: StartReason::Manual,
        dry_run: false, // a real RTMP publish
        scheduled_end: None,
        occurrence: None,
        order_seed: Some(1),
    })
    .expect("failed to start the broadcast");

    // Reaching LIVE means the ingest accepted the stream.
    let connect_started = Instant::now();
    let mut connected = false;
    while connect_started.elapsed() < Duration::from_secs(60) {
        h.rt.tick();
        if h.rt.state() == StreamState::Live {
            connected = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    assert!(
        connected,
        "never reached LIVE against {} (state {})\nruntime log:\n{}\nffmpeg:\n{}",
        destination_label(),
        h.rt.state(),
        h.recorder.logs.lock().unwrap().join("\n"),
        h.ffmpeg_log.lock().unwrap().join("\n"),
    );
    eprintln!("connected in {:.1}s — broadcasting\n", connect_started.elapsed().as_secs_f64());

    let mut metrics = MetricsCollector::new();
    let mut samples: Vec<Sample> = Vec::new();
    let started = Instant::now();
    let mut next_sample = Instant::now();
    let mut max_reconnects = 0;
    let mut dropped_out = 0;

    while started.elapsed() < duration {
        h.rt.tick();
        let st = h.rt.status();
        if st.supervisor.state != StreamState::Live {
            dropped_out += 1;
        }
        max_reconnects = max_reconnects.max(st.supervisor.reconnect_count);

        if Instant::now() >= next_sample {
            next_sample = Instant::now() + sample_every;
            let m = metrics.sample(st.supervisor.pid);
            let s = Sample {
                elapsed: started.elapsed().as_secs_f64(),
                app_rss: m.app_memory_bytes,
                app_cpu: m.app_cpu_percent,
                ff_rss: m.ffmpeg_memory_bytes,
                ff_cpu: m.ffmpeg_cpu_percent,
                pid: st.supervisor.pid,
                state: st.supervisor.state,
                reconnects: st.supervisor.reconnect_count,
                bytes: st.supervisor.progress.total_bytes,
            };
            eprintln!(
                "  {:>6.0}s  {:<12} app {:>6.1}MB/{:>4.1}%  ffmpeg {:>6.1}MB/{:>4.1}%  sent {:>7.1}MB  reconnects {}",
                s.elapsed, s.state.as_str(),
                s.app_rss as f64 / 1048576.0, s.app_cpu,
                s.ff_rss as f64 / 1048576.0, s.ff_cpu,
                s.bytes as f64 / 1048576.0, s.reconnects,
            );
            samples.push(s);
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    let final_status = h.rt.status();
    h.rt.stop(true).expect("stop failed");

    // --- report ----------------------------------------------------------
    let csv = out_dir.join(format!("{label}.csv"));
    write_csv(&csv, &samples);
    std::fs::write(out_dir.join(format!("{label}-runtime.log")), h.recorder.logs.lock().unwrap().join("\n"))
        .unwrap();
    std::fs::write(out_dir.join(format!("{label}-ffmpeg.log")), h.ffmpeg_log.lock().unwrap().join("\n"))
        .unwrap();

    let app_rss: Vec<u64> = samples.iter().map(|s| s.app_rss).collect();
    let ff_rss: Vec<u64> = samples.iter().map(|s| s.ff_rss).collect();
    let ff_cpu: Vec<f32> = samples.iter().map(|s| s.ff_cpu).filter(|c| *c > 0.0).collect();
    let mean = |v: &[f32]| if v.is_empty() { 0.0 } else { v.iter().sum::<f32>() / v.len() as f32 };
    let peak = |v: &[f32]| v.iter().cloned().fold(0.0f32, f32::max);

    let elapsed = started.elapsed().as_secs_f64();
    let mbps = (final_status.supervisor.progress.total_bytes as f64 * 8.0) / elapsed / 1e6;
    let errors = h
        .ffmpeg_log
        .lock()
        .unwrap()
        .iter()
        .filter(|l| {
            let l = l.to_lowercase();
            (l.contains("error") || l.contains("non-monotonic") || l.contains("invalid"))
                && !l.contains("error during demuxing")
        })
        .count();

    let summary = serde_json::json!({
        "label": label,
        "destination": destination_label(),
        "profile": PROFILE.id(),
        "duration_seconds": elapsed.round(),
        "playlist_videos": 5,
        "cycle_seconds": h.cycle_secs,
        "loops_completed": (elapsed / h.cycle_secs * 10.0).round() / 10.0,
        "connect_seconds": connect_started.elapsed().as_secs_f64(),
        "samples": samples.len(),
        "app_rss_start": app_rss.first(),
        "app_rss_end": app_rss.last(),
        "app_rss_steady_growth_pct": (steady_growth_pct(&app_rss) * 100.0).round() / 100.0,
        "ffmpeg_rss_start": ff_rss.first(),
        "ffmpeg_rss_end": ff_rss.last(),
        "ffmpeg_rss_steady_growth_pct": (steady_growth_pct(&ff_rss) * 100.0).round() / 100.0,
        // f32 -> f64 reintroduces noise, so round after the widening.
        "ffmpeg_cpu_mean_pct": ((mean(&ff_cpu) as f64) * 100.0).round() / 100.0,
        "ffmpeg_cpu_peak_pct": ((peak(&ff_cpu) as f64) * 100.0).round() / 100.0,
        "bytes_sent": final_status.supervisor.progress.total_bytes,
        "throughput_mbps": (mbps * 100.0).round() / 100.0,
        "reconnects": max_reconnects,
        "restarts": final_status.supervisor.restart_count,
        "ticks_not_live": dropped_out,
        "ffmpeg_errors": errors,
        "state_transitions": h.recorder.states.lock().unwrap()
            .iter().map(|(t, s)| format!("{t} {s}")).collect::<Vec<_>>(),
    });
    let summary_path = out_dir.join(format!("{label}-summary.json"));
    std::fs::write(&summary_path, serde_json::to_string_pretty(&summary).unwrap()).unwrap();

    eprintln!("\n=== result ===");
    eprintln!("{}", serde_json::to_string_pretty(&summary).unwrap());
    eprintln!("\ncsv     : {}", csv.display());
    eprintln!("summary : {}", summary_path.display());

    // --- assertions ------------------------------------------------------
    assert!(
        elapsed / h.cycle_secs >= 1.0,
        "the run was shorter than one playlist cycle, so the loop seam was never crossed"
    );
    assert_eq!(
        final_status.supervisor.state,
        StreamState::Live,
        "the broadcast was not LIVE at the end of the run"
    );
    assert_eq!(errors, 0, "FFmpeg reported {errors} error(s); see {label}-ffmpeg.log");
    assert!(
        steady_growth_pct(&ff_rss).abs() < 2.0,
        "FFmpeg memory grew {:.2}% in steady state",
        steady_growth_pct(&ff_rss)
    );
    assert!(
        steady_growth_pct(&app_rss).abs() < 5.0,
        "the runtime's memory grew {:.2}% in steady state",
        steady_growth_pct(&app_rss)
    );
    assert!(mbps > 0.5, "throughput was only {mbps:.2} Mbps — was anything actually sent?");
}

/// Real network interruption during a live broadcast (§7).
///
/// The outage is produced by blackholing packets to the ingest port with an
/// iptables DROP rule, not by stopping the server. That distinction matters:
/// a stopped server answers with "connection refused" immediately and a
/// reconnect succeeds the moment it returns, which is not what losing Wi-Fi
/// looks like. A DROP rule makes connections hang exactly as a real outage
/// does, and is the only way to hold the broadcast down for a full minute.
///
/// Requires root (or CAP_NET_ADMIN); skips loudly otherwise.
#[test]
#[ignore = "release-candidate harness: needs an ingest endpoint and packet-filter privileges"]
fn rc_network_interruption() {
    let tools = require_ffmpeg!();
    let outage = Duration::from_secs(env_or("LOUVER_RC_OUTAGE_SECS", "60").parse().unwrap());
    let port: u16 = env_or("LOUVER_RC_SINK_PORT", "1937").parse().unwrap();

    if !iptables(&["-L", "-n"]) {
        eprintln!("SKIP: cannot manage iptables rules, so no genuine outage can be produced");
        return;
    }

    let mut h = harness(tools);
    h.rt.start(StartOptions {
        playlist_id: h.playlist,
        reason: StartReason::Manual,
        dry_run: false,
        scheduled_end: None,
        occurrence: None,
        order_seed: Some(1),
    })
    .expect("failed to start");

    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline && h.rt.state() != StreamState::Live {
        h.rt.tick();
        std::thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(h.rt.state(), StreamState::Live, "never went live");
    let pid_before = h.rt.ffmpeg_pid();
    eprintln!("LIVE (ffmpeg {pid_before:?}) — blackholing port {port} for {}s", outage.as_secs());

    // --- the outage -------------------------------------------------------
    let drop_rule =
        |action: &str| iptables(&[action, "OUTPUT", "-p", "tcp", "--dport", &port.to_string(), "-j", "DROP"]);
    assert!(drop_rule("-I"), "could not install the DROP rule");
    // Make sure it is removed even if an assertion below fails.
    let _guard = DropGuard(port);

    let outage_started = Instant::now();
    let mut saw_reconnecting = false;
    let mut ui_responses = 0;
    // The real backoff runs here. Forcing it would produce a tight respawn
    // loop and would not test the behaviour §7 actually asks about.
    while outage_started.elapsed() < outage {
        h.rt.tick();
        if h.rt.state() == StreamState::Reconnecting {
            saw_reconnecting = true;
        }
        // §7: the app must stay alive and answer the UI throughout.
        let st = h.rt.status();
        assert!(st.elapsed_secs >= 0);
        ui_responses += 1;
        std::thread::sleep(Duration::from_millis(500));
    }

    let during = h.rt.status();
    eprintln!(
        "outage held {}s: state {}, reconnect attempts {}, UI answered {} times",
        outage.as_secs(),
        during.supervisor.state,
        during.supervisor.reconnect_count,
        ui_responses
    );
    assert!(
        saw_reconnecting,
        "the runtime never entered RECONNECTING during a {}s blackhole (state {})",
        outage.as_secs(),
        during.supervisor.state
    );
    assert!(during.supervisor.reconnect_count > 0, "no reconnect was attempted during the outage");
    // The backoff must space attempts out rather than spinning. Over 60s the
    // schedule (2s, 5s, 10s, 20s, 20s…) allows only a handful of tries.
    assert!(
        during.supervisor.reconnect_count <= 12,
        "{} reconnect attempts in {}s is a spin, not a backoff",
        during.supervisor.reconnect_count,
        outage.as_secs()
    );
    assert!(ui_responses > 10, "the UI stopped being answerable during the outage");
    // The user's broadcast is not silently marked finished.
    assert_ne!(during.supervisor.state, StreamState::Stopped);

    // --- recovery ---------------------------------------------------------
    assert!(drop_rule("-D"), "could not remove the DROP rule");
    eprintln!("network restored — waiting for recovery");

    let recovery_started = Instant::now();
    let mut recovered = false;
    while recovery_started.elapsed() < Duration::from_secs(180) {
        h.rt.tick();
        if h.rt.state() == StreamState::Live {
            recovered = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    let after = h.rt.status();
    let transitions: Vec<String> =
        h.recorder.states.lock().unwrap().iter().map(|(t, s)| format!("{t} {s}")).collect();
    eprintln!("state transitions: {transitions:?}");

    assert!(
        recovered,
        "the broadcast did not recover within 180s of the network returning (state {})\ntransitions: {transitions:?}",
        after.supervisor.state
    );
    eprintln!(
        "recovered in {:.0}s after {} reconnect attempt(s)",
        recovery_started.elapsed().as_secs_f64(),
        during.supervisor.reconnect_count
    );

    // A genuinely new process is carrying the broadcast.
    let pid_after = h.rt.ffmpeg_pid();
    assert!(pid_after.is_some());
    assert_ne!(pid_after, pid_before, "the same ffmpeg somehow survived the outage");

    // §7: an explicit stop must still be distinguishable from the outage.
    h.rt.stop(true).expect("stop failed");
    assert_eq!(h.rt.state(), StreamState::Stopped);
    for _ in 0..20 {
        h.rt.tick();
        assert_eq!(h.rt.state(), StreamState::Stopped, "a user stop was undone by the reconnect logic");
    }

    // No zombie FFmpeg is left behind.
    let mut m = MetricsCollector::new();
    for pid in [pid_before, pid_after].into_iter().flatten() {
        assert!(!m.is_ffmpeg_process(pid), "ffmpeg {pid} survived the run");
    }
}

/// Runs an iptables command, reporting whether it succeeded.
fn iptables(args: &[&str]) -> bool {
    std::process::Command::new("iptables")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Removes the DROP rule even if the test panics, so a failure cannot leave
/// the machine's networking altered.
struct DropGuard(u16);

impl Drop for DropGuard {
    fn drop(&mut self) {
        let port = self.0.to_string();
        // The rule may already be gone; deleting twice is harmless.
        while iptables(&["-D", "OUTPUT", "-p", "tcp", "--dport", &port, "-j", "DROP"]) {}
    }
}

// ---------------------------------------------------------------------------
// §9 — the application dies mid-broadcast and is restarted
// ---------------------------------------------------------------------------

/// A process death mid-broadcast, then a restart, against a real endpoint.
///
/// This is the §9 scenario end to end: a broadcast running over real RTMP, the
/// process disappearing without a clean stop (leaving an orphan FFmpeg and a
/// session file that still says LIVE), and a fresh runtime on the same data
/// directory deciding what to do. Both halves are checked: inside the
/// schedule it must resume; outside it must not.
#[test]
#[ignore = "release-candidate harness: needs an ingest endpoint"]
fn rc_application_restart_recovery() {
    let tools = require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let b = FfmpegCommandBuilder::new(tools.clone(), PROFILE);
    let cache = MediaCache::new(dir.path().join("cache"));
    let db = Database::open(&dir.path().join("louver.db")).unwrap();
    let (playlist, _cycle) = seed(&b, &cache, &db);

    let url = env_or("LOUVER_TEST_RTMPS_URL", "rtmp://127.0.0.1:1935/live");
    db.set_setting(louver_core::settings_keys::RTMPS_URL, &url).unwrap();
    let session_path = dir.path().join("session.json");

    // A schedule that covers right now, so recovery has a window to resume into.
    let now = chrono::Local::now().naive_local();
    let start = (now - chrono::Duration::hours(1)).format("%H:%M").to_string();
    let end = (now + chrono::Duration::hours(2)).format("%H:%M").to_string();
    db.create_schedule(&louver_core::database::models::Schedule {
        id: 0,
        playlist_id: playlist,
        days_of_week: louver_core::database::models::DaysOfWeek::everyday(),
        start_time: start.clone(),
        end_time: end.clone(),
        enabled: true,
    })
    .unwrap();

    let build_runtime = |db: Database| {
        let keys = Arc::new(StreamKeyStore::new(Arc::new(MemorySecretStore::new())));
        keys.set(&env_or("LOUVER_TEST_STREAM_KEY", "rc-test")).unwrap();
        let launcher: Arc<dyn StreamLauncher> =
            Arc::new(FfmpegLauncher { program: b.tools().ffmpeg.clone(), log: Arc::new(|_| {}) });
        BroadcastRuntime::new(
            db,
            b.clone(),
            launcher,
            Arc::new(SystemClock) as Arc<dyn Clock>,
            keys,
            Arc::new(NoopSleepPreventer::default()),
            Arc::new(Recorder::default()) as Arc<dyn RuntimeEvents>,
            SessionStore::new(&session_path),
            dir.path().join("manifest.txt"),
            dir.path().join("dry-run"),
        )
    };

    // --- process 1: broadcasting, then killed ----------------------------
    let orphan_pid;
    {
        let mut rt = build_runtime(db.clone());
        rt.start(StartOptions {
            playlist_id: playlist,
            reason: StartReason::Scheduled,
            dry_run: false,
            scheduled_end: None,
            occurrence: None,
            order_seed: Some(4242),
        })
        .expect("failed to start");

        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline && rt.state() != StreamState::Live {
            rt.tick();
            std::thread::sleep(Duration::from_millis(200));
        }
        assert_eq!(rt.state(), StreamState::Live, "never went live");
        rt.tick(); // persist a LIVE session file
        orphan_pid = rt.ffmpeg_pid().expect("no ffmpeg pid");
        eprintln!("process 1 broadcasting, ffmpeg {orphan_pid}");
        // Dropped without stop(): exactly what a kill -9 leaves behind.
    }

    // The evidence a crash leaves.
    let left = SessionStore::new(&session_path).load().expect("no session file was written");
    assert_eq!(left.stream_state, StreamState::Live, "the state file should still say LIVE");
    assert!(!left.user_requested_stop);
    assert_eq!(left.order_seed, 4242);
    assert_eq!(left.ffmpeg_pid, Some(orphan_pid));
    assert_eq!(db.unfinished_sessions().unwrap().len(), 1, "a dangling session row should remain");
    assert!(
        MetricsCollector::new().is_ffmpeg_process(orphan_pid),
        "the orphaned ffmpeg should still be running"
    );

    // --- process 2: restarted inside the window, must resume -------------
    {
        let mut rt = build_runtime(db.clone());
        let killed = rt.clean_orphan_process();
        assert_eq!(killed, Some(orphan_pid), "the orphaned ffmpeg was not cleaned up (§33)");

        let notice = rt.recover_on_startup().expect("the user should be told about the crash");
        assert!(notice.contains("비정상 종료"), "{notice}");
        eprintln!("process 2 recovery notice: {notice}");

        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline && rt.state() != StreamState::Live {
            rt.tick();
            std::thread::sleep(Duration::from_millis(200));
        }
        assert_eq!(rt.state(), StreamState::Live, "the broadcast did not resume after a restart");
        assert_eq!(rt.status().start_reason, Some(StartReason::Recovered));
        // The replayed order matches what the crashed process had chosen.
        assert_eq!(rt.plan().unwrap().order_seed, 4242, "the play order was not replayed");

        let new_pid = rt.ffmpeg_pid().expect("no pid after recovery");
        assert_ne!(new_pid, orphan_pid);
        eprintln!("resumed on ffmpeg {new_pid}");

        // The dangling row from the crashed run was closed.
        assert!(db.unfinished_sessions().unwrap().len() <= 1, "the crashed session row was not closed out");
        rt.stop(true).expect("stop failed");
        assert!(!MetricsCollector::new().is_ffmpeg_process(new_pid));
    }

    // --- process 3: restarted OUTSIDE any window, must stay off ----------
    {
        // Move the schedule to a window that does not include now.
        let s = &db.list_schedules().unwrap()[0];
        let away_start = (now + chrono::Duration::hours(3)).format("%H:%M").to_string();
        let away_end = (now + chrono::Duration::hours(5)).format("%H:%M").to_string();
        db.update_schedule(&louver_core::database::models::Schedule {
            id: s.id,
            playlist_id: playlist,
            days_of_week: s.days_of_week,
            start_time: away_start,
            end_time: away_end,
            enabled: true,
        })
        .unwrap();

        // Put a crashed session back on disk.
        let mut crashed = left.clone();
        crashed.stream_state = StreamState::Live;
        crashed.scheduled_end = None;
        SessionStore::new(&session_path).save(&crashed).unwrap();

        let mut rt = build_runtime(db.clone());
        let notice = rt.recover_on_startup();
        assert!(!rt.is_active(), "resumed a broadcast outside its schedule (§32)");
        assert!(notice.is_some(), "the user should still be told the previous run crashed");
        assert!(SessionStore::new(&session_path).load().is_none(), "stale state should have been cleared");
        eprintln!("outside the window: stayed off, notice = {notice:?}");
    }
}

// ---------------------------------------------------------------------------
// §8 — FFmpeg is killed mid-broadcast
// ---------------------------------------------------------------------------

/// Kill FFmpeg during a real RTMP broadcast and watch the supervisor recover.
///
/// Also checks the half that is easy to forget: after the user stops
/// deliberately, nothing restarts, however the process ends.
#[test]
#[ignore = "release-candidate harness: needs an ingest endpoint"]
fn rc_ffmpeg_crash_recovery() {
    let tools = require_ffmpeg!();
    let mut h = harness(tools);

    h.rt.start(StartOptions {
        playlist_id: h.playlist,
        reason: StartReason::Manual,
        dry_run: false,
        scheduled_end: None,
        occurrence: None,
        order_seed: Some(1),
    })
    .expect("failed to start");

    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline && h.rt.state() != StreamState::Live {
        h.rt.tick();
        std::thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(h.rt.state(), StreamState::Live, "never went live");

    // Three consecutive kills, to confirm recovery is repeatable rather than a
    // one-off, and that the backoff resets after each success.
    let mut pids = vec![h.rt.ffmpeg_pid().expect("no pid")];
    for round in 1..=3 {
        eprintln!("round {round}: killing ffmpeg {:?}", pids.last());
        h.rt.simulate_crash().expect("could not kill ffmpeg");

        let d = Instant::now() + Duration::from_secs(30);
        while Instant::now() < d && h.rt.state() != StreamState::Reconnecting {
            h.rt.tick();
            std::thread::sleep(Duration::from_millis(200));
        }
        assert_eq!(
            h.rt.state(),
            StreamState::Reconnecting,
            "round {round}: a killed ffmpeg did not enter RECONNECTING"
        );
        assert!(h.rt.status().supervisor.last_error.is_some(), "round {round}: no error recorded");

        let d = Instant::now() + Duration::from_secs(90);
        while Instant::now() < d && h.rt.state() != StreamState::Live {
            h.rt.tick();
            std::thread::sleep(Duration::from_millis(250));
        }
        assert_eq!(h.rt.state(), StreamState::Live, "round {round}: did not recover");

        let pid = h.rt.ffmpeg_pid().expect("no pid after recovery");
        assert!(!pids.contains(&pid), "round {round}: the same pid came back");
        pids.push(pid);
        eprintln!("round {round}: recovered on ffmpeg {pid}");
    }

    let st = h.rt.status();
    assert_eq!(st.supervisor.restart_count, 3, "every kill should be counted");
    // The backoff resets after a successful reconnect, so it never escalates
    // across unrelated faults.
    assert_eq!(st.supervisor.reconnect_count, 0, "backoff should have reset after recovery");

    // §8: an explicit stop must not be undone.
    h.rt.stop(true).expect("stop failed");
    assert_eq!(h.rt.state(), StreamState::Stopped);
    for _ in 0..40 {
        h.rt.tick();
        assert_eq!(h.rt.state(), StreamState::Stopped, "a user stop was undone by the reconnect logic");
    }

    // No zombie left from any round.
    let mut m = MetricsCollector::new();
    for pid in &pids {
        assert!(!m.is_ffmpeg_process(*pid), "ffmpeg {pid} survived");
    }
    eprintln!("3 kills, 3 recoveries, no zombies, explicit stop honoured");
}
