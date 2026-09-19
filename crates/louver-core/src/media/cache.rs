//! Normalized-media cache (§8, §10).
//!
//! The user's original file is never touched. Every normalized copy lives under
//! `<app-data>/cache/<media_hash>/normalized.mp4` with a sidecar
//! `metadata.json` describing what produced it.

use crate::config::OutputProfile;
use crate::error::{ErrorCode, LouverError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Identity of a source file: path, size, mtime and a prefix of the content (§8).
///
/// Hashing the whole file would mean reading gigabytes on every launch, so we
/// hash the head and tail. Combined with size and mtime that is enough to notice
/// a file the user replaced.
pub fn media_hash(path: &Path) -> Result<String> {
    let meta = fs::metadata(path)
        .map_err(|_| LouverError::with_detail(ErrorCode::MediaFileMissing, path.display().to_string()))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut h = Sha256::new();
    h.update(path.to_string_lossy().as_bytes());
    h.update(meta.len().to_le_bytes());
    h.update(mtime.to_le_bytes());

    const CHUNK: usize = 256 * 1024;
    let mut f = fs::File::open(path)?;
    let mut buf = vec![0u8; CHUNK];
    let n = f.read(&mut buf)?;
    h.update(&buf[..n]);
    if meta.len() > CHUNK as u64 * 2 {
        use std::io::{Seek, SeekFrom};
        f.seek(SeekFrom::End(-(CHUNK as i64)))?;
        let n = f.read(&mut buf)?;
        h.update(&buf[..n]);
    }
    Ok(hex::encode(&h.finalize()[..16]))
}

/// Sidecar written next to every cached file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheMetadata {
    pub media_hash: String,
    pub source_path: String,
    pub profile: String,
    /// Duration of the normalized file, in whole frames of the profile.
    pub duration_secs: f64,
    pub created_at: String,
    pub encoder: String,
    pub app_version: String,
}

/// Owns the cache directory and answers "is this already normalized?".
#[derive(Debug, Clone)]
pub struct MediaCache {
    root: PathBuf,
}

impl MediaCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Per-media directory. Includes the profile so 1080p and 720p copies of the
    /// same source can coexist.
    pub fn entry_dir(&self, media_hash: &str, profile: OutputProfile) -> PathBuf {
        self.root.join(format!("{media_hash}-{}", profile.id()))
    }

    pub fn normalized_path(&self, media_hash: &str, profile: OutputProfile) -> PathBuf {
        self.entry_dir(media_hash, profile).join("normalized.mp4")
    }

    pub fn metadata_path(&self, media_hash: &str, profile: OutputProfile) -> PathBuf {
        self.entry_dir(media_hash, profile).join("metadata.json")
    }

    /// A cache hit requires both the media file and a readable, matching sidecar.
    pub fn lookup(&self, media_hash: &str, profile: OutputProfile) -> Option<CacheMetadata> {
        let mp4 = self.normalized_path(media_hash, profile);
        if !mp4.is_file() || fs::metadata(&mp4).map(|m| m.len()).unwrap_or(0) == 0 {
            return None;
        }
        let meta: CacheMetadata =
            serde_json::from_str(&fs::read_to_string(self.metadata_path(media_hash, profile)).ok()?).ok()?;
        (meta.media_hash == media_hash && meta.profile == profile.id()).then_some(meta)
    }

    pub fn write_metadata(&self, meta: &CacheMetadata, profile: OutputProfile) -> Result<()> {
        let dir = self.entry_dir(&meta.media_hash, profile);
        fs::create_dir_all(&dir)?;
        fs::write(self.metadata_path(&meta.media_hash, profile), serde_json::to_vec_pretty(meta)?)?;
        Ok(())
    }

    pub fn prepare_entry(&self, media_hash: &str, profile: OutputProfile) -> Result<PathBuf> {
        let dir = self.entry_dir(media_hash, profile);
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// Drop one cached file.
    pub fn evict(&self, media_hash: &str, profile: OutputProfile) -> Result<()> {
        let dir = self.entry_dir(media_hash, profile);
        if dir.exists() {
            fs::remove_dir_all(dir)?;
        }
        Ok(())
    }

    /// Total bytes held by the cache (§45 "Cache Size").
    pub fn total_size(&self) -> u64 {
        dir_size(&self.root)
    }

    /// Delete everything. The caller warns the user first if any entry is in
    /// use by a playlist (§10).
    pub fn clear_all(&self) -> Result<u64> {
        let freed = self.total_size();
        if self.root.exists() {
            fs::remove_dir_all(&self.root)?;
        }
        fs::create_dir_all(&self.root)?;
        Ok(freed)
    }

    /// Cache entries not referenced by any known hash, for housekeeping.
    pub fn orphan_entries(&self, known: &[String]) -> Vec<PathBuf> {
        let Ok(rd) = fs::read_dir(&self.root) else { return Vec::new() };
        rd.flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .filter(|p| {
                let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                !known.iter().any(|h| name.starts_with(h.as_str()))
            })
            .collect()
    }
}

fn dir_size(p: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(p) else { return 0 };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(_) => e.metadata().map(|m| m.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}

/// Disk-space plan shown before optimization starts (§10).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskEstimate {
    pub files_to_process: usize,
    pub total_duration_secs: f64,
    pub estimated_bytes: u64,
    pub available_bytes: u64,
    /// False when the job must not start.
    pub has_enough_space: bool,
    /// Headroom kept free on top of the estimate.
    pub safety_margin_bytes: u64,
}

/// Keep 2 GB free beyond the estimate so the OS never hits a full disk.
pub const DISK_SAFETY_MARGIN: u64 = 2 * 1024 * 1024 * 1024;

pub fn estimate_disk(durations: &[f64], profile: OutputProfile, available_bytes: u64) -> DiskEstimate {
    let total: f64 = durations.iter().sum();
    let estimated = (total * profile.bytes_per_second() as f64) as u64;
    DiskEstimate {
        files_to_process: durations.len(),
        total_duration_secs: total,
        estimated_bytes: estimated,
        available_bytes,
        has_enough_space: available_bytes >= estimated.saturating_add(DISK_SAFETY_MARGIN),
        safety_margin_bytes: DISK_SAFETY_MARGIN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn file_with(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(bytes).unwrap();
        p
    }

    #[test]
    fn hash_is_stable_for_an_unchanged_file() {
        let d = tempfile::tempdir().unwrap();
        let p = file_with(d.path(), "a.mp4", b"some video bytes");
        assert_eq!(media_hash(&p).unwrap(), media_hash(&p).unwrap());
    }

    #[test]
    fn hash_changes_when_the_content_changes() {
        let d = tempfile::tempdir().unwrap();
        let p = file_with(d.path(), "a.mp4", b"original content here");
        let h1 = media_hash(&p).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100)); // mtime granularity
        file_with(d.path(), "a.mp4", b"totally different content!");
        assert_ne!(h1, media_hash(&p).unwrap(), "a replaced file must invalidate its cache");
    }

    #[test]
    fn hash_differs_between_paths_even_with_identical_bytes() {
        let d = tempfile::tempdir().unwrap();
        let a = file_with(d.path(), "a.mp4", b"same bytes");
        let b = file_with(d.path(), "b.mp4", b"same bytes");
        assert_ne!(media_hash(&a).unwrap(), media_hash(&b).unwrap());
    }

    #[test]
    fn hashing_a_missing_file_reports_media_missing() {
        assert_eq!(
            media_hash(Path::new("/definitely/not/here.mp4")).unwrap_err().code,
            ErrorCode::MediaFileMissing
        );
    }

    #[test]
    fn korean_filenames_hash_fine() {
        let d = tempfile::tempdir().unwrap();
        let p = file_with(d.path(), "오늘 밤 재즈.mp4", b"bytes");
        assert!(!media_hash(&p).unwrap().is_empty());
    }

    #[test]
    fn cache_miss_then_hit() {
        let d = tempfile::tempdir().unwrap();
        let c = MediaCache::new(d.path().join("cache"));
        let prof = OutputProfile::P1080p30;
        assert!(c.lookup("abc", prof).is_none());

        c.prepare_entry("abc", prof).unwrap();
        fs::write(c.normalized_path("abc", prof), b"mp4 data").unwrap();
        // Still a miss without the sidecar.
        assert!(c.lookup("abc", prof).is_none());

        c.write_metadata(
            &CacheMetadata {
                media_hash: "abc".into(),
                source_path: "/v/a.mp4".into(),
                profile: prof.id().into(),
                duration_secs: 12.0,
                created_at: "now".into(),
                encoder: "libx264".into(),
                app_version: "1.0.0".into(),
            },
            prof,
        )
        .unwrap();
        let hit = c.lookup("abc", prof).expect("should hit now");
        assert_eq!(hit.duration_secs, 12.0);
        assert_eq!(hit.encoder, "libx264");
    }

    #[test]
    fn a_cache_entry_for_one_profile_does_not_satisfy_another() {
        let d = tempfile::tempdir().unwrap();
        let c = MediaCache::new(d.path());
        c.prepare_entry("abc", OutputProfile::P1080p30).unwrap();
        fs::write(c.normalized_path("abc", OutputProfile::P1080p30), b"x").unwrap();
        c.write_metadata(
            &CacheMetadata {
                media_hash: "abc".into(),
                source_path: "/a".into(),
                profile: OutputProfile::P1080p30.id().into(),
                duration_secs: 1.0,
                created_at: String::new(),
                encoder: "libx264".into(),
                app_version: "1".into(),
            },
            OutputProfile::P1080p30,
        )
        .unwrap();
        assert!(c.lookup("abc", OutputProfile::P1080p30).is_some());
        assert!(c.lookup("abc", OutputProfile::P720p30).is_none());
    }

    #[test]
    fn a_zero_byte_cache_file_is_treated_as_a_miss() {
        let d = tempfile::tempdir().unwrap();
        let c = MediaCache::new(d.path());
        let prof = OutputProfile::P1080p30;
        c.prepare_entry("abc", prof).unwrap();
        fs::write(c.normalized_path("abc", prof), b"").unwrap(); // interrupted encode
        c.write_metadata(
            &CacheMetadata {
                media_hash: "abc".into(),
                source_path: "/a".into(),
                profile: prof.id().into(),
                duration_secs: 1.0,
                created_at: String::new(),
                encoder: "x".into(),
                app_version: "1".into(),
            },
            prof,
        )
        .unwrap();
        assert!(c.lookup("abc", prof).is_none(), "a truncated encode must be redone");
    }

    #[test]
    fn evict_and_clear_free_space() {
        let d = tempfile::tempdir().unwrap();
        let c = MediaCache::new(d.path().join("cache"));
        let prof = OutputProfile::P1080p30;
        for h in ["a", "b"] {
            c.prepare_entry(h, prof).unwrap();
            fs::write(c.normalized_path(h, prof), vec![0u8; 1000]).unwrap();
        }
        assert!(c.total_size() >= 2000);
        c.evict("a", prof).unwrap();
        assert!(c.lookup("a", prof).is_none());
        let freed = c.clear_all().unwrap();
        assert!(freed >= 1000);
        assert_eq!(c.total_size(), 0);
        assert!(c.root().exists(), "the cache root must survive a clear");
    }

    #[test]
    fn orphan_entries_are_found() {
        let d = tempfile::tempdir().unwrap();
        let c = MediaCache::new(d.path().join("cache"));
        let prof = OutputProfile::P1080p30;
        c.prepare_entry("keep", prof).unwrap();
        c.prepare_entry("drop", prof).unwrap();
        let orphans = c.orphan_entries(&["keep".to_string()]);
        assert_eq!(orphans.len(), 1);
        assert!(orphans[0].to_string_lossy().contains("drop"));
    }

    // --- disk estimation (§10) --------------------------------------------

    #[test]
    fn estimate_blocks_the_job_when_space_is_short() {
        // Ten one-hour videos at 1080p30 ≈ 45.9 GB
        let durs = vec![3600.0; 10];
        let e = estimate_disk(&durs, OutputProfile::P1080p30, 10 * 1_000_000_000);
        assert_eq!(e.files_to_process, 10);
        assert!(e.estimated_bytes > 40_000_000_000);
        assert!(!e.has_enough_space, "10 GB free must not be enough for ~46 GB");
    }

    #[test]
    fn estimate_allows_the_job_with_room_to_spare() {
        let e = estimate_disk(&[3600.0], OutputProfile::P1080p30, 200 * 1_000_000_000);
        assert!(e.has_enough_space);
        assert!(e.estimated_bytes > 4_000_000_000 && e.estimated_bytes < 5_500_000_000);
    }

    #[test]
    fn estimate_enforces_the_safety_margin() {
        // Exactly the estimated size, with nothing left over, must be refused.
        let e0 = estimate_disk(&[600.0], OutputProfile::P1080p30, 0);
        let exact = estimate_disk(&[600.0], OutputProfile::P1080p30, e0.estimated_bytes);
        assert!(!exact.has_enough_space);
        let with_margin =
            estimate_disk(&[600.0], OutputProfile::P1080p30, e0.estimated_bytes + DISK_SAFETY_MARGIN);
        assert!(with_margin.has_enough_space);
    }

    #[test]
    fn the_720p_profile_needs_less_space() {
        let a = estimate_disk(&[3600.0], OutputProfile::P1080p30, 0).estimated_bytes;
        let b = estimate_disk(&[3600.0], OutputProfile::P720p30, 0).estimated_bytes;
        assert!(b < a / 2);
    }

    #[test]
    fn empty_job_needs_no_space() {
        let e = estimate_disk(&[], OutputProfile::P1080p30, 0);
        assert_eq!(e.estimated_bytes, 0);
        assert_eq!(e.files_to_process, 0);
    }
}
