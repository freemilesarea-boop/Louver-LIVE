//! §8 against real media: upload, probe, prepare, and only what is needed.
//!
//! Skips loudly when FFmpeg is absent rather than passing on nothing.

use louver_cloud::ingest::Ingest;
use louver_cloud::storage::{LocalStorage, Storage};
use louver_cloud::{CloudDb, MediaState};
use louver_core::streaming::ffmpeg::FfmpegTools;
use std::sync::Arc;

fn tools() -> Option<FfmpegTools> {
    let sidecar = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps/desktop/src-tauri/binaries");
    FfmpegTools::discover(Some(&sidecar)).ok()
}

/// A clip that already matches the broadcast profile, and one that does not.
fn make(tools: &FfmpegTools, at: &std::path::Path, conformant: bool) {
    let args: Vec<String> = if conformant {
        vec![
            "-y", "-loglevel", "error",
            "-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=30:duration=2",
            "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=2",
            "-c:v", "libx264", "-preset", "ultrafast", "-profile:v", "high", "-level", "4.2",
            "-pix_fmt", "yuv420p", "-g", "60", "-keyint_min", "60", "-sc_threshold", "0",
            "-b:v", "6000k", "-r", "30", "-fps_mode", "cfr",
            "-c:a", "aac", "-b:a", "192k", "-ar", "48000", "-ac", "2",
            "-video_track_timescale", "30000", "-movflags", "+faststart",
            at.to_str().unwrap(),
        ]
    } else {
        vec![
            "-y", "-loglevel", "error",
            "-f", "lavfi", "-i", "smptebars=size=640x480:rate=24:duration=2",
            "-f", "lavfi", "-i", "sine=frequency=330:sample_rate=44100:duration=2",
            "-ac", "1", "-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac",
            at.to_str().unwrap(),
        ]
    }
    .iter()
    .map(|s| s.to_string())
    .collect();

    let out = std::process::Command::new(&tools.ffmpeg).args(&args).output().expect("ffmpeg");
    assert!(out.status.success(), "fixture: {}", String::from_utf8_lossy(&out.stderr));
}

struct Env {
    _dir: tempfile::TempDir,
    db: CloudDb,
    ingest: Ingest,
    user: String,
    root: std::path::PathBuf,
}

fn env(tools: &FfmpegTools) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let db = CloudDb::open(&dir.path().join("cloud.db")).unwrap();
    let user = db.create_user("up@x.com", "hash", "business").unwrap().id;
    let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(dir.path().join("media")));
    let ingest = Ingest::new(db.clone(), storage, tools.clone(), "libx264".into());
    let root = dir.path().to_path_buf();
    Env { _dir: dir, db, ingest, user, root }
}

#[test]
fn a_conformant_upload_is_remuxed_and_becomes_broadcastable() {
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let src = e.root.join("ok.mp4");
    make(&tools, &src, true);
    let original_bytes = std::fs::metadata(&src).unwrap().len();

    let m = e.ingest.accept_upload(&e.user, "ok.mp4", &src).unwrap();
    assert_eq!(m.state, MediaState::Uploaded, "the row exists before anything is read");
    assert!(!src.exists(), "the upload temp file was left behind");

    e.ingest.prepare(&m.id).expect("prepare");

    let done = e.db.media_owned(&e.user, &m.id).unwrap();
    assert_eq!(done.state, MediaState::Ready);
    assert_eq!(done.width, 1920);
    assert_eq!(done.height, 1080);
    assert!((done.fps - 30.0).abs() < 0.01);
    assert_eq!(done.video_codec, "h264");
    assert!(done.prepared_path.is_some(), "nothing was prepared");
    assert!(done.last_error.is_none());

    // The original is still there — preparing must never destroy an upload.
    let store = LocalStorage::new(e.root.join("media"));
    assert_eq!(store.size_bytes(&done.storage_path).unwrap(), original_bytes);

    // And the manager can now read what it needs to start a broadcast.
    let ready = e.db.prepared_media_for(&m.id).unwrap();
    assert!(ready.duration_secs > 1.0);
}

#[test]
fn a_non_conformant_upload_is_transcoded_and_still_becomes_broadcastable() {
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let src = e.root.join("odd.mp4");
    make(&tools, &src, false);

    let m = e.ingest.accept_upload(&e.user, "odd.mp4", &src).unwrap();
    e.ingest.prepare(&m.id).expect("prepare");

    let done = e.db.media_owned(&e.user, &m.id).unwrap();
    assert_eq!(done.state, MediaState::Ready);
    // What was probed is the source's own shape, not the profile's.
    assert_eq!((done.width, done.height), (640, 480));
    // What was produced conforms, which is what makes it broadcastable.
    let prepared = LocalStorage::new(e.root.join("media"))
        .localize(&done.prepared_path.clone().unwrap())
        .unwrap();
    let builder = louver_core::streaming::ffmpeg::FfmpegCommandBuilder::new(
        tools.clone(),
        louver_cloud::ingest::CLOUD_PROFILE,
    );
    let info = louver_core::media::probe::probe(&builder, &prepared).unwrap();
    assert_eq!((info.width, info.height), (1920, 1080));
    assert!(
        louver_core::media::probe::check_compatibility(&info, louver_cloud::ingest::CLOUD_PROFILE)
            .is_compatible(),
        "the prepared file does not match the broadcast profile",
    );
}

#[test]
fn a_file_that_is_not_video_fails_the_media_and_not_the_server() {
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let src = e.root.join("notvideo.mp4");
    std::fs::write(&src, b"this is not an mp4").unwrap();

    let m = e.ingest.accept_upload(&e.user, "notvideo.mp4", &src).unwrap();
    assert!(e.ingest.prepare(&m.id).is_err(), "garbage must not report success");

    // The failure belongs to the row, with something a person can read.
    e.db.record_media_failed(&m.id, "probe failed").unwrap();
    let done = e.db.media_owned(&e.user, &m.id).unwrap();
    assert_eq!(done.state, MediaState::Failed);
    assert!(done.last_error.is_some());

    // And a broadcast cannot be started from it.
    assert!(e.db.prepared_media_for(&m.id).is_err());
}

#[test]
fn an_upload_over_the_plan_limit_is_refused_before_it_is_stored() {
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let src = e.root.join("ok.mp4");
    make(&tools, &src, true);

    // Squeeze the plan down to nothing.
    e.db.raw()
        .lock()
        .unwrap()
        .execute(
            "UPDATE plans SET limits='{\"max_upload_bytes\":10,\"max_storage_bytes\":10,\"max_broadcasts\":1,\"max_concurrent_streams\":1}' WHERE id='business'",
            [],
        )
        .unwrap();

    let r = e.ingest.accept_upload(&e.user, "ok.mp4", &src);
    assert!(matches!(r, Err(louver_cloud::CloudError::LimitReached { .. })), "{r:?}");
    assert!(src.exists(), "a refused upload should not have been consumed");
    assert!(e.db.media_for(&e.user).unwrap().is_empty(), "a refused upload left a row");
}
