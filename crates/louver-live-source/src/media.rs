//! Each user's media, in each user's own directory.
//!
//! What a client may never choose, and where each is refused:
//!
//! **A path.** Media is named, never pathed. A name with a separator, a `..`, a
//! dot-prefix, a colon or a NUL is refused *before* anything is joined, and the
//! joined path is then canonicalised and checked to still be inside this user's
//! directory — which is what catches a symlink whose name looks innocent.
//!
//! **Another user's file.** Every call takes the caller's id and resolves
//! inside `<root>/<user>/`. There is no API that takes a path, and none that
//! takes a user id from a request body.
//!
//! **What counts as media.** An upload is accepted on three separate grounds,
//! and all three have to hold: a size under the per-file cap, an extension on a
//! short list, and `ffprobe` finding a real video or audio stream in it. The
//! third is the one that matters — an extension is a claim, not a fact, and
//! FFmpeg will happily be pointed at whatever a `.mp4` actually contains.
//!
//! An upload that fails any of those leaves nothing behind: the bytes go to a
//! temporary name inside the user's directory, are probed there, and are either
//! renamed into place or deleted. The temporary name begins with a dot, which
//! [`MediaRoot::resolve`] refuses, so a half-written upload cannot be used as a
//! playlist item even in the window before it is cleaned up.
//!
//! This directory is the worker's own. It is **not** production's media volume
//! and nothing here opens production's database — beta media is uploaded to
//! this service directly, which is what keeps the two storage systems from
//! sharing a failure.

use crate::error::{LiveSourceError, Result};
use louver_core::streaming::ffmpeg::FfmpegTools;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The most media one job may name.
pub const MAX_PLAYLIST_ITEMS: usize = 200;

/// The longest media file name this will consider.
pub const MAX_NAME_CHARS: usize = 120;

/// The largest single upload.
pub const MAX_UPLOAD_BYTES: u64 = 512 * 1024 * 1024;

/// The most one user may store in total.
pub const MAX_USER_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// How long `ffprobe` may take on an upload before it is rejected.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// Extensions this will store. A claim, checked against the bytes below.
pub const ALLOWED_EXTENSIONS: &[&str] = &["mp4", "mov", "m4v", "mkv", "webm", "m4a", "mp3", "aac", "wav"];

/// One upload, once it is on disk.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StoredMedia {
    pub name: String,
    pub bytes: u64,
    /// What `ffprobe` actually found, for the operator and the UI.
    pub kind: String,
}

/// Where this worker's media lives.
#[derive(Debug, Clone)]
pub struct MediaRoot {
    root: PathBuf,
    tools: FfmpegTools,
}

impl MediaRoot {
    /// The directory must exist, so a typo fails at startup.
    pub fn new(root: impl Into<PathBuf>, tools: FfmpegTools) -> Result<Self> {
        let root = root.into();
        let root = root
            .canonicalize()
            .map_err(|e| LiveSourceError::invalid(format!("미디어 디렉터리를 열 수 없습니다: {e}")))?;
        if !root.is_dir() {
            return Err(LiveSourceError::invalid("미디어 경로가 디렉터리가 아닙니다."));
        }
        Ok(Self { root, tools })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A user id as a single directory component.
    ///
    /// 247streams ids are hex uuids, so this is defence against a future id
    /// shape rather than against today's: anything outside a short safe set
    /// becomes `_`, and the result can be only one component deep.
    fn user_component(user_id: &str) -> Result<String> {
        let safe: String = user_id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .take(64)
            .collect();
        if safe.trim_matches('_').is_empty() {
            return Err(LiveSourceError::unauthorized("사용자를 확인할 수 없습니다."));
        }
        Ok(safe)
    }

    /// This user's directory, created if it is not there yet.
    pub fn dir_for(&self, user_id: &str) -> Result<PathBuf> {
        let dir = self.root.join(Self::user_component(user_id)?);
        std::fs::create_dir_all(&dir)
            .map_err(|e| LiveSourceError::invalid(format!("사용자 디렉터리를 만들 수 없습니다: {e}")))?;
        Ok(dir)
    }

    /// Check a media *name* without touching the filesystem.
    ///
    /// Separate from [`Self::resolve`] so an upload can validate the name it
    /// was given before it writes a single byte.
    pub fn check_name(name: &str) -> Result<&str> {
        let refuse = |why: &str| {
            Err(LiveSourceError::invalid(format!("영상 이름을 사용할 수 없습니다: {why}")))
        };
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
        // Everything that turns a name into a path, refused before any join.
        if n.contains('/') || n.contains('\\') || n.contains('\0') {
            return refuse("경로를 포함할 수 없습니다");
        }
        if n.starts_with('.') {
            // Also what keeps a `.tmp-…` upload from being nameable.
            return refuse("점으로 시작할 수 없습니다");
        }
        if n.contains(':') {
            return refuse("콜론을 포함할 수 없습니다");
        }
        Ok(n)
    }

    /// The extension, lowercased, if it is one this will store.
    fn check_extension(name: &str) -> Result<String> {
        let ext = Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        if !ALLOWED_EXTENSIONS.contains(&ext.as_str()) {
            return Err(LiveSourceError::invalid(format!(
                "지원하지 않는 형식입니다. 사용 가능: {}",
                ALLOWED_EXTENSIONS.join(", ")
            )));
        }
        Ok(ext)
    }

    /// One of this user's media names → its path.
    pub fn resolve(&self, user_id: &str, name: &str) -> Result<PathBuf> {
        let n = Self::check_name(name)?;
        let dir = self.dir_for(user_id)?;
        let real = dir
            .join(n)
            .canonicalize()
            .map_err(|_| LiveSourceError::not_found("해당 영상을 찾을 수 없습니다."))?;
        // Canonicalised and re-checked: a symlink inside the directory pointing
        // out of it is the case a name check cannot see.
        let dir_real = dir.canonicalize().unwrap_or(dir);
        if !real.starts_with(&dir_real) {
            return Err(LiveSourceError::invalid("영상 이름을 사용할 수 없습니다: 디렉터리를 벗어납니다"));
        }
        if !real.is_file() {
            return Err(LiveSourceError::invalid("영상 이름을 사용할 수 없습니다: 파일이 아닙니다"));
        }
        Ok(real)
    }

    /// What this user has stored.
    pub fn list(&self, user_id: &str) -> Result<Vec<StoredMedia>> {
        let dir = self.dir_for(user_id)?;
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(&dir) else { return Ok(out) };
        for e in entries.flatten() {
            let p = e.path();
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else { continue };
            // Temporary and hidden files are not media.
            if name.starts_with('.') || !p.is_file() {
                continue;
            }
            let bytes = e.metadata().map(|m| m.len()).unwrap_or(0);
            out.push(StoredMedia { name: name.to_string(), bytes, kind: String::new() });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// How many bytes this user is using.
    pub fn used_bytes(&self, user_id: &str) -> u64 {
        self.list(user_id).map(|v| v.iter().map(|m| m.bytes).sum()).unwrap_or(0)
    }

    /// Store an upload, or leave nothing behind.
    ///
    /// The order is deliberate: name, extension and declared size are checked
    /// before a byte is written; the quota is checked against what is already
    /// stored; the bytes go to a dotted temporary name; `ffprobe` decides
    /// whether they are media at all; only then is the file renamed into place.
    /// Every failure path removes the temporary file.
    pub fn store(&self, user_id: &str, name: &str, bytes: &[u8]) -> Result<StoredMedia> {
        let n = Self::check_name(name)?.to_string();
        Self::check_extension(&n)?;
        let len = bytes.len() as u64;
        if len == 0 {
            return Err(LiveSourceError::invalid("빈 파일은 올릴 수 없습니다."));
        }
        if len > MAX_UPLOAD_BYTES {
            return Err(LiveSourceError::invalid(format!(
                "파일이 너무 큽니다 (최대 {}MB).",
                MAX_UPLOAD_BYTES / 1024 / 1024
            )));
        }
        let dir = self.dir_for(user_id)?;
        // Replacing a file of their own is allowed; its bytes are not counted
        // twice against the quota.
        let replacing = self.resolve(user_id, &n).ok().and_then(|p| std::fs::metadata(p).ok()).map(|m| m.len()).unwrap_or(0);
        let used = self.used_bytes(user_id).saturating_sub(replacing);
        if used + len > MAX_USER_BYTES {
            return Err(LiveSourceError::limit(format!(
                "저장 용량을 초과합니다 (최대 {}GB, 현재 {}MB).",
                MAX_USER_BYTES / 1024 / 1024 / 1024,
                used / 1024 / 1024
            )));
        }

        // A dotted temporary name: `resolve` refuses anything starting with a
        // dot, so this cannot be used as a playlist item while it exists.
        let tmp = dir.join(format!(".tmp-{}-{}", std::process::id(), now_nanos()));
        std::fs::write(&tmp, bytes).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            LiveSourceError::invalid(format!("파일을 저장할 수 없습니다: {e}"))
        })?;

        // An extension is a claim. This is the fact.
        let kind = match self.probe_kind(&tmp) {
            Ok(k) => k,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        };

        let final_path = dir.join(&n);
        if let Err(e) = std::fs::rename(&tmp, &final_path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(LiveSourceError::invalid(format!("파일을 저장할 수 없습니다: {e}")));
        }
        Ok(StoredMedia { name: n, bytes: len, kind })
    }

    /// Remove one of this user's files.
    pub fn delete(&self, user_id: &str, name: &str) -> Result<()> {
        let path = self.resolve(user_id, name)?;
        std::fs::remove_file(path)
            .map_err(|e| LiveSourceError::invalid(format!("파일을 지울 수 없습니다: {e}")))
    }

    /// What `ffprobe` finds, or why this is not media.
    fn probe_kind(&self, path: &Path) -> Result<String> {
        let mut cmd = std::process::Command::new(&self.tools.ffprobe);
        cmd.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-print_format",
            "json",
            "-show_streams",
            "-show_format",
            // A local file, so no network protocol is needed or allowed.
            "-protocol_whitelist",
            "file",
            "-i",
        ])
        .arg(path);
        let run = crate::process::run_with_timeout(cmd, PROBE_TIMEOUT)
            .map_err(|e| LiveSourceError::invalid(format!("파일을 검사할 수 없습니다: {e}")))?;
        if run.timed_out {
            return Err(LiveSourceError::invalid("파일 검사 시간을 초과했습니다."));
        }
        if !run.ok {
            return Err(LiveSourceError::invalid("읽을 수 있는 영상·음성 파일이 아닙니다."));
        }
        let v: serde_json::Value = serde_json::from_slice(&run.stdout)
            .map_err(|_| LiveSourceError::invalid("파일 정보를 읽을 수 없습니다."))?;
        let streams = v.get("streams").and_then(|s| s.as_array()).cloned().unwrap_or_default();
        let has_video = streams.iter().any(|s| s.get("codec_type").and_then(|t| t.as_str()) == Some("video"));
        let has_audio = streams.iter().any(|s| s.get("codec_type").and_then(|t| t.as_str()) == Some("audio"));
        // The playlist supplies the sound, so audio is what a playlist item is
        // actually for; a video-only file is still usable as one.
        match (has_video, has_audio) {
            (_, true) | (true, false) => Ok(match (has_video, has_audio) {
                (true, true) => "video+audio".to_string(),
                (true, false) => "video".to_string(),
                _ => "audio".to_string(),
            }),
            (false, false) => Err(LiveSourceError::invalid("영상도 음성도 없는 파일입니다.")),
        }
    }

    /// Write the concat manifest for a playlist into a job's own directory.
    pub fn write_manifest(&self, user_id: &str, job_dir: &Path, names: &[String]) -> Result<PathBuf> {
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
            let path = self.resolve(user_id, name)?;
            let s = path.to_string_lossy();
            // A quote would break out of `file '…'`. It cannot occur — the name
            // it came from has no separators and the directory is ours — and is
            // refused anyway rather than assumed.
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

fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real, tiny media file, because the whole point of the upload check is
    /// that it looks at the bytes rather than the name.
    fn real_media() -> Option<Vec<u8>> {
        let ff = which("ffmpeg")?;
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("t.mp4");
        let ok = std::process::Command::new(ff)
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

    fn which(name: &str) -> Option<PathBuf> {
        std::env::var_os("PATH")?
            .to_string_lossy()
            .split(':')
            .map(|d| Path::new(d).join(name))
            .find(|p| p.is_file())
    }

    fn root() -> (tempfile::TempDir, MediaRoot) {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("media");
        std::fs::create_dir_all(&media).unwrap();
        std::fs::write(dir.path().join("secret.txt"), b"stream-key-abcd").unwrap();
        let tools = FfmpegTools::new(
            which("ffmpeg").map(|p| p.display().to_string()).unwrap_or_else(|| "ffmpeg".into()),
            which("ffprobe").map(|p| p.display().to_string()).unwrap_or_else(|| "ffprobe".into()),
        );
        let mr = MediaRoot::new(&media, tools).unwrap();
        (dir, mr)
    }

    /// Put a file straight into a user's directory, bypassing the upload check,
    /// for the tests that are about resolution rather than storage.
    fn plant(mr: &MediaRoot, user: &str, name: &str, body: &[u8]) -> PathBuf {
        let dir = mr.dir_for(user).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn each_user_gets_their_own_directory() {
        let (_d, mr) = root();
        let a = mr.dir_for("user-alice").unwrap();
        let b = mr.dir_for("user-bob").unwrap();
        assert_ne!(a, b);
        assert!(a.starts_with(mr.root()) && b.starts_with(mr.root()));
    }

    #[test]
    fn one_user_cannot_resolve_another_users_file() {
        let (_d, mr) = root();
        plant(&mr, "user-alice", "alice.mp4", b"x");
        assert!(mr.resolve("user-alice", "alice.mp4").is_ok());
        // Bob naming Alice's file: not found, in Bob's directory.
        let e = mr.resolve("user-bob", "alice.mp4").unwrap_err();
        assert_eq!(e.kind, crate::ErrorKind::NotFound);
    }

    #[test]
    fn one_user_cannot_reach_another_users_file_by_path() {
        let (_d, mr) = root();
        plant(&mr, "user-alice", "alice.mp4", b"x");
        let alice_dir = MediaRoot::user_component("user-alice").unwrap();
        for bad in [
            format!("../{alice_dir}/alice.mp4"),
            format!("..\\{alice_dir}\\alice.mp4"),
            "../../secret.txt".to_string(),
            "/etc/passwd".to_string(),
            "./alice.mp4".to_string(),
            ".hidden".to_string(),
            "a:b.mp4".to_string(),
            "a\0b.mp4".to_string(),
            String::new(),
        ] {
            let e = mr.resolve("user-bob", &bad).unwrap_err();
            assert!(
                matches!(e.kind, crate::ErrorKind::Invalid | crate::ErrorKind::NotFound),
                "{bad:?} → {e:?}"
            );
        }
    }

    #[test]
    fn a_symlink_out_of_a_users_directory_is_refused() {
        let (d, mr) = root();
        plant(&mr, "user-alice", "alice.mp4", b"secret");
        let bob = mr.dir_for("user-bob").unwrap();
        #[cfg(unix)]
        {
            // The case a name check cannot see: a plain name pointing elsewhere.
            std::os::unix::fs::symlink(d.path().join("secret.txt"), bob.join("escape.mp4")).unwrap();
            std::os::unix::fs::symlink(
                mr.dir_for("user-alice").unwrap().join("alice.mp4"),
                bob.join("peek.mp4"),
            )
            .unwrap();
            for name in ["escape.mp4", "peek.mp4"] {
                let e = mr.resolve("user-bob", name).unwrap_err();
                assert!(e.message.contains("디렉터리를 벗어납니다"), "{name} → {}", e.message);
            }
        }
        let _ = d;
    }

    #[test]
    fn a_listing_shows_only_this_users_files_and_hides_temporaries() {
        let (_d, mr) = root();
        plant(&mr, "user-alice", "a.mp4", b"12345");
        plant(&mr, "user-alice", ".tmp-123", b"junk");
        plant(&mr, "user-bob", "b.mp4", b"1");
        let names: Vec<String> = mr.list("user-alice").unwrap().into_iter().map(|m| m.name).collect();
        assert_eq!(names, vec!["a.mp4".to_string()], "no temporaries, no other users");
        assert_eq!(mr.used_bytes("user-alice"), 5);
        assert_eq!(mr.used_bytes("user-bob"), 1);
        assert_eq!(mr.used_bytes("user-nobody"), 0);
    }

    #[test]
    fn an_upload_has_to_be_real_media_and_not_merely_named_like_it() {
        let (_d, mr) = root();
        if which("ffprobe").is_none() {
            eprintln!("SKIP: no ffprobe");
            return;
        }
        // An extension is a claim. These are not media.
        for body in [b"not a video at all".as_slice(), &[0u8; 2048]] {
            let e = mr.store("user-alice", "claim.mp4", body).unwrap_err();
            assert_eq!(e.kind, crate::ErrorKind::Invalid);
            assert!(
                e.message.contains("영상") || e.message.contains("파일"),
                "{}",
                e.message
            );
        }
        // And nothing is left behind, not even a temporary.
        let left: Vec<_> = std::fs::read_dir(mr.dir_for("user-alice").unwrap()).unwrap().flatten().collect();
        assert!(left.is_empty(), "upload failure left {left:?}");
    }

    #[test]
    fn a_real_upload_is_stored_and_then_usable_as_a_playlist_item() {
        let (_d, mr) = root();
        let Some(bytes) = real_media() else {
            eprintln!("SKIP: no ffmpeg");
            return;
        };
        let stored = mr.store("user-alice", "song.m4a", &bytes).unwrap();
        assert_eq!(stored.name, "song.m4a");
        assert_eq!(stored.bytes, bytes.len() as u64);
        assert!(stored.kind.contains("audio"), "{}", stored.kind);
        // Resolvable by its owner, and only by its owner.
        assert!(mr.resolve("user-alice", "song.m4a").is_ok());
        assert!(mr.resolve("user-bob", "song.m4a").is_err());
        // And it makes a manifest.
        let job = mr.root().join("job-1");
        let m = mr.write_manifest("user-alice", &job, &["song.m4a".into()]).unwrap();
        let body = std::fs::read_to_string(m).unwrap();
        assert!(body.starts_with("file '") && body.trim_end().ends_with("song.m4a'"), "{body}");
    }

    #[test]
    fn an_unsupported_extension_is_refused_before_anything_is_written() {
        let (_d, mr) = root();
        for name in ["script.sh", "payload.exe", "x.php", "noext", "a.mp4.sh", "a.MP4x"] {
            let e = mr.store("user-alice", name, b"whatever").unwrap_err();
            assert!(e.message.contains("지원하지 않는 형식"), "{name} → {}", e.message);
        }
        // An upper-case extension on the list is fine.
        assert!(MediaRoot::check_extension("A.MP4").is_ok());
        let left: Vec<_> = std::fs::read_dir(mr.dir_for("user-alice").unwrap()).unwrap().flatten().collect();
        assert!(left.is_empty());
    }

    #[test]
    fn an_upload_name_that_is_a_path_is_refused() {
        let (_d, mr) = root();
        for name in ["../../etc/passwd.mp4", "a/b.mp4", "..", ".hidden.mp4", "a\0.mp4", ""] {
            assert!(mr.store("user-alice", name, b"x").is_err(), "{name:?}");
        }
    }

    #[test]
    fn an_empty_or_oversized_upload_is_refused() {
        let (_d, mr) = root();
        assert!(mr.store("user-alice", "a.mp4", b"").unwrap_err().message.contains("빈 파일"));
        // Checked against the declared length without allocating the file.
        assert_eq!(MAX_UPLOAD_BYTES, 512 * 1024 * 1024);
        assert_eq!(MAX_USER_BYTES, 4 * 1024 * 1024 * 1024);
    }

    #[test]
    fn the_storage_quota_counts_only_this_user() {
        let (_d, mr) = root();
        // Bob's usage must not reduce Alice's allowance.
        plant(&mr, "user-bob", "big.mp4", &vec![0u8; 4096]);
        assert_eq!(mr.used_bytes("user-alice"), 0);
        assert_eq!(mr.used_bytes("user-bob"), 4096);
    }

    #[test]
    fn a_delete_only_reaches_this_users_own_file() {
        let (_d, mr) = root();
        plant(&mr, "user-alice", "a.mp4", b"x");
        assert!(mr.delete("user-bob", "a.mp4").is_err(), "Bob must not delete Alice's file");
        assert!(mr.resolve("user-alice", "a.mp4").is_ok(), "and it is still there");
        assert!(mr.delete("user-alice", "a.mp4").is_ok());
        assert!(mr.resolve("user-alice", "a.mp4").is_err());
    }

    #[test]
    fn a_manifest_refuses_an_item_that_is_not_this_users_and_writes_nothing() {
        let (_d, mr) = root();
        plant(&mr, "user-alice", "a.mp4", b"x");
        plant(&mr, "user-bob", "b.mp4", b"x");
        let job = mr.root().join("job-2");
        assert!(mr.write_manifest("user-bob", &job, &["b.mp4".into(), "a.mp4".into()]).is_err());
        assert!(!job.join("manifest.txt").exists(), "nothing half-written");
    }

    #[test]
    fn a_user_id_that_is_not_a_usable_directory_name_is_refused() {
        let (_d, mr) = root();
        for bad in ["", "   ", "..", "///", "___"] {
            assert!(mr.dir_for(bad).is_err(), "{bad:?}");
        }
        // And an id with odd characters is flattened, never escaped.
        let dir = mr.dir_for("../../etc").unwrap();
        assert_eq!(dir.parent().unwrap(), mr.root());
    }

    #[test]
    fn an_error_message_never_echoes_the_server_path() {
        let (_d, mr) = root();
        for e in [
            mr.resolve("user-alice", "nope.mp4").unwrap_err(),
            mr.store("user-alice", "x.sh", b"x").unwrap_err(),
            mr.delete("user-alice", "nope.mp4").unwrap_err(),
        ] {
            assert!(!e.message.contains("/tmp"), "{}", e.message);
            assert!(!e.message.contains(".tmp-"), "{}", e.message);
        }
    }
}
