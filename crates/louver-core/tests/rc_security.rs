//! Stream-key containment, verified end to end (§4, §17, §60).
//!
//! The unit tests prove each masking function in isolation. This proves the
//! property that actually matters: after a real broadcast attempt with a
//! realistic YouTube-shaped key, the key is not in the database, the session
//! file, any log file, the event table, or anything the UI is handed.
//!
//! It deliberately publishes to a closed port so FFmpeg fails immediately and
//! writes the publish URL into its stderr — which is exactly the path by which
//! a key would leak into a log.

mod common;

use louver_core::clock::{Clock, SystemClock};
use louver_core::config::OutputProfile;
use louver_core::database::models::{EventLevel, Media, MediaStatus};
use louver_core::database::Database;
use louver_core::logging::{LogTarget, Logger};
use louver_core::runtime::{
    BroadcastRuntime, FfmpegLauncher, RuntimeEvents, RuntimeStatus, StartOptions, StartReason, StreamLauncher,
};
use louver_core::security::{MemorySecretStore, StreamKeyStore};
use louver_core::session::SessionStore;
use louver_core::streaming::ffmpeg::{FfmpegCommandBuilder, FfmpegTools};
use louver_core::PlaybackMode;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A key with the exact shape YouTube issues.
const REAL_SHAPED_KEY: &str = "a1b2-c3d4-e5f6-g7h8-i9j0";

/// Every distinctive fragment of the key, so a partial leak is caught too.
fn fragments() -> Vec<&'static str> {
    vec![
        REAL_SHAPED_KEY,
        // The same key with its separators removed, in case something strips
        // them before logging.
        "a1b2c3d4e5f6g7h8i9j0",
        "a1b2",
        "c3d4",
        "e5f6",
        "g7h8",
        "i9j0",
    ]
}

/// Does `haystack` contain `needle` as a token rather than as part of a longer
/// run of letters and digits?
///
/// The short fragments are four hex characters each, and FFmpeg logs pointer
/// addresses like `0x5647e511a380`. Plain substring matching therefore fails
/// this test at random — roughly once in a few hundred runs — with a leak
/// report that is not a leak. A security check nobody trusts is a security
/// check nobody reads, so the match is anchored: a real leak of `c3d4` is
/// surrounded by a separator, a quote, whitespace or the end of the text,
/// never by more hex.
fn contains_as_token(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    let mut from = 0;
    while let Some(rel) = haystack[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        let before_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        let after_ok = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

fn assert_clean(what: &str, haystack: &str) {
    for f in fragments() {
        assert!(
            !contains_as_token(haystack, f),
            "STREAM KEY LEAK in {what}: found {:?}\n--- content ---\n{}",
            f,
            &haystack.chars().take(4000).collect::<String>()
        );
    }
}

#[test]
fn the_leak_check_catches_a_real_leak_but_not_a_pointer_address() {
    // A genuine leak, in the shapes it would actually take.
    assert!(contains_as_token("publishing to rtmps://x/live2/a1b2-c3d4-e5f6-g7h8-i9j0", REAL_SHAPED_KEY));
    assert!(contains_as_token("key=c3d4", "c3d4"));
    assert!(contains_as_token("\"c3d4\"", "c3d4"));
    assert!(contains_as_token("c3d4", "c3d4"));
    assert!(contains_as_token("a1b2-c3d4-e5f6", "c3d4"));

    // And the false alarm that used to fail this suite at random.
    assert!(!contains_as_token("[mov @ 0x5647e5c3d4a1] moov atom not found", "c3d4"));
    assert!(!contains_as_token("0xc3d4ef", "c3d4"));
    assert!(!contains_as_token("abc3d4", "c3d4"));
}

#[derive(Default)]
struct Sink {
    logs: Mutex<Vec<String>>,
    statuses: Mutex<Vec<String>>,
}

impl RuntimeEvents for Sink {
    fn on_status(&self, s: &RuntimeStatus) {
        // Whatever the UI receives, serialized exactly as the IPC layer sends it.
        if let Ok(j) = serde_json::to_string(s) {
            self.statuses.lock().unwrap().push(j);
        }
    }
    fn on_log(&self, _l: EventLevel, m: &str) {
        self.logs.lock().unwrap().push(m.to_string());
    }
}

/// Read every file under a directory into one string.
fn read_tree(dir: &Path) -> String {
    let mut out = String::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.push_str(&read_tree(&p));
        } else if let Ok(bytes) = std::fs::read(&p) {
            out.push_str(&format!("\n=== {} ===\n", p.display()));
            out.push_str(&String::from_utf8_lossy(&bytes));
        }
    }
    out
}

#[test]
fn a_real_broadcast_attempt_leaks_the_stream_key_nowhere() {
    let Some(tools) = common::tools() else {
        eprintln!("SKIP: no ffmpeg available");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let b = FfmpegCommandBuilder::new(tools, OutputProfile::P720p30);

    // A playlist of one broadcast-ready file.
    let db = Database::open(&dir.path().join("louver.db")).unwrap();
    let playlist = db.create_playlist("Sec", PlaybackMode::Sequential, OutputProfile::P720p30).unwrap();
    let media_file = dir.path().join("v.mp4");
    std::fs::write(&media_file, vec![0u8; 2048]).unwrap();
    let mid = db
        .upsert_media(&Media {
            id: 0,
            source_path: media_file.to_string_lossy().into_owned(),
            display_name: "v.mp4".into(),
            status: MediaStatus::Normalized,
            media_hash: "h".into(),
            normalized_path: Some(media_file.to_string_lossy().into_owned()),
            normalized_profile: Some("720p30".into()),
            duration_secs: 10.0,
            normalized_duration_secs: Some(10.0),
            width: 1280,
            height: 720,
            fps: 30.0,
            video_codec: "h264".into(),
            audio_codec: Some("aac".into()),
            pixel_format: Some("yuv420p".into()),
            is_hdr: false,
            file_size: 2048,
            added_at: String::new(),
            last_error: None,
        })
        .unwrap();
    db.add_playlist_item(playlist, mid).unwrap();

    // Port 1 is closed, so FFmpeg fails at once and prints the publish URL.
    db.set_setting(louver_core::settings_keys::RTMPS_URL, "rtmp://127.0.0.1:1/live").unwrap();

    let logger = Logger::new(dir.path().join("logs")).unwrap();
    let keys = Arc::new(StreamKeyStore::new(Arc::new(MemorySecretStore::new())));
    keys.set(REAL_SHAPED_KEY).unwrap();

    let sink = Arc::new(Sink::default());
    let file_logger = Arc::clone(&logger);
    let launcher: Arc<dyn StreamLauncher> = Arc::new(FfmpegLauncher {
        program: b.tools().ffmpeg.clone(),
        // Exactly what the desktop app does: FFmpeg output goes to the log file.
        log: Arc::new(move |l: &str| file_logger.info(LogTarget::Ffmpeg, l)),
    });

    let session_file = dir.path().join("session.json");
    let mut rt = BroadcastRuntime::new(
        db.clone(),
        b,
        launcher,
        Arc::new(SystemClock) as Arc<dyn Clock>,
        keys,
        Arc::new(louver_core::system::NoopSleepPreventer::default()),
        Arc::clone(&sink) as Arc<dyn RuntimeEvents>,
        SessionStore::new(&session_file),
        dir.path().join("manifest.txt"),
        dir.path().join("dry-run"),
    );

    rt.start(StartOptions {
        playlist_id: playlist,
        reason: StartReason::Manual,
        dry_run: false,
        scheduled_end: None,
        occurrence: None,
        order_seed: Some(1),
    })
    .expect("start failed");

    // Let it fail and retry a few times, producing plenty of error output.
    for _ in 0..40 {
        rt.tick();
        rt.force_restart_due();
        std::thread::sleep(Duration::from_millis(100));
    }
    let last_error = rt.status().supervisor.last_error.clone();
    rt.stop(true).ok();

    // --- now audit every surface -----------------------------------------

    // 1. The log files, which is where FFmpeg's stderr landed.
    let logs = read_tree(&dir.path().join("logs"));
    assert!(!logs.is_empty(), "no log output was produced, so this proves nothing");
    assert!(logs.contains("••••"), "expected masked output in the logs:\n{logs}");
    assert_clean("log files", &logs);

    // 2. The SQLite database, including the events table and any settings.
    let db_bytes = read_tree(dir.path());
    assert_clean("application data directory (db, session file, manifest)", &db_bytes);

    // 3. The event rows as the Logs page would read them.
    let events = db
        .recent_events(500)
        .unwrap()
        .iter()
        .map(|e| format!("{} {}", e.code.clone().unwrap_or_default(), e.message))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!events.is_empty(), "no events were recorded, so this proves nothing");
    assert_clean("stream_events table", &events);

    // 4. What the runtime pushed to the UI.
    let statuses = sink.statuses.lock().unwrap().join("\n");
    assert!(!statuses.is_empty());
    assert_clean("status payloads sent to the UI", &statuses);

    // 5. What the event sink was told.
    let runtime_logs = sink.logs.lock().unwrap().join("\n");
    assert_clean("runtime log messages", &runtime_logs);

    // 6. The error the UI would display, including its technical detail.
    let err = last_error.expect("a failed connection should record an error");
    assert_clean("error message", &err.message);
    assert_clean("error detail", err.detail.as_deref().unwrap_or(""));
    // And the user-facing half is Korean guidance, not FFmpeg output (§18).
    assert!(!err.message.contains("rtmp"), "raw URL surfaced as the user message");

    // 7. The session state file, which survives a crash.
    if session_file.exists() {
        assert_clean("session.json", &std::fs::read_to_string(&session_file).unwrap());
    }
}

#[test]
fn the_key_is_never_written_to_the_settings_table() {
    let db = Database::open_in_memory().unwrap();
    db.set_setting(louver_core::settings_keys::RTMPS_URL, "rtmps://a.rtmps.youtube.com/live2").unwrap();
    let all = db.all_settings().unwrap();
    let dump = all.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("\n");
    assert_clean("settings table", &dump);
    // The key has no settings row at all, by design.
    assert!(!all.iter().any(|(k, _)| k.contains("key")), "no settings key may hold a stream key: {dump}");
}

#[test]
fn a_panic_message_would_not_carry_the_key() {
    // §60 also forbids the key reaching a crash report. Errors are the only
    // structured thing a panic handler would have to serialize.
    let e = louver_core::LouverError::with_detail(
        louver_core::ErrorCode::NetworkRtmpRejected,
        format!("rtmps://a.rtmps.youtube.com/live2/{REAL_SHAPED_KEY} refused"),
    );
    assert_clean("serialized error", &serde_json::to_string(&e).unwrap());
    assert_clean("Display form", &e.to_string());
    assert_clean("Debug form", &format!("{e:?}"));
}

#[test]
fn the_ui_is_only_ever_given_a_masked_key() {
    let store = StreamKeyStore::new(Arc::new(MemorySecretStore::new()));
    store.set(REAL_SHAPED_KEY).unwrap();

    assert_clean("masked display", &store.masked());
    assert_clean(
        "key hint",
        &louver_core::security::key_hint(&store.get().unwrap().unwrap())
            // The hint deliberately shows the last four characters, which is
            // the one disclosure the spec asks for; exclude it from the scan.
            .replace("i9j0", "____"),
    );
    // Only an explicit reveal returns the real value.
    assert_eq!(store.require().unwrap(), REAL_SHAPED_KEY);
}

#[test]
fn ffmpeg_argv_logging_masks_the_destination_but_stays_useful() {
    let tools = FfmpegTools::new("ffmpeg", "ffprobe");
    let b = FfmpegCommandBuilder::new(tools, OutputProfile::P1080p30);
    let dest = louver_core::security::build_ingest_url("rtmps://a.rtmps.youtube.com/live2", REAL_SHAPED_KEY);
    let argv = b.build_stream_args(Path::new("/tmp/m.txt"), &dest, louver_core::StreamMode::StreamCopy, true);
    // The real argv must carry the key, or nothing would be broadcast.
    assert!(argv.iter().any(|a| a.contains(REAL_SHAPED_KEY)));

    let masked = louver_core::streaming::ffmpeg::mask_argv(&argv).join(" ");
    assert_clean("masked argv", &masked);
    // ...but it is still a readable command for debugging.
    assert!(masked.contains("-f concat"));
    assert!(masked.contains("-c copy"));
    assert!(masked.contains("rtmps://a.rtmps.youtube.com/live2/••••••••"));
}

// ---------------------------------------------------------------------------
// §13 — the running command is verifiable, not just configured
// ---------------------------------------------------------------------------

/// A live session must be provably stream copy, from its real argv.
#[test]
fn diagnostics_prove_a_live_session_is_stream_copy() {
    let Some(tools) = common::tools() else {
        eprintln!("SKIP: no ffmpeg available");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let b = FfmpegCommandBuilder::new(tools, OutputProfile::P720p30);
    let db = Database::open(&dir.path().join("louver.db")).unwrap();
    let playlist = db.create_playlist("Diag", PlaybackMode::Sequential, OutputProfile::P720p30).unwrap();
    let f = dir.path().join("v.mp4");
    std::fs::write(&f, vec![0u8; 2048]).unwrap();
    let mid = db
        .upsert_media(&Media {
            id: 0,
            source_path: f.to_string_lossy().into_owned(),
            display_name: "v.mp4".into(),
            status: MediaStatus::Normalized,
            media_hash: "h".into(),
            normalized_path: Some(f.to_string_lossy().into_owned()),
            normalized_profile: Some("720p30".into()),
            duration_secs: 10.0,
            normalized_duration_secs: Some(10.0),
            width: 1280,
            height: 720,
            fps: 30.0,
            video_codec: "h264".into(),
            audio_codec: Some("aac".into()),
            pixel_format: Some("yuv420p".into()),
            is_hdr: false,
            file_size: 2048,
            added_at: String::new(),
            last_error: None,
        })
        .unwrap();
    db.add_playlist_item(playlist, mid).unwrap();
    db.set_setting(louver_core::settings_keys::RTMPS_URL, "rtmp://127.0.0.1:1/live").unwrap();

    let keys = Arc::new(StreamKeyStore::new(Arc::new(MemorySecretStore::new())));
    keys.set(REAL_SHAPED_KEY).unwrap();
    let launcher: Arc<dyn StreamLauncher> =
        Arc::new(FfmpegLauncher { program: b.tools().ffmpeg.clone(), log: Arc::new(|_| {}) });
    let mut rt = BroadcastRuntime::new(
        db.clone(),
        b,
        launcher,
        Arc::new(SystemClock) as Arc<dyn Clock>,
        keys,
        Arc::new(louver_core::system::NoopSleepPreventer::default()),
        Arc::new(Sink::default()) as Arc<dyn RuntimeEvents>,
        SessionStore::new(dir.path().join("session.json")),
        dir.path().join("manifest.txt"),
        dir.path().join("dry-run"),
    );

    // Nothing running yet.
    let idle = rt.diagnostics();
    assert!(idle.masked_command.is_empty());
    assert!(idle.mismatch.is_none());
    assert!(idle.verdict.contains("방송 중이 아닙니다"));

    rt.start(StartOptions {
        playlist_id: playlist,
        reason: StartReason::Manual,
        dry_run: false,
        scheduled_end: None,
        occurrence: None,
        order_seed: Some(1),
    })
    .expect("start failed");

    let d = rt.diagnostics();
    assert!(d.argv_is_stream_copy, "a stream-copy session reported an encoder: {:?}", d.video_encoder_args);
    assert!(d.video_encoder_args.is_empty());
    assert!(d.mismatch.is_none(), "{:?}", d.mismatch);
    assert!(d.verdict.contains("STREAM COPY"));

    // The command is displayable: readable, and carrying no key.
    let cmd = d.masked_command.join(" ");
    assert!(cmd.contains("-c copy"));
    assert!(cmd.contains("-f concat"));
    assert_clean("diagnostics command", &cmd);

    rt.stop(true).ok();
}

/// Compatibility mode is reported honestly too, so a high-CPU session is
/// explained rather than mysterious.
#[test]
fn diagnostics_report_compatibility_mode_as_encoding() {
    let Some(tools) = common::tools() else {
        eprintln!("SKIP: no ffmpeg available");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let b = FfmpegCommandBuilder::new(tools, OutputProfile::P720p30);
    let db = Database::open(&dir.path().join("louver.db")).unwrap();
    let playlist = db.create_playlist("Diag2", PlaybackMode::Sequential, OutputProfile::P720p30).unwrap();
    let f = dir.path().join("v.mp4");
    std::fs::write(&f, vec![0u8; 2048]).unwrap();
    let mid = db
        .upsert_media(&Media {
            id: 0,
            source_path: f.to_string_lossy().into_owned(),
            display_name: "v.mp4".into(),
            status: MediaStatus::Normalized,
            media_hash: "h2".into(),
            normalized_path: Some(f.to_string_lossy().into_owned()),
            normalized_profile: Some("720p30".into()),
            duration_secs: 10.0,
            normalized_duration_secs: Some(10.0),
            width: 1280,
            height: 720,
            fps: 30.0,
            video_codec: "h264".into(),
            audio_codec: Some("aac".into()),
            pixel_format: Some("yuv420p".into()),
            is_hdr: false,
            file_size: 2048,
            added_at: String::new(),
            last_error: None,
        })
        .unwrap();
    db.add_playlist_item(playlist, mid).unwrap();
    db.set_setting(louver_core::settings_keys::RTMPS_URL, "rtmp://127.0.0.1:1/live").unwrap();
    // Ask for the fallback path explicitly.
    db.set_setting(louver_core::settings_keys::STREAM_MODE, "compatibility_encode").unwrap();

    let keys = Arc::new(StreamKeyStore::new(Arc::new(MemorySecretStore::new())));
    keys.set(REAL_SHAPED_KEY).unwrap();
    let launcher: Arc<dyn StreamLauncher> =
        Arc::new(FfmpegLauncher { program: b.tools().ffmpeg.clone(), log: Arc::new(|_| {}) });
    let mut rt = BroadcastRuntime::new(
        db.clone(),
        b,
        launcher,
        Arc::new(SystemClock) as Arc<dyn Clock>,
        keys,
        Arc::new(louver_core::system::NoopSleepPreventer::default()),
        Arc::new(Sink::default()) as Arc<dyn RuntimeEvents>,
        SessionStore::new(dir.path().join("session.json")),
        dir.path().join("manifest.txt"),
        dir.path().join("dry-run"),
    );
    rt.start(StartOptions {
        playlist_id: playlist,
        reason: StartReason::Manual,
        dry_run: false,
        scheduled_end: None,
        occurrence: None,
        order_seed: Some(1),
    })
    .expect("start failed");

    let d = rt.diagnostics();
    assert!(!d.argv_is_stream_copy, "compatibility mode should be encoding");
    assert!(d.video_encoder_args.contains(&"-c:v".to_string()));
    assert!(d.mismatch.is_none(), "configured and actual agree, so no mismatch: {:?}", d.mismatch);
    assert!(d.verdict.contains("COMPATIBILITY"));
    rt.stop(true).ok();
}
