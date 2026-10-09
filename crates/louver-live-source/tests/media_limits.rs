//! The upload limits, at their boundaries.
//!
//! PHASE 4 built the checks and tested that each one fires. What it did not
//! test is the edges: a file of exactly the cap, a quota that is one byte from
//! full, two uploads racing for the same name, a disk that fills mid-write.
//! Those are where an off-by-one or a missed cleanup actually lives.
//!
//! ## How the big numbers are reached without big I/O
//!
//! Two of these limits are too large to exercise literally on a development
//! box with 5GB free, so each is reached by the cheapest honest route and the
//! substitution is named in the test:
//!
//!  * **the 4GB per-user quota** — with **sparse** files. `used_bytes` sums
//!    `metadata().len()`, and a file created with `set_len` reports its full
//!    length while occupying no blocks. So the quota *arithmetic* is tested
//!    exactly; what is not tested is 4GB of real writes.
//!  * **the 512MB per-file cap** — with a real 512MB buffer in memory, which
//!    the box has. Nothing of that size reaches the disk, because the size
//!    check happens before the write. Marked `#[ignore]` for the allocation,
//!    following this crate's convention for heavy tests.
//!
//! The disk-full test is **not** a substitution: it mounts a 2MB tmpfs and
//! fills it, so the `ENOSPC` is real.

mod common;

use common::*;
use louver_core::streaming::ffmpeg::FfmpegTools;
use louver_live_source::media::{MediaRoot, MAX_UPLOAD_BYTES, MAX_USER_BYTES};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const ALICE: &str = "user-alice";
const BOB: &str = "user-bob";

fn root_in(dir: &Path) -> MediaRoot {
    let media = dir.join("media");
    std::fs::create_dir_all(&media).unwrap();
    MediaRoot::new(&media, FfmpegTools::new(ffmpeg(), ffprobe())).unwrap()
}

fn root() -> (tempfile::TempDir, MediaRoot) {
    let dir = tempfile::tempdir().unwrap();
    let mr = root_in(dir.path());
    (dir, mr)
}

/// A real, tiny audio file. The probe looks at bytes, so a test of the upload
/// path needs bytes that are actually media.
fn tiny_media() -> Option<Vec<u8>> {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("t.m4a");
    let ok = std::process::Command::new(ffmpeg())
        .args([
            "-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "sine=frequency=440:duration=1",
            "-c:a", "aac",
        ])
        .arg(&out)
        .status()
        .ok()?
        .success();
    ok.then(|| std::fs::read(&out).ok()).flatten()
}

/// A sparse file of `len` bytes: full apparent size, no blocks used.
fn sparse(path: &Path, len: u64) {
    let f = std::fs::File::create(path).unwrap();
    f.set_len(len).unwrap();
    let got = std::fs::metadata(path).unwrap().len();
    assert_eq!(got, len, "the filesystem did not honour a sparse length");
}

/// Anything left in a user's directory that should not be.
fn temporaries(mr: &MediaRoot, user: &str) -> Vec<String> {
    let dir = mr.dir_for(user).unwrap();
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.starts_with(".tmp-"))
                .collect()
        })
        .unwrap_or_default()
}

/* ------------------------------------------- the per-file cap */

#[test]
#[ignore = "allocates 512MB in memory; run explicitly"]
fn the_per_file_cap_is_inclusive_and_one_byte_over_is_refused() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let (_d, mr) = root();

    // One byte over: refused for its size, and nothing is written at all —
    // the check is before the write, which is what keeps a 513MB upload from
    // costing 513MB of disk before being rejected.
    let over = vec![0u8; (MAX_UPLOAD_BYTES + 1) as usize];
    let e = mr.store(ALICE, "over.mp4", &over).unwrap_err();
    assert!(e.message.contains("너무 큽니다"), "{}", e.message);
    assert!(e.message.contains("512"), "the message should name the cap: {}", e.message);
    drop(over);
    assert!(temporaries(&mr, ALICE).is_empty(), "a refused-for-size upload wrote something");

    // Exactly at the cap: the size gate *passes* — so the cap is inclusive —
    // and the refusal that follows is the probe's, because 512MB of zeros is
    // not media. The distinction is the point: a different message proves a
    // different gate rejected it.
    let at = vec![0u8; MAX_UPLOAD_BYTES as usize];
    let e = mr.store(ALICE, "at.mp4", &at).unwrap_err();
    drop(at);
    assert!(!e.message.contains("너무 큽니다"), "the cap must be inclusive, not exclusive: {}", e.message);
    assert!(
        e.message.contains("영상") || e.message.contains("파일"),
        "expected the probe's refusal, got: {}",
        e.message
    );
    // And the half-written temporary is gone even though 512MB had been
    // written before the probe ran.
    assert!(temporaries(&mr, ALICE).is_empty(), "the temporary survived: {:?}", temporaries(&mr, ALICE));
    assert!(!mr.dir_for(ALICE).unwrap().join("at.mp4").exists());
}

/* ----------------------------------------- the per-user quota */

#[test]
fn the_per_user_quota_is_enforced_against_what_is_already_stored() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let Some(media) = tiny_media() else {
        eprintln!("SKIP: could not make a test file");
        return;
    };
    let (_d, mr) = root();
    let dir = mr.dir_for(ALICE).unwrap();

    // Sparse: the quota reads `len()`, so this is the real arithmetic against
    // a user who is 4KB short of full, at no disk cost.
    sparse(&dir.join("huge.mp4"), MAX_USER_BYTES - 4096);
    assert_eq!(mr.used_bytes(ALICE), MAX_USER_BYTES - 4096);

    // Over the line by the size of a real upload.
    assert!(media.len() > 4096, "the test file needs to be bigger than the headroom");
    let e = mr.store(ALICE, "nope.m4a", &media).unwrap_err();
    assert_eq!(e.kind, louver_live_source::ErrorKind::Limit, "{}", e.message);
    assert!(e.message.contains("저장 용량"), "{}", e.message);
    assert!(temporaries(&mr, ALICE).is_empty(), "a quota refusal left a temporary");

    // Bob is unaffected: the quota is per user, so Alice filling hers must not
    // spend his.
    assert_eq!(mr.used_bytes(BOB), 0);
    assert!(mr.store(BOB, "fine.m4a", &media).is_ok(), "Alice's usage blocked Bob");

    // And with room again, Alice can upload.
    std::fs::remove_file(dir.join("huge.mp4")).unwrap();
    assert!(mr.store(ALICE, "now.m4a", &media).is_ok());
}

#[test]
fn a_duplicate_name_replaces_and_is_not_counted_twice_against_the_quota() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let Some(media) = tiny_media() else {
        eprintln!("SKIP: no ffmpeg");
        return;
    };
    let (_d, mr) = root();

    let first = mr.store(ALICE, "song.m4a", &media).unwrap();
    let used_once = mr.used_bytes(ALICE);
    assert_eq!(used_once, first.bytes);

    // The same name again: one file, not two, and the usage does not double.
    let second = mr.store(ALICE, "song.m4a", &media).unwrap();
    assert_eq!(second.name, "song.m4a");
    assert_eq!(mr.used_bytes(ALICE), used_once, "a replacement was counted twice");
    assert_eq!(mr.list(ALICE).unwrap().len(), 1, "a replacement created a second entry");

    // Which is what makes a near-full user able to replace a file: without the
    // `replacing` subtraction, re-uploading the same file would be refused.
    let dir = mr.dir_for(ALICE).unwrap();
    std::fs::remove_file(dir.join("song.m4a")).unwrap();
    sparse(&dir.join("song.m4a"), MAX_USER_BYTES - 16);
    assert!(
        mr.store(ALICE, "song.m4a", &media).is_ok(),
        "replacing a file should not be refused for the space that file already holds"
    );
}

/* ------------------------------------------------ concurrency */

#[test]
fn concurrent_uploads_of_different_names_all_land_and_leave_no_temporaries() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let Some(media) = tiny_media() else {
        eprintln!("SKIP: no ffmpeg");
        return;
    };
    let (_d, mr) = root();
    let mr = Arc::new(mr);
    let media = Arc::new(media);

    let handles: Vec<_> = (0..8)
        .map(|i| {
            let (mr, media) = (Arc::clone(&mr), Arc::clone(&media));
            std::thread::spawn(move || mr.store(ALICE, &format!("s{i}.m4a"), &media))
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    for (i, r) in results.iter().enumerate() {
        assert!(r.is_ok(), "upload {i} failed: {:?}", r.as_ref().err().map(|e| &e.message));
    }
    assert_eq!(mr.list(ALICE).unwrap().len(), 8);
    // The temporary name carries the pid *and* the nanosecond clock, so eight
    // at once must not have collided — and none may be left behind.
    assert!(temporaries(&mr, ALICE).is_empty(), "{:?}", temporaries(&mr, ALICE));
}

#[test]
fn concurrent_uploads_of_the_same_name_leave_exactly_one_file() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let Some(media) = tiny_media() else {
        eprintln!("SKIP: no ffmpeg");
        return;
    };
    let (_d, mr) = root();
    let mr = Arc::new(mr);
    let media = Arc::new(media);

    // Six writers, one name. Each writes its own temporary and renames over
    // the same target; rename is atomic, so the loser's bytes are replaced
    // rather than interleaved. What must not happen is a surviving temporary
    // or a half-written target.
    let handles: Vec<_> = (0..6)
        .map(|_| {
            let (mr, media) = (Arc::clone(&mr), Arc::clone(&media));
            std::thread::spawn(move || mr.store(ALICE, "same.m4a", &media))
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(results.iter().any(|r| r.is_ok()), "every concurrent upload failed");

    let listed = mr.list(ALICE).unwrap();
    assert_eq!(listed.len(), 1, "expected one file, got {listed:?}");
    assert_eq!(listed[0].name, "same.m4a");
    assert_eq!(listed[0].bytes, media.len() as u64, "the file is whole, not interleaved");
    assert!(temporaries(&mr, ALICE).is_empty(), "{:?}", temporaries(&mr, ALICE));
    // And it is still readable as media, which a torn write would not be.
    assert!(mr.resolve(ALICE, "same.m4a").is_ok());
}

/* ------------------------------- replacing a file that is on air */

#[test]
fn replacing_a_file_does_not_disturb_a_reader_that_already_opened_it() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let Some(media) = tiny_media() else {
        eprintln!("SKIP: no ffmpeg");
        return;
    };
    let (_d, mr) = root();

    // A playlist item, and a manifest pointing at it — the same path a running
    // FFmpeg would hold.
    mr.store(ALICE, "onair.m4a", &media).unwrap();
    let path = mr.resolve(ALICE, "onair.m4a").unwrap();
    let job = _d.path().join("job-1");
    let manifest = mr.write_manifest(ALICE, &job, &["onair.m4a".to_string()]).unwrap();
    let manifest_body = std::fs::read_to_string(&manifest).unwrap();

    // FFmpeg has it open.
    let mut open = std::fs::File::open(&path).unwrap();
    let mut first = [0u8; 8];
    open.read_exact(&mut first).unwrap();

    // The user replaces it mid-broadcast with different bytes.
    let mut other = media.clone();
    let n = other.len();
    other[n - 1] ^= 0xFF;
    mr.store(ALICE, "onair.m4a", &other).unwrap();

    // `store` renames over the name, which swaps the directory entry and not
    // the inode. The already-open handle therefore still reads the bytes the
    // broadcast started with: a replacement cannot corrupt the file that is
    // playing. It is picked up when the concat demuxer next reopens the item,
    // which for `-stream_loop -1` is the next time round the playlist.
    let mut rest = Vec::new();
    open.read_to_end(&mut rest).unwrap();
    let mut seen = first.to_vec();
    seen.extend_from_slice(&rest);
    assert_eq!(seen, media, "the open handle saw the replacement; a broadcast could tear");
    assert_ne!(seen, other);

    // The manifest is unchanged, so no rewrite is needed and nothing has to be
    // signalled to the running worker.
    assert_eq!(std::fs::read_to_string(&manifest).unwrap(), manifest_body);
    // And the new bytes are what a fresh open gets.
    assert_eq!(std::fs::read(&path).unwrap(), other);
}

/* ------------------------------------------------ a full disk */

#[test]
fn a_full_disk_refuses_the_upload_and_leaves_no_temporary() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let Some(media) = tiny_media() else {
        eprintln!("SKIP: no ffmpeg");
        return;
    };
    // A real 2MB filesystem, so the ENOSPC is real rather than simulated.
    let mount = PathBuf::from(format!("/tmp/louver-tinyfs-{}", std::process::id()));
    std::fs::create_dir_all(&mount).unwrap();
    let mounted = std::process::Command::new("mount")
        .args(["-t", "tmpfs", "-o", "size=2M", "tmpfs"])
        .arg(&mount)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !mounted {
        eprintln!("SKIP: cannot mount a tmpfs (needs root)");
        let _ = std::fs::remove_dir(&mount);
        return;
    }
    // Unmount whatever happens below, including a panic.
    struct Unmount(PathBuf);
    impl Drop for Unmount {
        fn drop(&mut self) {
            let _ = std::process::Command::new("umount").arg(&self.0).status();
            let _ = std::fs::remove_dir(&self.0);
        }
    }
    let _guard = Unmount(mount.clone());

    let mr = root_in(&mount);
    // Fill it, leaving less room than the upload needs.
    let dir = mr.dir_for(ALICE).unwrap();
    let filler = dir.join("filler.bin");
    let mut wrote = 0u64;
    while wrote < 4 * 1024 * 1024 {
        if std::fs::write(&filler, vec![0u8; (wrote + 256 * 1024) as usize]).is_err() {
            break;
        }
        wrote += 256 * 1024;
    }
    assert!(wrote > 0, "could not fill the tiny filesystem");

    let e = mr.store(ALICE, "nospace.m4a", &media).unwrap_err();
    // The message is the write's, not a panic and not a success.
    assert_eq!(e.kind, louver_live_source::ErrorKind::Invalid, "{}", e.message);
    assert!(e.message.contains("저장할 수 없습니다"), "{}", e.message);
    // The path is not leaked to the user even in an I/O error.
    assert!(!e.message.contains("/tmp/louver-tinyfs"), "path leaked: {}", e.message);
    // And the partial write was cleaned up, which on a full disk is the whole
    // point: a temporary left behind would keep the space it failed to use.
    assert!(temporaries(&mr, ALICE).is_empty(), "{:?}", temporaries(&mr, ALICE));
    assert!(!dir.join("nospace.m4a").exists());
}

/* ------------------------------------------------- isolation */

#[test]
fn one_user_cannot_reach_another_users_file_through_any_entry_point() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg");
        return;
    }
    let Some(media) = tiny_media() else {
        eprintln!("SKIP: no ffmpeg");
        return;
    };
    let (_d, mr) = root();
    mr.store(ALICE, "private.m4a", &media).unwrap();

    // Every entry point that takes a name, from the other user.
    assert!(mr.resolve(BOB, "private.m4a").is_err(), "resolve");
    assert!(mr.delete(BOB, "private.m4a").is_err(), "delete");
    assert!(
        mr.write_manifest(BOB, &_d.path().join("job-x"), &["private.m4a".to_string()]).is_err(),
        "manifest"
    );
    assert!(!mr.list(BOB).unwrap().iter().any(|m| m.name == "private.m4a"), "list");
    assert_eq!(mr.used_bytes(BOB), 0, "quota");

    // Still there, and still Alice's.
    assert!(mr.resolve(ALICE, "private.m4a").is_ok());
    assert_eq!(std::fs::read(mr.resolve(ALICE, "private.m4a").unwrap()).unwrap(), media);

    // A replacement attempt under Bob's id writes into *Bob's* directory and
    // leaves Alice's file alone — it does not fail silently into hers.
    mr.store(BOB, "private.m4a", &media).unwrap();
    assert_eq!(mr.list(ALICE).unwrap().len(), 1);
    assert_eq!(mr.list(BOB).unwrap().len(), 1);
    assert_ne!(mr.resolve(ALICE, "private.m4a").unwrap(), mr.resolve(BOB, "private.m4a").unwrap());
}
