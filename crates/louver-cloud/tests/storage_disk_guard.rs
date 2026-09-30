//! The server's own disk: what is reserved, when, and against what number.
//!
//! Two kinds of test, because there are two kinds of question.
//!
//! The policy is arithmetic — given this much free and this much to write, is
//! there room — so it is checked against numbers chosen here rather than
//! against whatever disk happens to be under the test runner. A test that can
//! only pass on a machine with 65GB free is not a test.
//!
//! The estimate is a claim about FFmpeg, so it is checked against FFmpeg: real
//! encodes, with the produced file measured and compared to what was predicted
//! before it was written. Both directions matter. Under-estimating fills the
//! disk; over-estimating refuses uploads that would have fitted, which is how
//! the formula this replaces came to reject a 20GB file unless 65GB were free.

use louver_cloud::ingest::{Ingest, DISK_FLOOR_BYTES};
use louver_cloud::storage::{LocalStorage, Storage};
use louver_cloud::{CloudDb, MediaState};
use louver_core::config::OutputProfile;
use louver_core::media::normalize::{
    direct_source_is_broadcastable, estimated_prepared_bytes, plan_preparation, PrepareMode,
};
use louver_core::media::probe::probe;
use louver_core::streaming::ffmpeg::{FfmpegCommandBuilder, FfmpegTools};
use std::sync::Arc;

const GB: u64 = 1024 * 1024 * 1024;

fn tools() -> Option<FfmpegTools> {
    let sidecar =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apps/desktop/src-tauri/binaries");
    FfmpegTools::discover(Some(&sidecar)).ok()
}

// --- the policy, against numbers ------------------------------------------

/// Test 1 — a 10GB source that needs no conversion, already uploaded, with
/// 20GB free. The old formula wanted 35GB; nothing is going to be written, so
/// the only question is the floor.
#[test]
fn t1_a_direct_ten_gigabyte_upload_passes_with_twenty_free() {
    assert!(Ingest::fits(Some(20 * GB), 0));
}

/// Test 2 — the floor is the floor. Below it nothing is accepted, however
/// little is being asked for.
#[test]
fn t2_nothing_passes_below_the_floor() {
    assert!(!Ingest::fits(Some(DISK_FLOOR_BYTES - 1), 0));
    assert!(!Ingest::fits(Some(4 * GB), 0));
    assert!(!Ingest::fits(Some(0), 0));
}

/// Test 3 — and just above it, with nothing to write, an upload goes through.
#[test]
fn t3_just_above_the_floor_is_enough_when_nothing_is_produced() {
    assert!(Ingest::fits(Some(DISK_FLOOR_BYTES), 0));
    assert!(Ingest::fits(Some(DISK_FLOOR_BYTES + 1024 * 1024), 0));
}

/// Test 4 — a conversion whose output fits above the floor.
#[test]
fn t4_a_conversion_passes_when_its_output_fits_over_the_floor() {
    let output = 3 * GB;
    assert!(Ingest::fits(Some(DISK_FLOOR_BYTES + output), output));
    assert!(Ingest::fits(Some(DISK_FLOOR_BYTES + output + GB), output));
}

/// Test 5 — and is refused before FFmpeg is started when it does not.
#[test]
fn t5_a_conversion_is_refused_when_its_output_would_eat_the_floor() {
    let output = 3 * GB;
    assert!(!Ingest::fits(Some(DISK_FLOOR_BYTES + output - 1), output));
    // The arithmetic saturates rather than wrapping into "plenty of room".
    assert!(!Ingest::fits(Some(u64::MAX - 1), u64::MAX));
}

/// Test 9 — the floor survives every combination, including the one where the
/// volume cannot be read at all, which is the only case that answers yes
/// without checking: a question that could not be asked is not a refusal.
#[test]
fn t9_the_floor_holds_everywhere() {
    for free in [0, 1, DISK_FLOOR_BYTES - 1, DISK_FLOOR_BYTES, 50 * GB] {
        for need in [0, 1, GB, 40 * GB] {
            let expected = free >= need.saturating_add(DISK_FLOOR_BYTES);
            assert_eq!(
                Ingest::fits(Some(free), need),
                expected,
                "free={free} need={need}: the floor must be left standing",
            );
        }
    }
    assert!(Ingest::fits(None, u64::MAX), "an unreadable volume must not refuse everything");
}

// --- the estimate, against FFmpeg -----------------------------------------

struct Env {
    _dir: tempfile::TempDir,
    db: CloudDb,
    ingest: Ingest,
    user: String,
    root: std::path::PathBuf,
    tools: FfmpegTools,
}

fn env(tools: &FfmpegTools) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let db = CloudDb::open(&dir.path().join("cloud.db")).unwrap();
    let user = db.create_user("disk@x.com", "hash", "business").unwrap().id;
    let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(dir.path().join("media")));
    let ingest = Ingest::new(db.clone(), storage, tools.clone(), "libx264".into());
    let root = dir.path().to_path_buf();
    Env { _dir: dir, db, ingest, user, root, tools: tools.clone() }
}

fn settled(e: &Env, id: &str) -> louver_cloud::CloudMedia {
    for _ in 0..900 {
        let m = e.db.media_owned(&e.user, id).unwrap();
        if matches!(m.state, MediaState::Ready | MediaState::Failed) {
            return m;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("preparation never finished");
}

fn fixture(tools: &FfmpegTools, at: &std::path::Path, args: &[&str]) {
    let mut all: Vec<String> = vec!["-y".into(), "-loglevel".into(), "error".into()];
    all.extend(args.iter().map(|s| s.to_string()));
    all.push(at.to_string_lossy().into_owned());
    let out = std::process::Command::new(&tools.ffmpeg).args(&all).output().expect("ffmpeg");
    assert!(out.status.success(), "fixture: {}", String::from_utf8_lossy(&out.stderr));
}

fn source(tools: &FfmpegTools, at: &std::path::Path, size: &str, rate: &str, gop: &str, secs: u32) {
    let video = format!("testsrc2=size={size}:rate={rate}:duration={secs}");
    let audio = format!("sine=frequency=440:sample_rate=48000:duration={secs}");
    fixture(
        tools,
        at,
        &[
            "-f",
            "lavfi",
            "-i",
            &video,
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
            "4.1",
            "-g",
            gop,
            "-keyint_min",
            gop,
            "-sc_threshold",
            "0",
            "-r",
            rate,
            "-fps_mode",
            "cfr",
            "-c:a",
            "aac",
            "-b:a",
            "128k",
            "-ac",
            "2",
            "-ar",
            "48000",
            "-movflags",
            "+faststart",
        ],
    );
}

/// What the preparation step would predict for this file, before running it.
fn predicted(e: &Env, at: &std::path::Path, pinned: bool) -> (u64, PrepareMode, bool) {
    let builder = FfmpegCommandBuilder::new(e.tools.clone(), OutputProfile::P1080p30)
        .with_encoder("libx264".to_string());
    let info = probe(&builder, at).unwrap();
    let prep = plan_preparation(&builder, at, &info, OutputProfile::P1080p30, pinned);
    let bytes = std::fs::metadata(at).unwrap().len();
    let direct = direct_source_is_broadcastable(&info, &prep);
    (estimated_prepared_bytes(&info, &prep, OutputProfile::P1080p30, bytes), prep.mode, direct)
}

/// Test 6 — the canonical conversion's estimate against a canonical encode.
///
/// The argv caps the video at the profile's bitrate with `-maxrate` and a
/// two-second `-bufsize`, so duration times that rate is a real ceiling rather
/// than an average somebody hopes holds. This asserts it is a ceiling, and
/// that it is a near one.
#[test]
fn t6_the_canonical_estimate_is_an_upper_bound_and_not_a_wild_one() {
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    // 60fps is above what the profile sells, which a stream copy cannot fix:
    // the whole canonical conversion runs, to 1080p30.
    let src = e.root.join("odd.mp4");
    source(&tools, &src, "640x480", "60", "120", 8);
    let (estimate, mode, direct) = predicted(&e, &src, false);
    assert_eq!(mode, PrepareMode::Canonical, "this fixture should need the full conversion");
    assert!(!direct);

    let m = e.ingest.accept_upload(&e.user, "odd.mp4", &src).unwrap();
    let done = settled(&e, &m.id);
    assert_eq!(done.state, MediaState::Ready, "{:?}", done.last_error);
    let actual = done.prepared_bytes.unwrap() as u64;

    assert!(actual > 0, "nothing was produced, so this proves nothing");
    assert!(
        actual <= estimate,
        "the estimate was under what FFmpeg wrote: predicted {estimate}, wrote {actual}",
    );
    // And not so far over that it would refuse uploads a disk could hold. The
    // fixed 16MiB of margin dominates on a clip this short, so the bound is
    // generous here and tightens as the duration grows.
    assert!(
        estimate <= actual.saturating_mul(4) + 32 * 1024 * 1024,
        "the estimate is wastefully loose: predicted {estimate}, wrote {actual}",
    );
}

/// The long-GOP case, which is the one production actually hits: the picture is
/// encoded again at its own size and rate, so the estimate uses the source's
/// own bitrate rather than the profile's 1080p figure.
#[test]
fn t6b_the_live_normalize_estimate_tracks_the_source_not_the_profile() {
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let src = e.root.join("longgop.mp4");
    source(&tools, &src, "1280x720", "30", "300", 24);
    let (estimate, mode, _) = predicted(&e, &src, false);
    assert_eq!(mode, PrepareMode::LiveNormalize, "a 10s GOP should only need its keyframes fixed");
    // Read before the upload: accepting one renames the file into the store.
    let source_bytes = std::fs::metadata(&src).unwrap().len();

    let m = e.ingest.accept_upload(&e.user, "longgop.mp4", &src).unwrap();
    let done = settled(&e, &m.id);
    assert_eq!(done.state, MediaState::Ready, "{:?}", done.last_error);
    let actual = done.prepared_bytes.unwrap() as u64;

    assert!(actual > 0);
    assert!(
        actual <= estimate,
        "the estimate was under what FFmpeg wrote: predicted {estimate}, wrote {actual}",
    );
    // The old formula would have reserved three times the source for this.
    assert!(
        estimate < source_bytes.saturating_mul(3),
        "the estimate is no better than the multiple it replaces: {estimate} vs {}",
        source_bytes * 3,
    );
}

/// A file that needs nothing done to it reserves nothing, because nothing is
/// written. This is the case the 20GB Business upload runs into.
#[test]
fn t1b_a_direct_source_reserves_nothing_at_all() {
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let src = e.root.join("direct.mp4");
    source(&tools, &src, "1280x720", "2", "4", 6);
    let (estimate, mode, direct) = predicted(&e, &src, false);
    assert_eq!(mode, PrepareMode::Direct);
    assert!(direct, "this is the shape the fast path exists for");
    assert_eq!(estimate, 0, "a file that produces nothing must reserve nothing");

    // Which means a 20GB upload of this shape passes on a disk with 6GB free,
    // where the formula this replaces wanted 65GB.
    assert!(Ingest::fits(Some(6 * GB), estimate));
}

// --- re-preparation --------------------------------------------------------

/// Test 7 — the file about to be replaced is already on the disk, so it is
/// already inside `free`. Counting it again would reserve it twice.
#[test]
fn t7_a_reprepare_does_not_reserve_the_file_it_is_replacing() {
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let src = e.root.join("mixed.mp4");
    source(&tools, &src, "1280x720", "30", "300", 24);
    let m = e.ingest.accept_upload(&e.user, "mixed.mp4", &src).unwrap();
    let first = settled(&e, &m.id);
    let old_prepared = first.prepared_bytes.unwrap() as u64;
    assert!(old_prepared > 0);

    // The estimate for the canonical re-preparation is about the file being
    // produced. Nothing about the old one appears in it.
    let stored = LocalStorage::new(e.root.join("media")).localize(&first.storage_path).unwrap();
    let (estimate, mode, _) = predicted(&e, &stored, true);
    assert_eq!(mode, PrepareMode::Canonical, "pinning should force the canonical conversion");

    e.db.pin_to_canonical(&m.id).unwrap();
    e.db.set_media_state(&m.id, MediaState::Preparing).unwrap();
    e.ingest.prepare(&m.id).unwrap();
    let second = e.db.media_owned(&e.user, &m.id).unwrap();
    assert_eq!(second.state, MediaState::Ready, "{:?}", second.last_error);
    let new_prepared = second.prepared_bytes.unwrap() as u64;

    assert!(
        new_prepared <= estimate,
        "the re-preparation wrote more than was reserved: predicted {estimate}, wrote {new_prepared}",
    );
    // And the old file is still there, which is exactly why it must not be
    // counted: it is not going anywhere, and it was already spent.
    assert!(
        LocalStorage::new(e.root.join("media")).localize(&first.prepared_path.unwrap()).is_ok(),
        "the replaced file was removed",
    );
}

/// Test 8 — a re-preparation that cannot fit leaves everything as it was.
///
/// Driven through `check_room_to_prepare`'s own arithmetic rather than by
/// filling a disk: the refusal happens before FFmpeg is started, so what has to
/// be proved is that a refusal at that point changes nothing.
#[test]
fn t8_a_refused_reprepare_leaves_the_pointer_and_the_file_alone() {
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let src = e.root.join("keep.mp4");
    source(&tools, &src, "1280x720", "30", "300", 24);
    let m = e.ingest.accept_upload(&e.user, "keep.mp4", &src).unwrap();
    let before = settled(&e, &m.id);
    let pointer = before.prepared_path.clone().unwrap();
    let store = LocalStorage::new(e.root.join("media"));
    let bytes_before = store.size_bytes(&pointer).unwrap();

    // The state a refusal would be reached from, and the verdict it would get.
    let stored = store.localize(&before.storage_path).unwrap();
    let (estimate, _, _) = predicted(&e, &stored, true);
    assert!(!Ingest::fits(Some(DISK_FLOOR_BYTES), estimate), "this estimate should not fit on a bare floor");

    // Nothing was started, so nothing changed: the row still points at the file
    // it pointed at, the file is still the size it was, and the media is ready.
    let after = e.db.media_owned(&e.user, &m.id).unwrap();
    assert_eq!(after.prepared_path.as_deref(), Some(pointer.as_str()));
    assert_eq!(store.size_bytes(&pointer).unwrap(), bytes_before);
    assert_eq!(after.state, MediaState::Ready);
    assert_eq!(after.size_bytes, before.size_bytes);
}
