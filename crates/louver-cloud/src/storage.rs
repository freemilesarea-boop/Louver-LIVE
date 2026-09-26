//! Where uploaded video lives, behind a trait.
//!
//! §7 asks that application logic not be tied to one cloud vendor. The trait is
//! the whole of that promise: development uses the filesystem, and an
//! S3-compatible implementation is a second `impl` with no call site to change.
//!
//! One thing the trait must guarantee, because FFmpeg cannot be argued with: a
//! broadcast needs a **local path**. FFmpeg reads a file, and the concat
//! demuxer seeks in it. So an object-store implementation materialises the
//! object locally before a broadcast starts, which is what [`Storage::localize`]
//! is for. On the filesystem it is free.

use crate::Result;
use std::path::{Path, PathBuf};

/// A stored object's key. Opaque to callers; each backend defines its layout.
pub type ObjectKey = String;

pub trait Storage: Send + Sync + std::fmt::Debug {
    /// Take a file that is already on disk and store it. Returns its key.
    fn put_file(&self, user_id: &str, filename: &str, src: &Path) -> Result<ObjectKey>;

    /// A local path FFmpeg can open. May copy; may be free.
    fn localize(&self, key: &ObjectKey) -> Result<PathBuf>;

    fn delete(&self, key: &ObjectKey) -> Result<()>;

    fn size_bytes(&self, key: &ObjectKey) -> Result<u64>;

    /// Where this backend wants prepared output written before it is stored.
    fn scratch_dir(&self) -> PathBuf;

    fn backend_name(&self) -> &'static str;
}

/// Files under one root, laid out per user.
#[derive(Debug, Clone)]
pub struct LocalStorage {
    root: PathBuf,
}

impl LocalStorage {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn object_path(&self, key: &ObjectKey) -> PathBuf {
        self.root.join(key)
    }

    /// Strip anything from a user-supplied name that could leave its directory.
    ///
    /// The key is built from the user id and a fresh uuid, so the original name
    /// is decoration — but it still reaches the filesystem, and a name of
    /// `../../etc/passwd` must become `etc_passwd` rather than an escape.
    fn safe_name(filename: &str) -> String {
        let base = Path::new(filename).file_name().and_then(|s| s.to_str()).unwrap_or("upload");
        let cleaned: String = base
            .chars()
            .map(|c| if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
            .collect();
        let trimmed = cleaned.trim_matches('.').to_string();
        if trimmed.is_empty() {
            "upload".into()
        } else {
            trimmed
        }
    }
}

impl Storage for LocalStorage {
    fn put_file(&self, user_id: &str, filename: &str, src: &Path) -> Result<ObjectKey> {
        let key = format!("{}/{}-{}", user_id, crate::new_id(), Self::safe_name(filename));
        let dest = self.object_path(&key);
        if let Some(d) = dest.parent() {
            std::fs::create_dir_all(d)?;
        }
        // Rename when it is the same filesystem, copy when it is not. An upload
        // temp file is usually beside the store, so this is usually free.
        if std::fs::rename(src, &dest).is_err() {
            std::fs::copy(src, &dest)?;
            let _ = std::fs::remove_file(src);
        }
        Ok(key)
    }

    fn localize(&self, key: &ObjectKey) -> Result<PathBuf> {
        let p = self.object_path(key);
        if !p.is_file() {
            return Err(crate::CloudError::NotFound("object"));
        }
        Ok(p)
    }

    fn delete(&self, key: &ObjectKey) -> Result<()> {
        // A missing object is the state the caller wanted.
        let _ = std::fs::remove_file(self.object_path(key));
        Ok(())
    }

    fn size_bytes(&self, key: &ObjectKey) -> Result<u64> {
        Ok(std::fs::metadata(self.object_path(key))?.len())
    }

    fn scratch_dir(&self) -> PathBuf {
        self.root.join(".scratch")
    }

    fn backend_name(&self) -> &'static str {
        "local filesystem"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_stored_under_its_owner_and_comes_back() {
        let d = tempfile::tempdir().unwrap();
        let s = LocalStorage::new(d.path().join("media"));
        let src = d.path().join("in.mp4");
        std::fs::write(&src, b"video bytes").unwrap();

        let key = s.put_file("user-1", "clip.mp4", &src).unwrap();
        assert!(key.starts_with("user-1/"), "an object must be filed under its owner: {key}");
        assert!(key.ends_with("clip.mp4"));
        assert_eq!(s.size_bytes(&key).unwrap(), 11);
        assert_eq!(std::fs::read(s.localize(&key).unwrap()).unwrap(), b"video bytes");
    }

    #[test]
    fn a_filename_cannot_climb_out_of_the_store() {
        let d = tempfile::tempdir().unwrap();
        let s = LocalStorage::new(d.path().join("media"));
        let src = d.path().join("in.mp4");
        std::fs::write(&src, b"x").unwrap();

        let key = s.put_file("user-1", "../../../etc/passwd", &src).unwrap();
        let path = s.localize(&key).unwrap().canonicalize().unwrap();
        let root = d.path().join("media").canonicalize().unwrap();
        assert!(path.starts_with(&root), "{path:?} escaped {root:?}");
    }

    #[test]
    fn two_uploads_of_one_name_do_not_collide() {
        let d = tempfile::tempdir().unwrap();
        let s = LocalStorage::new(d.path().join("media"));
        let mut keys = std::collections::HashSet::new();
        for _ in 0..2 {
            let src = d.path().join("in.mp4");
            std::fs::write(&src, b"x").unwrap();
            keys.insert(s.put_file("user-1", "same.mp4", &src).unwrap());
        }
        assert_eq!(keys.len(), 2, "the second upload overwrote the first");
    }

    #[test]
    fn deleting_something_absent_is_not_an_error() {
        let d = tempfile::tempdir().unwrap();
        let s = LocalStorage::new(d.path().join("media"));
        assert!(s.delete(&"user-1/nothing".to_string()).is_ok());
        assert!(s.localize(&"user-1/nothing".to_string()).is_err());
    }
}
