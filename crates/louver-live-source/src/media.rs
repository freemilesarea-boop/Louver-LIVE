//! Turning a request's playlist into a concat manifest, safely.
//!
//! Two things a client must never be able to choose, and this module is where
//! both are refused:
//!
//! **A file path.** The request names media by file **name** only, and the name
//! is resolved inside one configured directory. A name containing a separator,
//! a `..`, a NUL, a drive letter or a leading `/` is refused before it is
//! joined to anything, and the joined path is then canonicalised and checked to
//! still be under the root. Without that, `../../../etc/passwd` or
//! `/var/lib/louver/cloud.db` would become an FFmpeg input, and FFmpeg would
//! happily read either.
//!
//! **A destination.** The RTMP(S) URL carries the stream key, so it is not in
//! any request body. The client names a destination and this module looks the
//! URL up in configuration the operator wrote. A client that could post a
//! destination could point someone else's picture and music at its own ingest,
//! or read a stream key back out of an error message.
//!
//! The manifest itself is written with the concat demuxer's quoting rules, and
//! a name that would need escaping has already been refused — so there is
//! nothing to escape.

use crate::error::{LiveSourceError, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The most media one job may name. A playlist is music, not a filesystem dump.
pub const MAX_PLAYLIST_ITEMS: usize = 200;

/// The longest media file name this will consider.
pub const MAX_NAME_CHARS: usize = 200;

/// Where media lives and where destinations are defined.
#[derive(Debug, Clone)]
pub struct MediaRoot {
    root: PathBuf,
    /// name → RTMP(S) URL. From configuration only.
    destinations: BTreeMap<String, String>,
}

impl MediaRoot {
    /// The directory must exist, so a typo fails at startup rather than on the
    /// first request.
    pub fn new(root: impl Into<PathBuf>, destinations: BTreeMap<String, String>) -> Result<Self> {
        let root = root.into();
        let root = root
            .canonicalize()
            .map_err(|e| LiveSourceError::invalid(format!("미디어 디렉터리를 열 수 없습니다: {e}")))?;
        if !root.is_dir() {
            return Err(LiveSourceError::invalid("미디어 경로가 디렉터리가 아닙니다."));
        }
        Ok(Self { root, destinations })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The names an operator has defined. Never the URLs.
    pub fn destination_names(&self) -> Vec<String> {
        self.destinations.keys().cloned().collect()
    }

    /// A destination's URL, by the name a client asked for.
    ///
    /// The returned string is a credential. It goes into the worker's config
    /// and nowhere else — not into a response, not into a log, not into the
    /// state file.
    pub fn destination(&self, name: &str) -> Result<String> {
        self.destinations
            .get(name)
            .cloned()
            // The message names what was asked for, not what exists, so this is
            // not an oracle for guessing other operators' destination names.
            .ok_or_else(|| LiveSourceError::invalid("등록되지 않은 송출 대상입니다."))
    }

    /// One media file name → its path inside the root.
    pub fn resolve(&self, name: &str) -> Result<PathBuf> {
        let refuse =
            |why: &str| Err(LiveSourceError::invalid(format!("영상 이름을 사용할 수 없습니다: {why}")));
        let n = name.trim();
        if n.is_empty() {
            return refuse("비어 있습니다");
        }
        if n.chars().count() > MAX_NAME_CHARS {
            return refuse("너무 깁니다");
        }
        if n.chars().any(|c| c.is_control()) {
            return refuse("제어문자가 있습니다");
        }
        // Everything that makes a name into a path. Checked before any join, so
        // there is no intermediate path to get wrong.
        if n.contains('/') || n.contains('\\') || n.contains('\0') {
            return refuse("경로를 포함할 수 없습니다");
        }
        if n == "." || n == ".." || n.starts_with('.') {
            return refuse("점으로 시작할 수 없습니다");
        }
        // `C:` and `\\?\` shapes, which are paths on Windows.
        if n.contains(':') {
            return refuse("콜론을 포함할 수 없습니다");
        }
        let joined = self.root.join(n);
        // Canonicalise and check again: a symlink inside the root pointing out
        // of it is the case the name check cannot see.
        let real =
            joined.canonicalize().map_err(|_| LiveSourceError::invalid("해당 영상을 찾을 수 없습니다."))?;
        if !real.starts_with(&self.root) {
            return refuse("디렉터리를 벗어납니다");
        }
        if !real.is_file() {
            return refuse("파일이 아닙니다");
        }
        Ok(real)
    }

    /// Write the concat manifest for a playlist, returning its path.
    ///
    /// The manifest is written into the job's own directory, which the caller
    /// owns, so two jobs never share one.
    pub fn write_manifest(&self, job_dir: &Path, names: &[String]) -> Result<PathBuf> {
        if names.is_empty() {
            return Err(LiveSourceError::invalid("음악이 될 영상을 최소 한 개 선택해 주세요."));
        }
        if names.len() > MAX_PLAYLIST_ITEMS {
            return Err(LiveSourceError::invalid(format!(
                "재생목록은 {MAX_PLAYLIST_ITEMS}개까지 넣을 수 있습니다."
            )));
        }
        let mut body = String::new();
        for name in names {
            let path = self.resolve(name)?;
            // The concat demuxer takes `file '<path>'`. A path containing a
            // quote would break out of it — and cannot occur, because the name
            // it came from has no separators and lives under a root the
            // operator chose. Refused anyway rather than assumed.
            let s = path.to_string_lossy();
            if s.contains('\'') || s.contains('\n') {
                return Err(LiveSourceError::invalid("영상 경로에 사용할 수 없는 문자가 있습니다."));
            }
            body.push_str(&format!("file '{s}'\n"));
        }
        std::fs::create_dir_all(job_dir)
            .map_err(|e| LiveSourceError::invalid(format!("작업 디렉터리를 만들 수 없습니다: {e}")))?;
        let manifest = job_dir.join("manifest.txt");
        std::fs::write(&manifest, body)
            .map_err(|e| LiveSourceError::invalid(format!("재생목록을 쓸 수 없습니다: {e}")))?;
        Ok(manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> (tempfile::TempDir, MediaRoot) {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("media");
        std::fs::create_dir_all(&media).unwrap();
        for n in ["a.mp4", "b.mp4"] {
            std::fs::write(media.join(n), b"not really a video").unwrap();
        }
        std::fs::create_dir_all(media.join("sub")).unwrap();
        std::fs::write(media.join("sub").join("c.mp4"), b"x").unwrap();
        // A secret outside the root, which is what the traversal tests aim at.
        std::fs::write(dir.path().join("secret.txt"), b"stream-key-abcd").unwrap();
        let mut d = BTreeMap::new();
        d.insert("test-sink".to_string(), "rtmp://127.0.0.1:1935/live/test".to_string());
        let mr = MediaRoot::new(&media, d).unwrap();
        (dir, mr)
    }

    #[test]
    fn a_plain_name_resolves_inside_the_root() {
        let (_d, mr) = root();
        let p = mr.resolve("a.mp4").unwrap();
        assert!(p.starts_with(mr.root()));
        assert!(p.ends_with("a.mp4"));
    }

    #[test]
    fn nothing_that_looks_like_a_path_is_accepted() {
        let (_d, mr) = root();
        for bad in [
            "../secret.txt",
            "../../etc/passwd",
            "/etc/passwd",
            "sub/c.mp4",
            "sub\\c.mp4",
            "./a.mp4",
            ".",
            "..",
            "C:\\windows\\system32",
            "a.mp4\0.txt",
            "a\nb.mp4",
            "",
            "   ",
        ] {
            let e = mr.resolve(bad).unwrap_err();
            assert_eq!(e.kind, crate::ErrorKind::Invalid, "{bad:?} was accepted");
        }
    }

    #[test]
    fn a_symlink_out_of_the_root_is_refused_even_though_its_name_is_plain() {
        let (d, mr) = root();
        // The case a name check alone cannot see: the name is `escape.mp4`, with
        // no separators, but it points outside.
        let link = mr.root().join("escape.mp4");
        #[cfg(unix)]
        std::os::unix::fs::symlink(d.path().join("secret.txt"), &link).unwrap();
        #[cfg(unix)]
        {
            let e = mr.resolve("escape.mp4").unwrap_err();
            assert!(e.message.contains("디렉터리를 벗어납니다"), "{}", e.message);
        }
        let _ = (d, link);
    }

    #[test]
    fn a_directory_is_not_media() {
        let (_d, mr) = root();
        assert!(mr.resolve("sub").is_err());
    }

    #[test]
    fn a_missing_file_says_so_without_revealing_the_root() {
        let (_d, mr) = root();
        let e = mr.resolve("nope.mp4").unwrap_err();
        assert!(e.message.contains("찾을 수 없습니다"), "{}", e.message);
        assert!(!e.message.contains("/tmp"), "the path must not be echoed: {}", e.message);
    }

    #[test]
    fn a_manifest_names_every_item_in_order() {
        let (d, mr) = root();
        let job = d.path().join("job-1");
        let m = mr.write_manifest(&job, &["a.mp4".into(), "b.mp4".into(), "a.mp4".into()]).unwrap();
        let body = std::fs::read_to_string(&m).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("file '") && lines[0].ends_with("a.mp4'"), "{}", lines[0]);
        assert!(lines[1].ends_with("b.mp4'"));
        assert!(lines[2].ends_with("a.mp4'"), "a repeat is allowed");
    }

    #[test]
    fn a_manifest_refuses_an_item_that_escapes_and_writes_nothing() {
        let (d, mr) = root();
        let job = d.path().join("job-2");
        assert!(mr.write_manifest(&job, &["a.mp4".into(), "../secret.txt".into()]).is_err());
        // Nothing half-written: the manifest must not exist with the good half.
        assert!(!job.join("manifest.txt").exists());
    }

    #[test]
    fn an_empty_or_enormous_playlist_is_refused() {
        let (d, mr) = root();
        let job = d.path().join("job-3");
        assert!(mr.write_manifest(&job, &[]).unwrap_err().message.contains("최소 한 개"));
        let many: Vec<String> = (0..MAX_PLAYLIST_ITEMS + 1).map(|_| "a.mp4".to_string()).collect();
        assert!(mr.write_manifest(&job, &many).unwrap_err().message.contains("개까지"));
    }

    #[test]
    fn a_destination_comes_from_configuration_and_never_from_a_request() {
        let (_d, mr) = root();
        assert_eq!(mr.destination("test-sink").unwrap(), "rtmp://127.0.0.1:1935/live/test");
        // A client naming anything else gets a refusal that does not say what
        // does exist.
        let e = mr.destination("rtmp://evil.example/live/steal").unwrap_err();
        assert_eq!(e.message, "등록되지 않은 송출 대상입니다.");
        assert!(mr.destination("").is_err());
    }

    #[test]
    fn the_names_are_listable_but_the_urls_are_not() {
        let (_d, mr) = root();
        let names = mr.destination_names();
        assert_eq!(names, vec!["test-sink".to_string()]);
        // There is no accessor that hands out the whole map.
        assert!(!format!("{names:?}").contains("rtmp://"));
    }

    #[test]
    fn a_media_root_that_does_not_exist_fails_at_construction() {
        assert!(MediaRoot::new("/definitely/not/here", BTreeMap::new()).is_err());
    }
}
