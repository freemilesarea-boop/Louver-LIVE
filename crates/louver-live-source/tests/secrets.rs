//! That nothing secret leaves this worker.
//!
//! Three things are treated as secret:
//!
//!  * the RTMP(S) **destination**, because it carries the stream key;
//!  * the **resolved manifest URL**, because YouTube signs it — it is closer to
//!    a credential than to an address, and anyone holding it can read the
//!    stream;
//!  * anything to do with OAuth, which this crate does not have.
//!
//! The first two are checked here against the real binary's output and against
//! the state file it writes, not against a mock, because a leak would happen in
//! a log line and a log line is the thing a mock does not produce.

mod common;

use common::*;
use louver_core::streaming::ffmpeg::FfmpegTools;
use louver_live_source::{
    resolver::{self, LiveSourceResolver, ResolvedSource},
    state::StateStore,
    worker::{LiveWorker, WorkerConfig},
    Result,
};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

/// Shaped like a real YouTube stream key, and unmistakable in a haystack.
const STREAM_KEY: &str = "abcd-efgh-ijkl-mnop-qrst";
/// Shaped like a real signed manifest URL: the signature is the secret part.
const SIGNED_MANIFEST: &str =
    "https://93.184.216.34/videoplayback/index.m3u8?sig=SECRETSIGNATUREVALUE&expire=1900000000";

fn dest() -> String {
    format!("rtmps://a.rtmps.youtube.com/live2/{STREAM_KEY}")
}

fn assert_clean(haystack: &str, what: &str) {
    for needle in [STREAM_KEY, "SECRETSIGNATUREVALUE", "rtmps://a.rtmps.youtube.com"] {
        assert!(!haystack.contains(needle), "{what} leaked {needle}:\n{haystack}");
    }
    // A key can also leak in pieces.
    assert!(!haystack.contains("live2/abcd"), "{what} leaked a partial key:\n{haystack}");
}

#[test]
fn the_binary_never_prints_the_destination_even_while_failing() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let manifest = make_playlist(dir.path());
    // A public address that will not answer: the worker gets as far as starting
    // FFmpeg and logging the send, then fails and gives up. That exercises the
    // startup, sending and failure log paths with a real destination in config.
    let out = Command::new(env!("CARGO_BIN_EXE_live-source-worker"))
        .args([
            "--source",
            "https://93.184.216.34/live/stream.m3u8",
            "--manifest",
            manifest.to_str().unwrap(),
            "--state-dir",
            dir.path().join("state").to_str().unwrap(),
            "--dest",
            &dest(),
            "--worker-id",
            "sec1",
            "--ffmpeg",
            &ffmpeg(),
            "--ffprobe",
            &ffprobe(),
            "--max-restarts",
            "0",
            "--stall-after",
            "3",
            "--grace",
            "4",
            "--run-for",
            "40",
        ])
        .output()
        .expect("worker binary");

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(stdout.contains("[louver][live-source]"), "expected the worker's own log:\n{stdout}");
    assert_clean(&stdout, "stdout");
    assert_clean(&stderr, "stderr");
    // And the state file it left behind.
    let state = std::fs::read_to_string(dir.path().join("state").join("worker-sec1.json")).unwrap();
    assert_clean(&state, "the state file");
}

#[test]
fn the_binary_never_prints_the_destination_while_resolving_youtube() {
    // Needs FFmpeg for the playlist fixture, like the rest. The missing guard
    // here is what failed the Linux gate.
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    // The resolve path, which runs yt-dlp. Whether yt-dlp can reach YouTube
    // from this machine is not the point: either way the destination must not
    // appear in the output, and the failure must be classified rather than
    // dumped raw.
    let dir = tempfile::tempdir().unwrap();
    let manifest = make_playlist(dir.path());
    let out = Command::new(env!("CARGO_BIN_EXE_live-source-worker"))
        .args([
            "--source",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "--manifest",
            manifest.to_str().unwrap(),
            "--state-dir",
            dir.path().join("state").to_str().unwrap(),
            "--dest",
            &dest(),
            "--worker-id",
            "sec2",
            "--yt-dlp",
            "yt-dlp",
            "--run-for",
            "40",
        ])
        .output()
        .expect("worker binary");
    let both = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_clean(&both, "the resolve path");
}

#[test]
fn a_signed_manifest_url_is_never_written_down() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    struct Signed;
    impl LiveSourceResolver for Signed {
        fn resolve(&self, _: &str) -> Result<ResolvedSource> {
            Ok(ResolvedSource {
                manifest_url: SIGNED_MANIFEST.into(),
                is_live: true,
                width: Some(1280),
                height: Some(720),
                title: Some("테스트".into()),
            })
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let manifest = make_playlist(dir.path());
    let state_dir = dir.path().join("state");
    let mut cfg = WorkerConfig::new(
        "sec3",
        "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        &manifest,
        dest(),
        &state_dir,
    );
    cfg.tools = FfmpegTools::new(ffmpeg(), ffprobe());
    cfg.max_restarts = 0;
    cfg.stall_after = Duration::from_secs(3);
    cfg.grace = Duration::from_secs(4);
    cfg.run_for = Some(Duration::from_secs(25));
    // The address does not answer, so this gives up quickly; what matters is
    // what it wrote down on the way.
    let mut w = LiveWorker::new(cfg, Arc::new(Signed));
    let _ = w.run();
    let state = std::fs::read_to_string(StateStore::new(&state_dir, "sec3").path()).unwrap();
    assert_clean(&state, "the state file");
    assert!(!state.contains("videoplayback"), "the manifest path must not be stored:\n{state}");
    // The video id is fine — it is in every share link, and without it an
    // operator cannot tell two workers apart.
    assert!(state.contains("dQw4w9WgXcQ"), "the video id is not a secret and is useful:\n{state}");
}

#[test]
fn a_resolver_failure_message_carries_no_credential() {
    // yt-dlp is run with no cookies and no credentials, so its stderr has none
    // to leak; this pins that the classifier does not pass a whole body through
    // either.
    for stderr in [
        "ERROR: [youtube] abc: Private video. Sign in if you've been granted access",
        &format!("ERROR: unable to download: {SIGNED_MANIFEST}"),
        &format!("ERROR: something with a key {STREAM_KEY} in it"),
    ] {
        let (_, message) = resolver::classify_resolver_stderr(stderr);
        // The two classified cases must not quote the input at all.
        if !message.starts_with("영상 정보를 가져오지 못했습니다") {
            assert_clean(&message, "a classified resolver message");
        }
    }
}

#[test]
fn the_only_logging_function_cannot_be_given_the_destination() {
    // `say` takes a `&str` and the worker builds every line from fields that
    // are not the destination. Checked as a property of the source because a
    // future edit could pass `self.cfg.destination` into a format string and
    // no happy-path test would notice.
    let worker =
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/worker.rs"))
            .unwrap();
    for line in worker.lines() {
        let t = line.trim_start();
        if t.starts_with("//") || t.starts_with("//!") {
            continue;
        }
        if t.contains("self.say(") || t.contains("println!") {
            assert!(!line.contains("destination"), "a log line is being given the destination: {line}");
            assert!(!line.contains("manifest_url"), "a log line is being given the manifest URL: {line}");
        }
    }
}

/* ----------------------------------------- the API binary's own output */

#[test]
fn the_api_binary_never_prints_its_secret_or_its_destination_map() {
    // Both are given to it through the environment, and both are the kind of
    // thing a startup banner cheerfully echoes. The destination map holds
    // stream keys; the signing secret mints tokens for every user.
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("media");
    std::fs::create_dir_all(&media).unwrap();
    std::fs::write(media.join("song-a.mp4"), b"x").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_live-source-api"))
        .args([
            // An address that is not on this machine, so the bind fails and it
            // starts, logs, and exits — exactly the window a banner would leak
            // in. Not a privileged port: as root `127.0.0.1:1` binds, and the
            // process then serves forever instead of exiting, so this test
            // would hang rather than fail. It passed before only because a
            // previous run had left something holding port 1.
            "--listen",
            "203.0.113.1:9080",
            "--media-dir",
            media.to_str().unwrap(),
            "--state-dir",
            dir.path().join("state").to_str().unwrap(),
            "--origin",
            "https://247streams.kr",
        ])
        .env("LOUVER_LIVE_SOURCE_SECRET", "a-signing-secret-that-must-not-leak-32")
        .env("LOUVER_LIVE_SOURCE_GATE_SECRET", "a-gate-secret-that-must-not-leak-32!")
        .env("LOUVER_LIVE_SOURCE_ADMIN_SECRET", "an-admin-secret-that-must-not-leak32")
        .env(
            "LOUVER_LIVE_SOURCE_DESTINATIONS",
            format!(r#"{{"user-alice":{{"test-sink":"rtmps://a.rtmps.youtube.com/live2/{STREAM_KEY}"}}}}"#),
        )
        .output()
        .expect("api binary");

    let both = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(both.contains("[louver][live-source-api]"), "expected its own log:\n{both}");
    assert_clean(&both, "the api binary");
    assert!(!both.contains("a-signing-secret-that-must-not-leak-32"), "the signing secret leaked:\n{both}");
    assert!(!both.contains("a-gate-secret-that-must-not-leak-32!"), "the gate secret leaked:\n{both}");
    assert!(!both.contains("an-admin-secret-that-must-not-leak32"), "the admin secret leaked:\n{both}");
    assert!(both.contains("admin=on"), "the banner should say the admin API is on:\n{both}");
    // The destination *URL* is not printed, and neither is a name: the banner
    // says how many users have one and nothing else.
    assert!(!both.contains("live2/"), "{both}");
    assert!(!both.contains("test-sink"), "a destination name is itself a leak:\n{both}");
    assert!(both.contains("사용자 1명"), "the banner should count them:\n{both}");
}

#[test]
fn a_malformed_destination_map_is_not_echoed_back() {
    // A JSON error message quotes the input, and the input is a map of stream
    // keys. The binary must refuse without printing what it read.
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("media");
    std::fs::create_dir_all(&media).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_live-source-api"))
        .args([
            "--media-dir",
            media.to_str().unwrap(),
            "--state-dir",
            dir.path().join("state").to_str().unwrap(),
        ])
        .env("LOUVER_LIVE_SOURCE_SECRET", "a-signing-secret-that-must-not-leak-32")
        .env("LOUVER_LIVE_SOURCE_GATE_SECRET", "a-gate-secret-that-must-not-leak-32!")
        .env("LOUVER_LIVE_SOURCE_ADMIN_SECRET", "an-admin-secret-that-must-not-leak32")
        .env(
            "LOUVER_LIVE_SOURCE_DESTINATIONS",
            format!(r#"{{"user-alice":{{"broken": "rtmps://x/live2/{STREAM_KEY}"#),
        )
        .output()
        .expect("api binary");
    let both = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_clean(&both, "the malformed-map path");
    assert!(both.contains("JSON"), "it should still say what is wrong:\n{both}");
}

#[test]
fn a_short_signing_secret_is_refused_at_startup() {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("media");
    std::fs::create_dir_all(&media).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_live-source-api"))
        .args([
            "--media-dir",
            media.to_str().unwrap(),
            "--state-dir",
            dir.path().join("state").to_str().unwrap(),
        ])
        .env("LOUVER_LIVE_SOURCE_SECRET", "short")
        .env("LOUVER_LIVE_SOURCE_GATE_SECRET", "a-gate-secret-that-must-not-leak-32!")
        .env("LOUVER_LIVE_SOURCE_ADMIN_SECRET", "an-admin-secret-that-must-not-leak32")
        .env("LOUVER_LIVE_SOURCE_DESTINATIONS", r#"{"user-alice":{"a":"rtmp://127.0.0.1/live/x"}}"#)
        .output()
        .expect("api binary");
    assert_ne!(out.status.code(), Some(0), "it must not start with a decorative signature");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("32"), "{err}");
    assert!(!err.contains("short"), "the secret itself must not be echoed: {err}");
}

#[test]
fn the_api_will_not_start_without_a_usable_gate_secret() {
    // Fail-closed, and the only way to make it so: if a missing gate secret
    // merely disabled the gate, a deployment that forgot the variable would
    // expose the port to anything that can route to the machine and nothing
    // would say so. There is deliberately no flag that turns it off.
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("media");
    std::fs::create_dir_all(&media).unwrap();

    for (what, gate) in [("missing", None), ("too short", Some("short")), ("empty", Some(""))] {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_live-source-api"));
        cmd.args([
            "--media-dir",
            media.to_str().unwrap(),
            "--state-dir",
            dir.path().join("state").to_str().unwrap(),
        ])
        .env_remove("LOUVER_LIVE_SOURCE_GATE_SECRET")
        .env("LOUVER_LIVE_SOURCE_SECRET", "a-signing-secret-that-must-not-leak-32")
        .env("LOUVER_LIVE_SOURCE_ADMIN_SECRET", "an-admin-secret-that-must-not-leak32")
        .env("LOUVER_LIVE_SOURCE_DESTINATIONS", r#"{"user-alice":{"a":"rtmp://127.0.0.1/live/x"}}"#);
        if let Some(g) = gate {
            cmd.env("LOUVER_LIVE_SOURCE_GATE_SECRET", g);
        }
        let out = cmd.output().expect("api binary");
        assert_ne!(out.status.code(), Some(0), "{what}: it must not start");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("GATE_SECRET"), "{what}: {err}");
        if let Some(g) = gate {
            if !g.is_empty() {
                assert!(!err.contains(g), "{what}: the secret itself was echoed: {err}");
            }
        }
    }
}

#[test]
fn a_flat_destination_map_is_refused_rather_than_shared_between_users() {
    // The isolation defect, as a configuration mistake: the old shape was one
    // `{name: url}` map for everybody. If the binary accepted it now, every
    // beta account would share one set of destinations again — so it has to be
    // a refusal to start rather than something reinterpreted.
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("media");
    std::fs::create_dir_all(&media).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_live-source-api"))
        .args([
            "--media-dir",
            media.to_str().unwrap(),
            "--state-dir",
            dir.path().join("state").to_str().unwrap(),
        ])
        .env("LOUVER_LIVE_SOURCE_SECRET", "a-signing-secret-that-must-not-leak-32")
        .env("LOUVER_LIVE_SOURCE_GATE_SECRET", "a-gate-secret-that-must-not-leak-32!")
        .env("LOUVER_LIVE_SOURCE_ADMIN_SECRET", "an-admin-secret-that-must-not-leak32")
        // The old flat shape, with a key that looks exactly like a real one.
        .env(
            "LOUVER_LIVE_SOURCE_DESTINATIONS",
            format!(r#"{{"test-sink":"rtmps://a.rtmps.youtube.com/live2/{STREAM_KEY}"}}"#),
        )
        .output()
        .expect("api binary");
    assert_ne!(out.status.code(), Some(0), "the flat shape must not start");
    let both = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_clean(&both, "the flat-map path");
    assert!(!both.contains("live2/"), "{both}");
}

#[test]
fn the_api_will_not_start_without_a_usable_admin_secret() {
    // Fail-closed, for the same reason as the gate: an operator endpoint that
    // can be run without a secret is one that eventually is. There is
    // deliberately no flag that disables the admin API.
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("media");
    std::fs::create_dir_all(&media).unwrap();

    for (what, admin) in [("missing", None), ("too short", Some("short")), ("empty", Some(""))] {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_live-source-api"));
        cmd.args([
            "--media-dir",
            media.to_str().unwrap(),
            "--state-dir",
            dir.path().join("state").to_str().unwrap(),
        ])
        .env_remove("LOUVER_LIVE_SOURCE_ADMIN_SECRET")
        .env("LOUVER_LIVE_SOURCE_SECRET", "a-signing-secret-that-must-not-leak-32")
        .env("LOUVER_LIVE_SOURCE_GATE_SECRET", "a-gate-secret-that-must-not-leak-32!")
        .env("LOUVER_LIVE_SOURCE_DESTINATIONS", r#"{"user-alice":{"a":"rtmp://127.0.0.1/live/x"}}"#);
        if let Some(a) = admin {
            cmd.env("LOUVER_LIVE_SOURCE_ADMIN_SECRET", a);
        }
        let out = cmd.output().expect("api binary");
        assert_ne!(out.status.code(), Some(0), "{what}: it must not start");
        let err = String::from_utf8_lossy(&out.stderr);
        // Names its own variable, so an operator is not left guessing which of
        // the three secrets is being complained about.
        assert!(err.contains("ADMIN_SECRET"), "{what}: {err}");
        if let Some(a) = admin {
            if !a.is_empty() {
                assert!(!err.contains(a), "{what}: the secret itself was echoed: {err}");
            }
        }
    }
}
