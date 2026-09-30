//! What is on the disk, what points at it, and what is reading it.
//!
//! Read-only, all of it. Nothing in this file unlinks, renames, writes or even
//! opens a media file — it stats them, reads the manifests a broadcast is
//! running from, and looks at which paths the kernel says are open. An operator
//! runs it, reads it, and decides. That separation is the point: a garbage
//! collector that runs by itself is correct until the day a path resolves
//! differently than it used to, and then it is a data-loss bug with no undo.
//!
//! The question it answers is narrow and has to be answered conservatively:
//! **is this file safe to remove?** A file is only ever called safe when every
//! one of these is true —
//!
//! * no `media` row names it, as its source or as its prepared file;
//! * no manifest under the work directory names it;
//! * no running process holds it open;
//! * and we were actually able to check that last one.
//!
//! Anything unknown makes the answer no. A false "safe" deletes a broadcast's
//! input; a false "not safe" costs disk until the next run.

use crate::db::CloudDb;
use crate::Result;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// What a file on the disk is, as far as the database is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// An upload, and also what a broadcast reads: nothing had to be done to it.
    SourceDirect,
    /// An upload that has a converted copy beside it.
    Source,
    /// A converted copy, which is what a broadcast reads.
    Prepared,
    /// Written off by a re-preparation. Recorded in `storage_trash`.
    Retired,
    /// Nothing points at it.
    Orphan,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Self::SourceDirect => "source (직접 송출)",
            Self::Source => "source",
            Self::Prepared => "prepared",
            Self::Retired => "retired",
            Self::Orphan => "orphan",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ObjectRow {
    /// The object key: the path under the media root, which is what the
    /// database stores.
    pub key: String,
    pub bytes: u64,
    pub role: Role,
    pub media_id: Option<String>,
    pub user_id: Option<String>,
    pub filename: Option<String>,
    /// A `media` row names this file.
    pub referenced_by_db: bool,
    /// A manifest under the work directory names this file.
    pub in_manifest: bool,
    /// A running process holds this file open.
    pub open_now: bool,
    /// Every check above came back negative, and every check could be made.
    pub safe_to_delete: bool,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Totals {
    pub files: usize,
    pub bytes: u64,
    pub source_bytes: u64,
    pub prepared_bytes: u64,
    pub orphan_files: usize,
    pub orphan_bytes: u64,
    pub retired_files: usize,
    pub retired_bytes: u64,
    /// Orphaned or retired, nothing open, nothing pointing at it.
    pub reclaimable_files: usize,
    pub reclaimable_bytes: u64,
}

/// The volume itself, which is the thing the floor actually protects.
///
/// Separate from every quota on purpose. A plan's storage ceiling is what an
/// account was sold; this is what the machine has. The two are allowed to
/// disagree — several accounts may be sold more in total than the disk holds,
/// which is ordinary — and the floor is what keeps that from becoming a server
/// that cannot write.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Volume {
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub used_bytes: u64,
    pub floor_bytes: u64,
    /// Free space above the floor: what an upload or a conversion may use.
    pub usable_bytes: u64,
    /// `false` when the volume could not be identified, in which case every
    /// figure above is zero and none of them means anything.
    pub known: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MediaStorageAudit {
    pub media_root: String,
    pub volume: Volume,
    pub objects: Vec<ObjectRow>,
    /// Scratch directories left behind by a preparation that died.
    pub scratch_bytes: u64,
    /// Manifests found, and how many entries each named.
    pub manifests: Vec<(String, usize)>,
    /// Whether open files could be listed at all. `false` makes every
    /// `safe_to_delete` false, because the question could not be answered.
    pub open_files_known: bool,
    /// Rows whose `size_bytes` disagrees with what the files actually measure.
    pub accounting_drift: Vec<Drift>,
    pub totals: Totals,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Drift {
    pub media_id: String,
    pub filename: String,
    pub db_bytes: i64,
    pub disk_bytes: i64,
}

/// One row of `media`, as the audit needs it.
struct MediaRef {
    id: String,
    user_id: String,
    filename: String,
    storage_path: String,
    prepared_path: Option<String>,
    size_bytes: i64,
}

/// Every path a running process has open, when that can be known.
///
/// Linux only, and deliberately not faked anywhere else: on a platform where
/// this cannot be read the audit says so and refuses to call anything safe,
/// rather than returning an empty set that would read as "nothing is open".
#[cfg(target_os = "linux")]
fn open_paths() -> Option<HashSet<PathBuf>> {
    let mut out = HashSet::new();
    let procs = std::fs::read_dir("/proc").ok()?;
    for p in procs.flatten() {
        let name = p.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(p.path().join("fd")) else { continue };
        for fd in fds.flatten() {
            if let Ok(target) = std::fs::read_link(fd.path()) {
                out.insert(target);
            }
        }
    }
    Some(out)
}

#[cfg(not(target_os = "linux"))]
fn open_paths() -> Option<HashSet<PathBuf>> {
    None
}

/// Every `file '...'` line of every manifest under the work directory.
///
/// This is the one reference a database cannot show: FFmpeg was handed a
/// manifest at start and reads it again on every loop, so a file named there
/// is in use whatever the `media` rows say now.
fn manifest_entries(work_root: &Path) -> (HashSet<PathBuf>, Vec<(String, usize)>) {
    let mut paths = HashSet::new();
    let mut found = Vec::new();
    let Ok(dirs) = std::fs::read_dir(work_root) else { return (paths, found) };
    for d in dirs.flatten() {
        let manifest = d.path().join("manifest.txt");
        let Ok(text) = std::fs::read_to_string(&manifest) else { continue };
        let mut n = 0;
        for line in text.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("file '") else { continue };
            let Some(path) = rest.strip_suffix('\'') else { continue };
            // The manifest escapes a literal quote as '\'' — undo it so the
            // comparison is against the path FFmpeg opens.
            paths.insert(PathBuf::from(path.replace("'\\''", "'")));
            n += 1;
        }
        found.push((d.file_name().to_string_lossy().into_owned(), n));
    }
    (paths, found)
}

fn dir_bytes(dir: &Path) -> u64 {
    let mut total = 0;
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    for e in entries.flatten() {
        match e.file_type() {
            Ok(t) if t.is_dir() => total += dir_bytes(&e.path()),
            Ok(t) if t.is_file() => total += e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => {}
        }
    }
    total
}

/// Walk the media root, returning `(object key, path, bytes)` for each file.
///
/// The key is the path relative to the root, which is exactly what the database
/// stores, so the two can be compared as strings without guessing.
fn walk_objects(root: &Path) -> Vec<(String, PathBuf, u64)> {
    let mut out = Vec::new();
    fn go(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf, u64)>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let path = e.path();
            // `.scratch` is a preparation's workspace, counted separately: a
            // file in it is mid-production, not an object anybody points at.
            if path.file_name().is_some_and(|n| n == ".scratch") {
                continue;
            }
            match e.file_type() {
                Ok(t) if t.is_dir() => go(root, &path, out),
                Ok(t) if t.is_file() => {
                    let key = path
                        .strip_prefix(root)
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| path.to_string_lossy().into_owned());
                    let bytes = e.metadata().map(|m| m.len()).unwrap_or(0);
                    out.push((key, path, bytes));
                }
                _ => {}
            }
        }
    }
    go(root, root, &mut out);
    out
}

/// Look at the disk and the database, and say what is where.
///
/// `data_dir` is the server's `LOUVER_DATA_DIR`: `media/` under it is the
/// object store, `work/` is where a running broadcast keeps its manifest.
pub fn audit(db: &CloudDb, data_dir: &Path) -> Result<MediaStorageAudit> {
    let media_root = data_dir.join("media");
    let work_root = data_dir.join("work");

    let rows: Vec<MediaRef> = {
        let conn = db.raw();
        let guard = conn.lock().unwrap();
        let mut st = guard
            .prepare("SELECT id, user_id, filename, storage_path, prepared_path, size_bytes FROM media")?;
        let it = st.query_map([], |r| {
            Ok(MediaRef {
                id: r.get(0)?,
                user_id: r.get(1)?,
                filename: r.get(2)?,
                storage_path: r.get(3)?,
                prepared_path: r.get(4)?,
                size_bytes: r.get(5)?,
            })
        })?;
        it.collect::<std::result::Result<Vec<_>, _>>()?
    };

    let retired: HashMap<String, String> = {
        let conn = db.raw();
        let guard = conn.lock().unwrap();
        let mut st = guard.prepare("SELECT path, reason FROM storage_trash")?;
        let it = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        it.collect::<std::result::Result<HashMap<_, _>, _>>()?
    };

    // Which media row, if any, each key belongs to — and in which capacity.
    let mut by_key: HashMap<&str, (&MediaRef, Role)> = HashMap::new();
    for m in &rows {
        let direct = m.prepared_path.as_deref() == Some(m.storage_path.as_str());
        by_key.insert(m.storage_path.as_str(), (m, if direct { Role::SourceDirect } else { Role::Source }));
        if let Some(p) = &m.prepared_path {
            if !direct {
                by_key.insert(p.as_str(), (m, Role::Prepared));
            }
        }
    }

    let open = open_paths();
    let open_files_known = open.is_some();
    let open = open.unwrap_or_default();
    let (manifest_paths, manifests) = manifest_entries(&work_root);

    let mut objects = Vec::new();
    let mut totals = Totals::default();
    for (key, path, bytes) in walk_objects(&media_root) {
        let hit = by_key.get(key.as_str());
        let role = match hit {
            Some((_, role)) => *role,
            None if retired.contains_key(&key) => Role::Retired,
            None => Role::Orphan,
        };
        // Compare resolved paths: the manifest and /proc both name the file as
        // the kernel sees it, and the media root may be reached through a
        // symlink on some deployments.
        let real = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        let open_now = open.contains(&real) || open.contains(&path);
        let in_manifest = manifest_paths.contains(&real) || manifest_paths.contains(&path);
        let referenced_by_db = hit.is_some();

        totals.files += 1;
        totals.bytes += bytes;
        match role {
            Role::SourceDirect | Role::Source => totals.source_bytes += bytes,
            Role::Prepared => totals.prepared_bytes += bytes,
            Role::Retired => {
                totals.retired_files += 1;
                totals.retired_bytes += bytes;
            }
            Role::Orphan => {
                totals.orphan_files += 1;
                totals.orphan_bytes += bytes;
            }
        }

        let safe_to_delete = open_files_known
            && !referenced_by_db
            && !in_manifest
            && !open_now
            && matches!(role, Role::Orphan | Role::Retired);
        if safe_to_delete {
            totals.reclaimable_files += 1;
            totals.reclaimable_bytes += bytes;
        }

        objects.push(ObjectRow {
            key,
            bytes,
            role,
            media_id: hit.map(|(m, _)| m.id.clone()),
            user_id: hit.map(|(m, _)| m.user_id.clone()),
            filename: hit.map(|(m, _)| m.filename.clone()),
            referenced_by_db,
            in_manifest,
            open_now,
            safe_to_delete,
        });
    }
    objects.sort_by_key(|o| std::cmp::Reverse(o.bytes));

    // What each row claims against what its files measure. A legacy row whose
    // prepared file was replaced reads high by exactly the orphan's size, which
    // is how the drift and the orphan list corroborate each other.
    let on_disk: HashMap<&str, u64> = objects.iter().map(|o| (o.key.as_str(), o.bytes)).collect();
    let mut accounting_drift = Vec::new();
    for m in &rows {
        let mut disk = *on_disk.get(m.storage_path.as_str()).unwrap_or(&0) as i64;
        if let Some(p) = &m.prepared_path {
            if p != &m.storage_path {
                disk += *on_disk.get(p.as_str()).unwrap_or(&0) as i64;
            }
        }
        if disk != m.size_bytes {
            accounting_drift.push(Drift {
                media_id: m.id.clone(),
                filename: m.filename.clone(),
                db_bytes: m.size_bytes,
                disk_bytes: disk,
            });
        }
    }

    let (total, free) = louver_core::system::disk_capacity_bytes(&media_root);
    let volume = Volume {
        total_bytes: total,
        free_bytes: free,
        used_bytes: total.saturating_sub(free),
        floor_bytes: crate::ingest::DISK_FLOOR_BYTES,
        usable_bytes: free.saturating_sub(crate::ingest::DISK_FLOOR_BYTES),
        known: total > 0,
    };

    Ok(MediaStorageAudit {
        media_root: media_root.to_string_lossy().into_owned(),
        volume,
        objects,
        scratch_bytes: dir_bytes(&media_root.join(".scratch")),
        manifests,
        open_files_known,
        accounting_drift,
        totals,
    })
}

/// Bytes, for a person reading a terminal.
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manifest_is_read_back_as_the_paths_ffmpeg_opens() {
        let d = tempfile::tempdir().unwrap();
        let work = d.path().join("work/b1");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("manifest.txt"), "ffconcat version 1.0\nfile '/m/a.mp4'\nfile '/m/b.mp4'\n")
            .unwrap();
        let (paths, found) = manifest_entries(&d.path().join("work"));
        assert!(paths.contains(&PathBuf::from("/m/a.mp4")));
        assert!(paths.contains(&PathBuf::from("/m/b.mp4")));
        assert_eq!(found, vec![("b1".to_string(), 2)]);
    }

    #[test]
    fn scratch_is_not_walked_as_an_object() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("u1")).unwrap();
        std::fs::create_dir_all(d.path().join(".scratch/m1")).unwrap();
        std::fs::write(d.path().join("u1/a.mp4"), b"x").unwrap();
        std::fs::write(d.path().join(".scratch/m1/partial.mp4"), b"yyyy").unwrap();
        let found = walk_objects(d.path());
        assert_eq!(found.len(), 1, "only the stored object is an object: {found:?}");
        assert_eq!(found[0].0, "u1/a.mp4");
    }

    #[test]
    fn human_reads_like_the_rest_of_the_product() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(1536), "1.5 KB");
        assert_eq!(human(11_853_776_847), "11.0 GB");
    }
}
