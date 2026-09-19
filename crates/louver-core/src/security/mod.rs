//! Stream key storage (§15).
//!
//! The key never reaches SQLite, JSON, a log file or a crash report. The core
//! defines the trait and a development fallback; the desktop crate supplies the
//! OS keychain implementation (macOS Keychain / Windows Credential Manager).

use crate::error::{ErrorCode, LouverError, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub const SERVICE_NAME: &str = "com.louver.live";
pub const STREAM_KEY_ACCOUNT: &str = "youtube_stream_key";

/// An OS-backed secret store.
pub trait SecretStore: Send + Sync + std::fmt::Debug {
    fn set(&self, account: &str, secret: &str) -> Result<()>;
    fn get(&self, account: &str) -> Result<Option<String>>;
    fn delete(&self, account: &str) -> Result<()>;
    /// Human-readable backing store, shown in Settings.
    fn backend_name(&self) -> &'static str;
    /// False when the OS store is unavailable and secrets are not persisted.
    fn is_secure(&self) -> bool {
        true
    }
}

/// In-process store used by tests and by platforms without a keychain.
///
/// It deliberately does not persist: losing the key on restart is a far smaller
/// problem than writing it to disk in the clear (§60).
#[derive(Debug, Default)]
pub struct MemorySecretStore {
    map: Arc<Mutex<HashMap<String, String>>>,
}

impl MemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for MemorySecretStore {
    fn set(&self, account: &str, secret: &str) -> Result<()> {
        self.map.lock().unwrap().insert(account.into(), secret.into());
        Ok(())
    }
    fn get(&self, account: &str) -> Result<Option<String>> {
        Ok(self.map.lock().unwrap().get(account).cloned())
    }
    fn delete(&self, account: &str) -> Result<()> {
        self.map.lock().unwrap().remove(account);
        Ok(())
    }
    fn backend_name(&self) -> &'static str {
        "메모리 (재시작 시 삭제됨)"
    }
    fn is_secure(&self) -> bool {
        false
    }
}

/// How the UI shows a stored key (§15).
pub fn masked_display(secret: Option<&str>) -> String {
    match secret {
        Some(s) if !s.is_empty() => "•".repeat(s.chars().count().clamp(8, 24)),
        _ => String::new(),
    }
}

/// Last four characters, for "is this the right key?" without revealing it.
pub fn key_hint(secret: &str) -> String {
    let n = secret.chars().count();
    if n <= 4 {
        return "•".repeat(n);
    }
    format!("••••{}", secret.chars().skip(n - 4).collect::<String>())
}

/// Assemble the publish URL. Kept in one place so nothing else concatenates a
/// stream key into a string.
pub fn build_ingest_url(rtmps_url: &str, stream_key: &str) -> String {
    format!("{}/{}", rtmps_url.trim_end_matches('/'), stream_key.trim())
}

/// Reject keys that would break the FFmpeg URL or that are obviously not keys.
pub fn validate_stream_key(key: &str) -> Result<()> {
    let k = key.trim();
    if k.is_empty() {
        return Err(LouverError::new(ErrorCode::StreamNoStreamKey));
    }
    if k.chars().any(|c| c.is_whitespace() || c == '/' || c == '\\') {
        return Err(LouverError::with_detail(
            ErrorCode::ConfigInvalid,
            "stream key contains whitespace or a path separator",
        ));
    }
    Ok(())
}

/// Convenience wrapper binding a store to the stream-key account.
#[derive(Debug)]
pub struct StreamKeyStore {
    inner: Arc<dyn SecretStore>,
}

impl StreamKeyStore {
    pub fn new(inner: Arc<dyn SecretStore>) -> Self {
        Self { inner }
    }
    pub fn set(&self, key: &str) -> Result<()> {
        validate_stream_key(key)?;
        self.inner.set(STREAM_KEY_ACCOUNT, key.trim())
    }
    pub fn get(&self) -> Result<Option<String>> {
        self.inner.get(STREAM_KEY_ACCOUNT)
    }
    pub fn require(&self) -> Result<String> {
        self.get()?.ok_or_else(|| LouverError::new(ErrorCode::StreamNoStreamKey))
    }
    pub fn clear(&self) -> Result<()> {
        self.inner.delete(STREAM_KEY_ACCOUNT)
    }
    pub fn has_key(&self) -> bool {
        matches!(self.get(), Ok(Some(k)) if !k.is_empty())
    }
    pub fn masked(&self) -> String {
        masked_display(self.get().ok().flatten().as_deref())
    }
    pub fn backend_name(&self) -> &'static str {
        self.inner.backend_name()
    }
    pub fn is_secure(&self) -> bool {
        self.inner.is_secure()
    }
}

/// Directory the app stores its data in, per platform.
pub fn default_app_data_dir() -> PathBuf {
    if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("LouverLive")
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Library/Application Support/LouverLive")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("LouverLive")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> StreamKeyStore {
        StreamKeyStore::new(Arc::new(MemorySecretStore::new()))
    }

    #[test]
    fn key_round_trips_through_the_store() {
        let s = store();
        assert!(!s.has_key());
        assert_eq!(s.require().unwrap_err().code, ErrorCode::StreamNoStreamKey);

        s.set("abcd-efgh-ijkl-mnop").unwrap();
        assert!(s.has_key());
        assert_eq!(s.require().unwrap(), "abcd-efgh-ijkl-mnop");

        s.clear().unwrap();
        assert!(!s.has_key());
    }

    #[test]
    fn stored_key_is_trimmed() {
        let s = store();
        s.set("  abcd-efgh-ijkl-mnop \n").unwrap();
        assert_eq!(s.require().unwrap(), "abcd-efgh-ijkl-mnop");
    }

    #[test]
    fn ui_never_sees_the_real_characters() {
        let s = store();
        s.set("abcd-efgh-ijkl-mnop").unwrap();
        let m = s.masked();
        assert!(m.chars().all(|c| c == '•'), "{m}");
        assert!(!m.contains("abcd"));
        assert_eq!(masked_display(None), "");
        assert_eq!(masked_display(Some("")), "");
    }

    #[test]
    fn masked_length_does_not_reveal_the_key_length_exactly() {
        // Clamped to 8..=24 so a very short or very long key is not obvious.
        assert_eq!(masked_display(Some("ab")).chars().count(), 8);
        assert_eq!(masked_display(Some(&"x".repeat(80))).chars().count(), 24);
    }

    #[test]
    fn key_hint_shows_only_the_tail() {
        assert_eq!(key_hint("abcd-efgh-ijkl-mnop"), "••••mnop");
        assert_eq!(key_hint("ab"), "••");
    }

    #[test]
    fn ingest_url_is_assembled_without_double_slashes() {
        assert_eq!(
            build_ingest_url("rtmps://a.rtmps.youtube.com/live2", "abcd-efgh"),
            "rtmps://a.rtmps.youtube.com/live2/abcd-efgh"
        );
        assert_eq!(
            build_ingest_url("rtmps://a.rtmps.youtube.com/live2/", " abcd-efgh "),
            "rtmps://a.rtmps.youtube.com/live2/abcd-efgh"
        );
    }

    #[test]
    fn the_assembled_url_is_masked_when_logged() {
        let url = build_ingest_url("rtmps://a.rtmps.youtube.com/live2", "abcd-efgh-ijkl-mnop");
        let masked = crate::streaming::ffmpeg::mask_secrets(&url);
        assert!(!masked.contains("abcd"), "{masked}");
    }

    #[test]
    fn invalid_keys_are_rejected_before_they_reach_ffmpeg() {
        assert_eq!(validate_stream_key("").unwrap_err().code, ErrorCode::StreamNoStreamKey);
        assert_eq!(validate_stream_key("   ").unwrap_err().code, ErrorCode::StreamNoStreamKey);
        // A key containing a slash would silently change the RTMP app path.
        assert_eq!(validate_stream_key("abc/def").unwrap_err().code, ErrorCode::ConfigInvalid);
        assert_eq!(validate_stream_key("abc def").unwrap_err().code, ErrorCode::ConfigInvalid);
        validate_stream_key("abcd-efgh-ijkl-mnop").unwrap();
    }

    #[test]
    fn memory_store_declares_itself_insecure() {
        let s = store();
        assert!(!s.is_secure(), "the fallback must tell the UI it is not a keychain");
        assert!(!s.backend_name().is_empty());
    }

    #[test]
    fn app_data_dir_is_platform_appropriate() {
        let d = default_app_data_dir();
        assert!(d.to_string_lossy().contains("LouverLive"));
    }
}
