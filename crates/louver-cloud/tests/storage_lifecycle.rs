//! What a media costs on disk, and what happens to the files over its life.
//!
//! Every test here is about bytes that exist or do not exist on a real
//! filesystem, so every fixture is real media put through the real preparation.
//! Skips loudly when FFmpeg is absent rather than passing on nothing.

use louver_cloud::db::NewItem;
use louver_cloud::ingest::Ingest;
use louver_cloud::media_audit::{self, Role};
use louver_cloud::storage::{LocalStorage, Storage};
use louver_cloud::{CloudDb, MediaState};
use louver_core::streaming::ffmpeg::FfmpegTools;
use std::sync::Arc;

fn tools() -> Option<FfmpegTools> {
    let sidecar =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apps/desktop/src-tauri/binaries");
    FfmpegTools::discover(Some(&sidecar)).ok()
}

struct Env {
    _dir: tempfile::TempDir,
    db: CloudDb,
    ingest: Ingest,
    user: String,
    root: std::path::PathBuf,
}

impl Env {
    fn store(&self) -> LocalStorage {
        LocalStorage::new(self.root.join("media"))
    }
    /// Every file under the object store, as `(key, bytes)`.
    fn files(&self) -> Vec<(String, u64)> {
        let root = self.root.join("media");
        let mut out = Vec::new();
        fn go(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, u64)>) {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    go(root, &p, out);
                } else if p.is_file() {
                    let key = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                    out.push((key, e.metadata().unwrap().len()));
                }
            }
        }
        go(&root, &root, &mut out);
        out.sort();
        out
    }
    fn audit(&self) -> media_audit::MediaStorageAudit {
        media_audit::audit(&self.db, &self.root).unwrap()
    }
}

fn env(tools: &FfmpegTools) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let db = CloudDb::open(&dir.path().join("cloud.db")).unwrap();
    let user = db.create_user("disk@x.com", "hash", "business").unwrap().id;
    let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(dir.path().join("media")));
    let ingest = Ingest::new(db.clone(), storage, tools.clone(), "libx264".into());
    let root = dir.path().to_path_buf();
    Env { _dir: dir, db, ingest, user, root }
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

/// A clip that needs nothing done to it, at whatever shape a test asks for.
///
/// `gop` is in frames; at `rate` frames a second, `gop / rate` seconds of
/// keyframe spacing is what decides whether the picture can be copied.
fn source(tools: &FfmpegTools, at: &std::path::Path, size: &str, rate: &str, gop: &str, sample_rate: &str) {
    source_of(tools, at, size, rate, gop, sample_rate, 6)
}

/// The same, long enough that a keyframe gap can be *measured*.
///
/// A gap needs two keyframes to exist between, so a clip shorter than the
/// spacing being tested has nothing to measure and reads as conformant. The
/// ten-second-GOP fixtures below are twenty-four seconds for that reason.
fn source_of(
    tools: &FfmpegTools,
    at: &std::path::Path,
    size: &str,
    rate: &str,
    gop: &str,
    sample_rate: &str,
    secs: u32,
) {
    let video = format!("testsrc2=size={size}:rate={rate}:duration={secs}");
    let audio = format!("sine=frequency=440:sample_rate={sample_rate}:duration={secs}");
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
            sample_rate,
            "-movflags",
            "+faststart",
        ],
    );
}

// --- A, B: a file that needs nothing is copied nowhere ----------------------

/// Test A — the production file's shape: 720p, 2fps, GOP 2s, AAC 44.1 kHz.
#[test]
fn a_direct_720p_2fps_source_is_broadcast_from_where_it_lies() {
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let src = e.root.join("export.mp4");
    source(&tools, &src, "1280x720", "2", "4", "44100");
    let uploaded = std::fs::metadata(&src).unwrap().len();

    let m = e.ingest.accept_upload(&e.user, "playlist-export.mp4", &src).unwrap();
    let done = settled(&e, &m.id);
    assert_eq!(done.state, MediaState::Ready, "{:?}", done.last_error);

    assert_eq!(done.prepared_mode(&e.db), "direct", "this shape is exactly what direct means");
    assert_eq!(
        done.prepared_path.as_deref(),
        Some(done.storage_path.as_str()),
        "a broadcast should read the upload itself",
    );

    // The whole point: one file, not two.
    let files = e.files();
    assert_eq!(files.len(), 1, "a copy was made anyway: {files:?}");
    assert_eq!(files[0].1, uploaded);

    // And the accounting says so, in both halves.
    assert_eq!(done.source_bytes, Some(uploaded as i64));
    assert_eq!(done.prepared_bytes, Some(0), "there is no converted copy to charge for");
    assert_eq!(done.size_bytes, uploaded as i64);
    assert_eq!(e.db.storage_used(&e.user).unwrap(), uploaded as i64, "quota must match the disk");

    // A broadcast can still be started from it.
    let ready = e.db.prepared_media_for(&m.id).unwrap();
    assert!(e.store().localize(&ready.prepared_key).is_ok(), "the pointer does not resolve");
}

/// Test B — the other direct shape, at the profile's own geometry.
#[test]
fn b_direct_1080p30_source_is_not_duplicated_either() {
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let src = e.root.join("hd.mp4");
    source(&tools, &src, "1920x1080", "30", "60", "48000");
    let uploaded = std::fs::metadata(&src).unwrap().len();

    let m = e.ingest.accept_upload(&e.user, "hd.mp4", &src).unwrap();
    let done = settled(&e, &m.id);
    assert_eq!(done.state, MediaState::Ready, "{:?}", done.last_error);
    assert_eq!(done.prepared_mode(&e.db), "direct");
    assert_eq!(e.files().len(), 1);
    assert_eq!(done.size_bytes, uploaded as i64);
    assert_eq!(done.prepared_bytes, Some(0));
}

// --- C: a file that does need work still gets it ----------------------------

/// Test C — keyframes ten seconds apart. The picture has to be encoded again,
/// so there is a second file and both are charged for.
#[test]
fn c_a_long_gop_source_is_still_converted_and_both_files_are_counted() {
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let src = e.root.join("longgop.mp4");
    // 30fps with a keyframe every 300 frames is ten seconds — past the four
    // second ceiling a live ingest allows.
    source_of(&tools, &src, "1280x720", "30", "300", "48000", 24);
    let uploaded = std::fs::metadata(&src).unwrap().len();

    let m = e.ingest.accept_upload(&e.user, "longgop.mp4", &src).unwrap();
    let done = settled(&e, &m.id);
    assert_eq!(done.state, MediaState::Ready, "{:?}", done.last_error);

    assert_ne!(done.prepared_mode(&e.db), "direct", "a 10s GOP cannot be sent as it is");
    assert_ne!(
        done.prepared_path.as_deref(),
        Some(done.storage_path.as_str()),
        "a converted file must be its own object",
    );
    let files = e.files();
    assert_eq!(files.len(), 2, "the upload and its conversion: {files:?}");

    assert_eq!(done.source_bytes, Some(uploaded as i64));
    assert!(done.prepared_bytes.unwrap() > 0);
    assert_eq!(done.size_bytes, done.source_bytes.unwrap() + done.prepared_bytes.unwrap());
    let on_disk: u64 = files.iter().map(|(_, b)| b).sum();
    assert_eq!(done.size_bytes, on_disk as i64, "the total must be what the disk holds");
}

// --- D, E: replacing a prepared file ---------------------------------------

/// Test D — a re-preparation swaps the pointer and writes the old file off
/// without unlinking it.
#[test]
fn d_a_reprepare_swaps_the_pointer_and_never_unlinks_the_old_file() {
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let src = e.root.join("mixed.mp4");
    source_of(&tools, &src, "1280x720", "30", "300", "48000", 24);

    let m = e.ingest.accept_upload(&e.user, "mixed.mp4", &src).unwrap();
    let first = settled(&e, &m.id);
    let old = first.prepared_path.clone().unwrap();
    assert!(e.store().localize(&old).is_ok());

    // What a mixed playlist does: aim the next preparation at the canonical
    // format and run it again.
    e.db.pin_to_canonical(&m.id).unwrap();
    e.db.set_media_state(&m.id, MediaState::Preparing).unwrap();
    e.ingest.prepare(&m.id).unwrap();
    let second = e.db.media_owned(&e.user, &m.id).unwrap();
    assert_eq!(second.state, MediaState::Ready, "{:?}", second.last_error);

    let new = second.prepared_path.clone().unwrap();
    assert_ne!(new, old, "the pointer did not move");
    assert!(e.store().localize(&new).is_ok(), "the new file is not there");
    assert!(
        e.store().localize(&old).is_ok(),
        "the old file was unlinked — a broadcast reading it would die at the next restart",
    );

    // It is written off, with its size, so an operator can find it.
    let trashed: Vec<(String, i64)> = {
        let conn = e.db.raw();
        let guard = conn.lock().unwrap();
        let mut st = guard.prepare("SELECT path, bytes FROM storage_trash").unwrap();
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        rows.map(|r| r.unwrap()).collect()
    };
    assert_eq!(trashed.len(), 1, "the replaced file was not recorded: {trashed:?}");
    assert_eq!(trashed[0].0, old);
    assert!(trashed[0].1 > 0);

    // And the account is charged for what it now points at, not for both.
    assert_eq!(second.size_bytes, second.source_bytes.unwrap() + second.prepared_bytes.unwrap());

    // The audit finds the leftover, and tells the truth about why it is there.
    let report = e.audit();
    let retired: Vec<_> = report.objects.iter().filter(|o| o.role == Role::Retired).collect();
    assert_eq!(retired.len(), 1, "{:?}", report.objects);
    assert_eq!(retired[0].key, old);
    assert!(!retired[0].referenced_by_db);
    assert_eq!(report.totals.retired_files, 1);
}

/// Test E — a file something is reading is never called safe to remove.
#[test]
fn e_a_file_that_is_open_or_in_a_manifest_is_not_cleanup_material() {
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let src = e.root.join("busy.mp4");
    source_of(&tools, &src, "1280x720", "30", "300", "48000", 24);
    let m = e.ingest.accept_upload(&e.user, "busy.mp4", &src).unwrap();
    let first = settled(&e, &m.id);
    let old = first.prepared_path.clone().unwrap();

    e.db.pin_to_canonical(&m.id).unwrap();
    e.db.set_media_state(&m.id, MediaState::Preparing).unwrap();
    e.ingest.prepare(&m.id).unwrap();

    // Nothing is reading it yet, so it is a candidate.
    let before = e.audit();
    let row = before.objects.iter().find(|o| o.key == old).unwrap();
    assert!(before.open_files_known, "this platform cannot answer the question that matters");
    assert!(row.safe_to_delete, "an unreferenced, unopened leftover should be a candidate");

    // Now hold it open, the way the FFmpeg of a broadcast that started before
    // the re-preparation still does.
    let path = e.store().localize(&old).unwrap();
    let held = std::fs::File::open(&path).unwrap();
    let during = e.audit();
    let row = during.objects.iter().find(|o| o.key == old).unwrap();
    assert!(row.open_now, "an open file was not seen as open");
    assert!(!row.safe_to_delete, "a file being read was offered for deletion");
    drop(held);

    // And a manifest naming it counts even when no process is running: a
    // broadcast re-reads its manifest on every loop.
    let work = e.root.join("work/b1");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("manifest.txt"), format!("ffconcat version 1.0\nfile '{}'\n", path.display()))
        .unwrap();
    let after = e.audit();
    let row = after.objects.iter().find(|o| o.key == old).unwrap();
    assert!(row.in_manifest, "a manifest reference was missed");
    assert!(!row.safe_to_delete, "a file a playlist names was offered for deletion");
}

// --- F, G: the audit ---------------------------------------------------------

/// Test F — a file nothing points at is found.
#[test]
fn f_an_unreferenced_file_is_reported_as_an_orphan() {
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let src = e.root.join("one.mp4");
    source(&tools, &src, "1280x720", "2", "4", "48000");
    let m = e.ingest.accept_upload(&e.user, "one.mp4", &src).unwrap();
    settled(&e, &m.id);

    // Something nobody recorded — the shape of an artifact left by a build that
    // no longer exists.
    let stray = e.root.join("media").join(&e.user).join("left-behind.mp4");
    std::fs::write(&stray, vec![7u8; 4096]).unwrap();

    let report = e.audit();
    let orphans: Vec<_> = report.objects.iter().filter(|o| o.role == Role::Orphan).collect();
    assert_eq!(orphans.len(), 1, "{:?}", report.objects);
    assert!(orphans[0].key.ends_with("left-behind.mp4"));
    assert_eq!(orphans[0].bytes, 4096);
    assert!(orphans[0].safe_to_delete);
    assert_eq!(report.totals.orphan_files, 1);
    assert_eq!(report.totals.orphan_bytes, 4096);
}

/// Test G — nothing a media row points at is ever a candidate, including the
/// source of a media that is broadcast directly from it.
#[test]
fn g_referenced_files_are_never_offered_for_deletion() {
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let direct = e.root.join("direct.mp4");
    source(&tools, &direct, "1280x720", "2", "4", "44100");
    let a = e.ingest.accept_upload(&e.user, "direct.mp4", &direct).unwrap();
    settled(&e, &a.id);

    let converted = e.root.join("conv.mp4");
    source_of(&tools, &converted, "1280x720", "30", "300", "48000", 24);
    let b = e.ingest.accept_upload(&e.user, "conv.mp4", &converted).unwrap();
    settled(&e, &b.id);

    let report = e.audit();
    assert!(report.objects.len() >= 3, "{:?}", report.objects);
    for o in &report.objects {
        assert!(o.referenced_by_db, "an expected file was unreferenced: {o:?}");
        assert!(!o.safe_to_delete, "a live file was offered for deletion: {o:?}");
    }
    assert_eq!(report.totals.reclaimable_files, 0);
    assert!(report.accounting_drift.is_empty(), "{:?}", report.accounting_drift);

    // The direct upload is labelled for what it is, so the report explains why
    // there is only one file for it.
    assert!(report.objects.iter().any(|o| o.role == Role::SourceDirect));
    assert!(report.objects.iter().any(|o| o.role == Role::Prepared));
}

// --- H, I, J: the rest of the life -----------------------------------------

/// Test H — the pointer survives a restart and still resolves.
#[test]
fn h_a_direct_media_still_starts_after_a_restart() {
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let src = e.root.join("restart.mp4");
    source(&tools, &src, "1280x720", "2", "4", "44100");
    let m = e.ingest.accept_upload(&e.user, "restart.mp4", &src).unwrap();
    settled(&e, &m.id);

    // A new process, opening the same database file: what a container restart
    // is, as far as this question goes.
    let again = CloudDb::open(&e.root.join("cloud.db")).unwrap();
    let ready = again.prepared_media_for(&m.id).unwrap();
    let path = e.store().localize(&ready.prepared_key).expect("the pointer no longer resolves");
    assert!(path.is_file());
    assert!(ready.duration_secs > 1.0);
}

/// Test I — a playlist may hold both kinds, and the existing compatibility
/// check is what decides, exactly as before.
#[test]
fn i_a_mixed_playlist_is_judged_by_signature_not_by_where_the_file_came_from() {
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let one = e.root.join("m1.mp4");
    source(&tools, &one, "1280x720", "2", "4", "44100");
    let a = e.ingest.accept_upload(&e.user, "m1.mp4", &one).unwrap();
    let a = settled(&e, &a.id);
    assert_eq!(a.prepared_mode(&e.db), "direct");

    let two = e.root.join("m2.mp4");
    source_of(&tools, &two, "1280x720", "30", "300", "48000", 24);
    let b = e.ingest.accept_upload(&e.user, "m2.mp4", &two).unwrap();
    let b = settled(&e, &b.id);
    assert_ne!(b.prepared_mode(&e.db), "direct");

    let dest = e.db.create_destination(&e.user, "ch", "rtmps://a/live2", "••••").unwrap();
    let cast = e.db.create_broadcast(&e.user, "mix", &a.id, &dest.id, true).unwrap();
    e.db.replace_items(
        &e.user,
        &cast.id,
        &[
            NewItem { media_id: a.id.clone(), enabled: true, repeat_count: 1 },
            NewItem { media_id: b.id.clone(), enabled: true, repeat_count: 1 },
        ],
    )
    .unwrap();

    // Two shapes that do not match: the check refuses, which is what sends them
    // down the canonical path. A direct item takes part in that exactly like
    // any other — the check reads signatures and knows nothing else.
    let shapes = e.db.playlist_shapes(&cast.id).unwrap();
    assert_eq!(shapes.len(), 2);
    assert!(shapes.iter().all(|(_, _, sig)| sig.is_some()), "a direct item has no signature: {shapes:?}");
    assert!(e.db.check_playlist_joinable(&cast.id).is_err(), "a 720p2fps and a 720p30 were called joinable");

    // Two direct items of the same shape do join.
    let three = e.root.join("m3.mp4");
    source(&tools, &three, "1280x720", "2", "4", "44100");
    let c = e.ingest.accept_upload(&e.user, "m3.mp4", &three).unwrap();
    let c = settled(&e, &c.id);
    e.db.replace_items(
        &e.user,
        &cast.id,
        &[
            NewItem { media_id: a.id.clone(), enabled: true, repeat_count: 1 },
            NewItem { media_id: c.id.clone(), enabled: true, repeat_count: 1 },
        ],
    )
    .unwrap();
    e.db.check_playlist_joinable(&cast.id).expect("two identical direct sources should join");
}

/// Test J — deleting a media removes its files, and a media a playlist names
/// cannot be deleted at all.
#[test]
fn j_delete_clears_the_files_and_a_playlist_item_cannot_be_deleted() {
    let Some(tools) = tools() else {
        eprintln!("SKIP: no FFmpeg sidecar");
        return;
    };
    let e = env(&tools);
    let direct = e.root.join("d.mp4");
    source(&tools, &direct, "1280x720", "2", "4", "44100");
    let a = e.ingest.accept_upload(&e.user, "d.mp4", &direct).unwrap();
    let a = settled(&e, &a.id);

    let conv = e.root.join("c.mp4");
    source_of(&tools, &conv, "1280x720", "30", "300", "48000", 24);
    let b = e.ingest.accept_upload(&e.user, "c.mp4", &conv).unwrap();
    let b = settled(&e, &b.id);

    // `b` is only ever a playlist item, never the broadcast's own media_id.
    // Before this release that made it deletable while a broadcast read it.
    let dest = e.db.create_destination(&e.user, "ch", "rtmps://a/live2", "••••").unwrap();
    let cast = e.db.create_broadcast(&e.user, "show", &a.id, &dest.id, true).unwrap();
    e.db.replace_items(
        &e.user,
        &cast.id,
        &[NewItem { media_id: b.id.clone(), enabled: true, repeat_count: 1 }],
    )
    .unwrap();
    let refused = e.db.delete_media_owned(&e.user, &b.id);
    assert!(refused.is_err(), "a video a playlist is about to read was deleted");

    // A media nothing names deletes cleanly, and both of its paths go.
    let lone = e.root.join("lone.mp4");
    source_of(&tools, &lone, "1280x720", "30", "300", "48000", 24);
    let c = e.ingest.accept_upload(&e.user, "lone.mp4", &lone).unwrap();
    let c = settled(&e, &c.id);
    let (src_key, prep_key) = (c.storage_path.clone(), c.prepared_path.clone().unwrap());
    let gone = e.db.delete_media_owned(&e.user, &c.id).unwrap();
    let store = e.store();
    store.delete(&gone.storage_path).unwrap();
    if let Some(p) = &gone.prepared_path {
        store.delete(p).unwrap();
    }
    assert!(store.localize(&src_key).is_err(), "the upload survived the delete");
    assert!(store.localize(&prep_key).is_err(), "the conversion survived the delete");

    // Deleting a direct media asks the store to remove the same key twice —
    // which must be harmless, and must take the one file with it.
    let a_src = a.storage_path.clone();
    // Deleting the broadcast takes its playlist rows with it, which is what
    // releases both videos.
    e.db.delete_broadcast_owned(&e.user, &cast.id).unwrap();
    e.db.delete_media_owned(&e.user, &b.id).expect("the playlist item is free once the broadcast is gone");
    let gone = e.db.delete_media_owned(&e.user, &a.id).unwrap();
    store.delete(&gone.storage_path).unwrap();
    store.delete(gone.prepared_path.as_ref().unwrap()).unwrap();
    assert!(store.localize(&a_src).is_err(), "the direct upload survived the delete");
}

/// The mode the preparation recorded, which is not on `CloudMedia` because a
/// browser has no use for it.
trait PreparedMode {
    fn prepared_mode(&self, db: &CloudDb) -> String;
}

impl PreparedMode for louver_cloud::CloudMedia {
    fn prepared_mode(&self, db: &CloudDb) -> String {
        db.raw()
            .lock()
            .unwrap()
            .query_row("SELECT COALESCE(prepared_mode,'') FROM media WHERE id=?1", [&self.id], |r| r.get(0))
            .unwrap()
    }
}
