//! §8 against real media: upload, probe, prepare, and only what is needed.
//!
//! Skips loudly when FFmpeg is absent rather than passing on nothing.

use louver_cloud::ingest::Ingest;
use louver_cloud::storage::{LocalStorage, Storage};
use louver_cloud::{CloudDb, MediaState};
use louver_core::streaming::ffmpeg::FfmpegTools;
use std::sync::Arc;

fn tools() -> Option<FfmpegTools> {
    let sidecar =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apps/desktop/src-tauri/binaries");
    FfmpegTools::discover(Some(&sidecar)).ok()
}

/// A clip that already matches the broadcast profile, and one that does not.
fn make(tools: &FfmpegTools, at: &std::path::Path, conformant: bool) {
    let args: Vec<String> = if conformant {
        vec![
            "-y",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1920x1080:rate=30:duration=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=2",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-profile:v",
            "high",
            "-level",
            "4.2",
            "-pix_fmt",
            "yuv420p",
            "-g",
            "60",
            "-keyint_min",
            "60",
            "-sc_threshold",
            "0",
            "-b:v",
            "6000k",
            "-r",
            "30",
            "-fps_mode",
            "cfr",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-video_track_timescale",
            "30000",
            "-movflags",
            "+faststart",
            at.to_str().unwrap(),
        ]
    } else {
        vec![
            "-y",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "smptebars=size=640x480:rate=24:duration=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=330:sample_rate=44100:duration=2",
            "-ac",
            "1",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-c:a",
            "aac",
            at.to_str().unwrap(),
        ]
    }
    .iter()
    .map(|s| s.to_string())
    .collect();

    let out = std::process::Command::new(&tools.ffmpeg).args(&args).output().expect("ffmpeg");
    assert!(out.status.success(), "fixture: {}", String::from_utf8_lossy(&out.stderr));
}

/// Wait for the preparation `accept_upload` started on its own thread.
///
/// The test watches the row the way a browser does rather than starting a
/// second preparation of the same upload, which is the production path and also
/// the only one that cannot race itself.
fn settled(e: &Env, id: &str) -> louver_cloud::CloudMedia {
    for _ in 0..600 {
        let m = e.db.media_owned(&e.user, id).unwrap();
        if matches!(m.state, MediaState::Ready | MediaState::Failed) {
            return m;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("preparation never finished");
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

    let done = settled(&e, &m.id);
    assert_eq!(done.state, MediaState::Ready, "{:?}", done.last_error);
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
fn only_the_audio_is_converted_when_only_the_audio_is_wrong() {
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let src = e.root.join("odd.mp4");
    make(&tools, &src, false);

    let m = e.ingest.accept_upload(&e.user, "odd.mp4", &src).unwrap();

    let done = settled(&e, &m.id);
    assert_eq!(done.state, MediaState::Ready, "{:?}", done.last_error);
    // What was probed is the source's own shape.
    assert_eq!((done.width, done.height), (640, 480));

    // Test B. The video is H.264 yuv420p at a size and rate that can be sent as
    // they are; only the audio (mono, 44.1 kHz) has to change. Re-encoding the
    // picture to fix the sound is the waste this release exists to remove, so
    // the geometry is kept and the audio alone is converted.
    let prepared = probe_prepared(&e, &tools, &done);
    assert_eq!((prepared.width, prepared.height), (640, 480), "the picture was re-encoded to fix the audio");
    assert_eq!(prepared.audio_sample_rate, Some(48_000));
    assert_eq!(prepared.audio_channels, Some(2));
    assert_eq!(prepared.video_codec, "h264");
}

/// What the prepared file turned out to be.
fn probe_prepared(
    e: &Env,
    tools: &FfmpegTools,
    m: &louver_cloud::CloudMedia,
) -> louver_core::media::probe::MediaInfo {
    let path = LocalStorage::new(e.root.join("media")).localize(&m.prepared_path.clone().unwrap()).unwrap();
    let builder = louver_core::streaming::ffmpeg::FfmpegCommandBuilder::new(
        tools.clone(),
        louver_cloud::ingest::CLOUD_PROFILE,
    );
    louver_core::media::probe::probe(&builder, &path).unwrap()
}

/// A fixture with exactly the shape a test needs.
///
/// Written out rather than parameterised over `make` because the interesting
/// cases differ in one parameter each, and the point of every test below is
/// *which* parameter.
fn fixture(tools: &FfmpegTools, at: &std::path::Path, args: &[&str]) {
    let mut all: Vec<String> = vec!["-y".into(), "-loglevel".into(), "error".into()];
    all.extend(args.iter().map(|s| s.to_string()));
    all.push(at.to_string_lossy().into_owned());
    let out = std::process::Command::new(&tools.ffmpeg).args(&all).output().expect("ffmpeg");
    assert!(out.status.success(), "fixture: {}", String::from_utf8_lossy(&out.stderr));
}

/// The 4.5-hour production file in miniature: 720p, two frames a second,
/// H.264 and AAC, keyframes every two seconds.
fn low_fps_source(tools: &FfmpegTools, at: &std::path::Path) {
    low_fps_source_at(tools, at, "48000")
}

/// The same, at whichever sample rate a test needs. YouTube's encoder guide
/// lists both 44.1 kHz and 48 kHz for stereo RTMP, so both arrive in practice.
fn low_fps_source_at(tools: &FfmpegTools, at: &std::path::Path, sample_rate: &str) {
    let audio = format!("sine=frequency=440:sample_rate={sample_rate}:duration=6");
    fixture(
        tools,
        at,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1280x720:rate=2:duration=6",
            "-f",
            "lavfi",
            "-i",
            &audio,
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-profile:v",
            "high",
            "-level",
            "3.1",
            // Two seconds of keyframe spacing at 2fps is four frames.
            "-g",
            "4",
            "-keyint_min",
            "4",
            "-sc_threshold",
            "0",
            "-r",
            "2",
            "-fps_mode",
            "cfr",
            "-c:a",
            "aac",
            "-b:a",
            "128k",
            "-ar",
            sample_rate,
            "-ac",
            "2",
            "-shortest",
        ],
    );
}

#[test]
fn a_long_low_frame_rate_source_is_not_re_encoded_to_thirty_frames_a_second() {
    // Test G, and the reason this release exists. The production upload was
    // 4h36m of 720p at two frames a second — a slideshow — and the old rule
    // ("does this already match 1920x1080 at 30fps?") sent it through a full
    // libx264 encode of every one of those hours. Nothing a viewer sees is
    // different for it.
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let src = e.root.join("slideshow.mp4");
    low_fps_source(&tools, &src);

    let m = e.ingest.accept_upload(&e.user, "slideshow.mp4", &src).unwrap();
    let done = settled(&e, &m.id);
    assert_eq!(done.state, MediaState::Ready, "{:?}", done.last_error);

    let prepared = probe_prepared(&e, &tools, &done);
    assert_eq!((prepared.width, prepared.height), (1280, 720), "the picture was scaled up to 1080p");
    assert!(prepared.fps < 5.0, "the frame rate was multiplied up to {}fps", prepared.fps);
    assert_eq!(prepared.video_codec, "h264");
    // 48 kHz stereo AAC already, so nothing at all was encoded.
    assert_eq!(prepared.audio_sample_rate, Some(48_000));
}

#[test]
fn a_file_that_already_matches_the_profile_is_not_encoded_either() {
    // Test A. The conformant fixture: nothing about it needs changing, so
    // neither stream is encoded and the result is a remux.
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let src = e.root.join("ok.mp4");
    make(&tools, &src, true);
    let m = e.ingest.accept_upload(&e.user, "ok.mp4", &src).unwrap();
    let done = settled(&e, &m.id);
    assert_eq!(done.state, MediaState::Ready, "{:?}", done.last_error);
    let prepared = probe_prepared(&e, &tools, &done);
    assert_eq!((prepared.width, prepared.height), (1920, 1080));
    assert!((prepared.fps - 30.0).abs() < 0.1);
}

#[test]
fn an_unsupported_codec_still_goes_through_the_full_encode() {
    // Test C. MPEG-4 part 2 cannot be copied into an H.264 stream at all.
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let src = e.root.join("mpeg4.mp4");
    fixture(
        &tools,
        &src,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x480:rate=25:duration=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=2",
            "-c:v",
            "mpeg4",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-shortest",
        ],
    );

    let m = e.ingest.accept_upload(&e.user, "mpeg4.mp4", &src).unwrap();
    let done = settled(&e, &m.id);
    assert_eq!(done.state, MediaState::Ready, "{:?}", done.last_error);
    let prepared = probe_prepared(&e, &tools, &done);
    assert_eq!(prepared.video_codec, "h264", "an unsupported codec was passed through");
    assert_eq!((prepared.width, prepared.height), (1920, 1080), "a transcode must land on the profile");
}

#[test]
fn an_unsupported_pixel_format_still_goes_through_the_full_encode() {
    // Test D. 4:4:4 is H.264, and no player on an ingest will take it.
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let src = e.root.join("yuv444.mp4");
    fixture(
        &tools,
        &src,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x480:rate=25:duration=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=2",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv444p",
            "-c:a",
            "aac",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-shortest",
        ],
    );

    let m = e.ingest.accept_upload(&e.user, "yuv444.mp4", &src).unwrap();
    let done = settled(&e, &m.id);
    assert_eq!(done.state, MediaState::Ready, "{:?}", done.last_error);
    let prepared = probe_prepared(&e, &tools, &done);
    assert_eq!(prepared.pixel_format, "yuv420p", "4:4:4 was sent to an ingest that cannot take it");
    assert_eq!((prepared.width, prepared.height), (1920, 1080));
}

#[test]
fn a_picture_larger_than_the_plan_sells_is_scaled_down() {
    // A copy cannot resize, and 1440p at the profile's bitrate would be both
    // worse than the plan promises and more than the ingest expects.
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let src = e.root.join("big.mp4");
    fixture(
        &tools,
        &src,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=2560x1440:rate=25:duration=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=2",
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
        ],
    );

    let m = e.ingest.accept_upload(&e.user, "big.mp4", &src).unwrap();
    let done = settled(&e, &m.id);
    assert_eq!(done.state, MediaState::Ready, "{:?}", done.last_error);
    let prepared = probe_prepared(&e, &tools, &done);
    assert_eq!((prepared.width, prepared.height), (1920, 1080));
}

/// The longest gap between keyframes in a prepared file.
fn keyframe_gap(e: &Env, tools: &FfmpegTools, m: &louver_cloud::CloudMedia) -> f64 {
    let path = LocalStorage::new(e.root.join("media")).localize(&m.prepared_path.clone().unwrap()).unwrap();
    let builder = louver_core::streaming::ffmpeg::FfmpegCommandBuilder::new(
        tools.clone(),
        louver_cloud::ingest::CLOUD_PROFILE,
    );
    louver_core::media::probe::probe_max_keyframe_gap(&builder, &path, 60).expect("keyframes")
}

/// The production file in miniature: everything is right except the keyframes.
fn sparse_keyframe_source(tools: &FfmpegTools, at: &std::path::Path, sample_rate: &str) {
    let audio = format!("sine=frequency=440:sample_rate={sample_rate}:duration=30");
    fixture(
        tools,
        at,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1280x720:rate=2:duration=30",
            "-f",
            "lavfi",
            "-i",
            &audio,
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-profile:v",
            "high",
            "-level",
            "3.1",
            // One keyframe every ten seconds — YouTube's limit is four.
            "-g",
            "20",
            "-keyint_min",
            "20",
            "-sc_threshold",
            "0",
            "-r",
            "2",
            "-fps_mode",
            "cfr",
            "-c:a",
            "aac",
            "-b:a",
            "128k",
            "-ar",
            sample_rate,
            "-ac",
            "2",
            "-shortest",
        ],
    );
}

#[test]
fn keyframes_too_far_apart_are_never_copied() {
    // Test D. YouTube's own encoder guide asks for a keyframe every two seconds
    // and forbids more than four. A ten-second gap means a viewer joining the
    // stream waits ten seconds for a picture, and the ingest may buffer. No
    // amount of "everything else is fine" makes that copyable — and what is
    // checked is the *produced* file, not what the encoder was asked for.
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let src = e.root.join("sparse.mp4");
    sparse_keyframe_source(&tools, &src, "48000");

    let m = e.ingest.accept_upload(&e.user, "sparse.mp4", &src).unwrap();
    let done = settled(&e, &m.id);
    assert_eq!(done.state, MediaState::Ready, "{:?}", done.last_error);

    // Test F: the produced file is inside the limit.
    let gap = keyframe_gap(&e, &tools, &done);
    assert!(gap <= 4.0, "a prepared file went out with {gap:.1}s between keyframes");
}

#[test]
fn fixing_the_keyframes_does_not_also_change_the_size_or_the_frame_rate() {
    // Test E, and the whole point of this release. The production upload is
    // 720p at two frames a second with ten-second keyframes: everything about
    // it is broadcastable except the keyframes. Adding an IDR needs an encode —
    // there is no way to copy one in — but nothing about that encode requires
    // scaling to 1080p or inventing twenty-eight frames a second.
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let src = e.root.join("prod-like.mp4");
    sparse_keyframe_source(&tools, &src, "44100");

    let m = e.ingest.accept_upload(&e.user, "prod-like.mp4", &src).unwrap();
    let done = settled(&e, &m.id);
    assert_eq!(done.state, MediaState::Ready, "{:?}", done.last_error);

    let prepared = probe_prepared(&e, &tools, &done);
    assert_eq!((prepared.width, prepared.height), (1280, 720), "the picture was scaled for no reason");
    assert!(prepared.fps < 5.0, "the frame rate was raised to {:.2}fps for no reason", prepared.fps);
    assert_eq!(prepared.pixel_format, "yuv420p");
    // Test A: 44.1 kHz stereo AAC is what YouTube's guide lists for RTMP, so
    // the sound is not touched either.
    assert_eq!(prepared.audio_sample_rate, Some(44_100), "44.1kHz audio was re-encoded for nothing");
    assert_eq!(prepared.audio_channels, Some(2));
    // And the one thing that did have to change, changed.
    let gap = keyframe_gap(&e, &tools, &done);
    assert!(gap <= 4.0, "{gap:.1}s between keyframes");

    // Test G: the result is measured and recorded like any other preparation.
    let shapes = e.db.playlist_shapes(&broadcast_over(&e, &[&m.id])).unwrap();
    let (_, mode, signature) = shapes[0].clone();
    assert_eq!(mode, "live_normalize");
    let sig = signature.expect("no signature was recorded");
    assert!(sig.contains("1280x720"), "{sig}");
    assert!(sig.contains("44100"), "{sig}");
}

#[test]
fn keyframes_already_close_enough_are_left_alone() {
    // Test C. The same file with two-second keyframes is copied outright.
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let src = e.root.join("dense.mp4");
    low_fps_source(&tools, &src);

    let m = e.ingest.accept_upload(&e.user, "dense.mp4", &src).unwrap();
    let done = settled(&e, &m.id);
    assert_eq!(done.state, MediaState::Ready, "{:?}", done.last_error);
    let prepared = probe_prepared(&e, &tools, &done);
    assert_eq!((prepared.width, prepared.height), (1280, 720));
    assert!(keyframe_gap(&e, &tools, &done) <= 4.0);
}

#[test]
fn two_sample_rates_in_one_playlist_are_converged_rather_than_joined() {
    // Test B. 44.1 kHz and 48 kHz are both fine to send; they are not fine to
    // concatenate. The FLV muxer writes one audio configuration at the head of
    // the stream, so the second file's sound would be decoded at the first
    // file's rate.
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);

    let a = e.root.join("at-44.mp4");
    low_fps_source_at(&tools, &a, "44100");
    let first = e.ingest.accept_upload(&e.user, "at-44.mp4", &a).unwrap().id;
    assert_eq!(settled(&e, &first).state, MediaState::Ready);

    let b_src = e.root.join("at-48.mp4");
    low_fps_source_at(&tools, &b_src, "48000");
    let second = e.ingest.accept_upload(&e.user, "at-48.mp4", &b_src).unwrap().id;
    assert_eq!(settled(&e, &second).state, MediaState::Ready);

    // Each on its own kept its own rate.
    assert_eq!(
        probe_prepared(&e, &tools, &e.db.media_owned(&e.user, &first).unwrap()).audio_sample_rate,
        Some(44_100),
    );

    let b = broadcast_over(&e, &[&first, &second]);
    assert!(e.db.check_playlist_joinable(&b).is_err(), "two sample rates were judged joinable");
    assert_eq!(e.ingest.ensure_playlist_compatible(&b).unwrap(), 2);
    for id in [&first, &second] {
        assert_eq!(settled(&e, id).state, MediaState::Ready);
        let prepared = probe_prepared(&e, &tools, &e.db.media_owned(&e.user, id).unwrap());
        assert_eq!(prepared.audio_sample_rate, Some(48_000), "convergence must land on one rate");
    }
    e.db.check_playlist_joinable(&b).expect("the playlist was not made joinable");
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

    // The failure belongs to the row, with something a person can read, and the
    // server carries on.
    let done = settled(&e, &m.id);
    assert_eq!(done.state, MediaState::Failed, "garbage must not report success");
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

#[test]
fn preparing_the_same_upload_twice_at_once_does_not_destroy_it() {
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let src = e.root.join("twice.mp4");
    make(&tools, &src, true);
    let m = e.ingest.accept_upload(&e.user, "twice.mp4", &src).unwrap();

    // Four callers at once, on top of the thread the upload already started.
    // Exactly one preparation must happen: the others found it in flight and
    // left it alone rather than filing its output away underneath it.
    let hands: Vec<_> = (0..4)
        .map(|_| {
            let ingest = e.ingest.clone();
            let id = m.id.clone();
            std::thread::spawn(move || ingest.prepare(&id))
        })
        .collect();
    for h in hands {
        assert!(h.join().unwrap().is_ok(), "a concurrent prepare reported a failure");
    }

    let done = settled(&e, &m.id);
    assert_eq!(done.state, MediaState::Ready, "{:?}", done.last_error);
    assert!(done.prepared_path.is_some());

    // And a retry afterwards is still allowed — the claim was released.
    e.ingest.prepare(&m.id).expect("a retry after the work finished");
}

#[test]
fn an_upload_that_would_fill_the_server_is_refused_before_it_is_stored() {
    // Not the plan's ceiling — the machine's. A disk with no room left cannot
    // write the database, the prepared file or the container's log, so every
    // broadcast on the server dies with it. This is the one lever a user has on
    // that, so it is the one place the floor is enforced.
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);

    // A byte is always fine, whatever disk this test runs on.
    e.ingest.check_disk_has_room(1).unwrap();

    // Eight exabytes is not, on any disk, and the arithmetic must saturate
    // rather than wrap into "plenty of room".
    match e.ingest.check_disk_has_room(i64::MAX) {
        Err(louver_cloud::CloudError::OutOfSpace) => {}
        other => panic!("an impossible upload was accepted: {other:?}"),
    }
    // And the message a user sees says what to do, with no path in it.
    let said = louver_cloud::CloudError::OutOfSpace.to_string();
    assert!(said.contains("저장 공간"), "{said}");
    assert!(!said.contains('/'), "a user-facing message must not carry a path: {said}");
}

// --- playlists are concatenated, so their items have to agree -------------

/// A broadcast over the given media, in order.
fn broadcast_over(e: &Env, media: &[&str]) -> String {
    let dest = e.db.create_destination(&e.user, "채널", "rtmps://a/live2", "••••").unwrap();
    let b = e.db.create_broadcast(&e.user, "플레이리스트", media[0], &dest.id, true).unwrap();
    let items: Vec<louver_cloud::db::NewItem> = media
        .iter()
        .map(|m| louver_cloud::db::NewItem { media_id: (*m).to_string(), enabled: true, repeat_count: 1 })
        .collect();
    e.db.replace_items(&e.user, &b.id, &items).unwrap();
    b.id
}

#[test]
fn a_playlist_of_identical_uploads_needs_nothing_doing_to_it() {
    // Test E. Two exports of the same shape — the ordinary case, and the one
    // that must not cost anything. Both take the fast path, both come out with
    // the same signature, and the playlist is joinable as it stands.
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let mut ids = Vec::new();
    for name in ["one.mp4", "two.mp4"] {
        let src = e.root.join(name);
        low_fps_source(&tools, &src);
        let m = e.ingest.accept_upload(&e.user, name, &src).unwrap();
        assert_eq!(settled(&e, &m.id).state, MediaState::Ready);
        ids.push(m.id);
    }
    let b = broadcast_over(&e, &[&ids[0], &ids[1]]);

    e.db.check_playlist_joinable(&b).expect("two identical uploads were judged unjoinable");
    assert_eq!(e.ingest.ensure_playlist_compatible(&b).unwrap(), 0, "something was re-prepared for nothing");
    // Both kept their own shape rather than being pushed to 1080p30.
    for id in &ids {
        let m = e.db.media_owned(&e.user, id).unwrap();
        assert_eq!(probe_prepared(&e, &tools, &m).width, 1280);
    }
    assert_eq!(e.db.prepared_items_for(&b).unwrap().len(), 2);
}

#[test]
fn a_playlist_that_mixes_formats_is_made_to_agree_before_it_can_start() {
    // Test F. Two files that are each individually fine to send, and cannot be
    // joined to each other: different geometry, different frame rate, different
    // SPS. Concatenating them and copying packets would produce a stream that
    // decodes as garbage from the seam onwards — live, with nobody watching.
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);

    let a = e.root.join("slideshow.mp4");
    low_fps_source(&tools, &a);
    let first = e.ingest.accept_upload(&e.user, "slideshow.mp4", &a).unwrap().id;
    assert_eq!(settled(&e, &first).state, MediaState::Ready);

    let b_src = e.root.join("normal.mp4");
    fixture(
        &tools,
        &b_src,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=854x480:rate=25:duration=3",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=330:sample_rate=48000:duration=3",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-g",
            "50",
            "-keyint_min",
            "50",
            "-sc_threshold",
            "0",
            "-c:a",
            "aac",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-shortest",
        ],
    );
    let second = e.ingest.accept_upload(&e.user, "normal.mp4", &b_src).unwrap().id;
    assert_eq!(settled(&e, &second).state, MediaState::Ready);

    let b = broadcast_over(&e, &[&first, &second]);
    // As they stand, this playlist may not go on air.
    assert!(e.db.check_playlist_joinable(&b).is_err(), "a mixed playlist was judged joinable");

    // Putting them in one playlist is what triggers the expensive path — and it
    // is the only thing that does.
    assert_eq!(e.ingest.ensure_playlist_compatible(&b).unwrap(), 2);
    for id in [&first, &second] {
        assert_eq!(settled(&e, id).state, MediaState::Ready, "re-preparation failed");
    }

    e.db.check_playlist_joinable(&b).expect("the playlist was not made joinable");
    for id in [&first, &second] {
        let m = e.db.media_owned(&e.user, id).unwrap();
        let prepared = probe_prepared(&e, &tools, &m);
        assert_eq!((prepared.width, prepared.height), (1920, 1080));
        assert!((prepared.fps - 30.0).abs() < 0.1);
    }
    // And it stays that way: a second pass has nothing left to do.
    assert_eq!(e.ingest.ensure_playlist_compatible(&b).unwrap(), 0);
}

#[test]
fn media_prepared_before_this_release_is_still_broadcastable() {
    // Test J. Every file prepared before signatures existed came out of the one
    // canonical encode, so a row with no signature reads as canonical — and two
    // of them are joinable, which is exactly what production has today.
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let mut ids = Vec::new();
    for name in ["legacy-a.mp4", "legacy-b.mp4"] {
        let src = e.root.join(name);
        make(&tools, &src, true);
        let m = e.ingest.accept_upload(&e.user, name, &src).unwrap();
        assert_eq!(settled(&e, &m.id).state, MediaState::Ready);
        ids.push(m.id);
    }
    // Exactly what a production row looks like: prepared, with nothing recorded
    // about its shape.
    e.db.raw()
        .lock()
        .unwrap()
        .execute("UPDATE media SET prepared_mode=NULL, prepared_signature=NULL", [])
        .unwrap();

    let b = broadcast_over(&e, &[&ids[0], &ids[1]]);
    e.db.check_playlist_joinable(&b).expect("legacy media was judged unjoinable");
    assert_eq!(e.ingest.ensure_playlist_compatible(&b).unwrap(), 0, "legacy media was re-prepared");
    assert_eq!(e.db.prepared_items_for(&b).unwrap().len(), 2);
}
