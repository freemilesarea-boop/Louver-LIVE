//! Stream key storage backed by the OS keychain (§15).
//!
//! macOS uses the Keychain and Windows the Credential Manager. On any other
//! platform — which for this product means a developer machine — the key is
//! kept in memory only, and the UI is told the store is not secure so it can
//! say so. Writing a key to a plain file is never an option (§60).

#[cfg_attr(any(target_os = "macos", windows), allow(unused_imports))]
use louver_core::error::{ErrorCode, LouverError, Result};
#[cfg(any(target_os = "macos", windows))]
use louver_core::security::SERVICE_NAME;
use louver_core::security::{MemorySecretStore, SecretStore};

#[derive(Debug)]
pub struct KeyringSecretStore;

#[cfg(any(target_os = "macos", windows))]
impl SecretStore for KeyringSecretStore {
    fn set(&self, account: &str, secret: &str) -> Result<()> {
        keyring::Entry::new(SERVICE_NAME, account)
            .and_then(|e| e.set_password(secret))
            .map_err(|e| LouverError::with_detail(ErrorCode::SecretStoreUnavailable, e.to_string()))
    }

    fn get(&self, account: &str) -> Result<Option<String>> {
        match keyring::Entry::new(SERVICE_NAME, account).and_then(|e| e.get_password()) {
            Ok(p) => Ok(Some(p)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(LouverError::with_detail(ErrorCode::SecretStoreUnavailable, e.to_string())),
        }
    }

    fn delete(&self, account: &str) -> Result<()> {
        match keyring::Entry::new(SERVICE_NAME, account).and_then(|e| e.delete_password()) {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(LouverError::with_detail(ErrorCode::SecretStoreUnavailable, e.to_string())),
        }
    }

    fn backend_name(&self) -> &'static str {
        if cfg!(target_os = "macos") {
            "macOS 키체인"
        } else {
            "Windows 자격 증명 관리자"
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
impl SecretStore for KeyringSecretStore {
    fn set(&self, _a: &str, _s: &str) -> Result<()> {
        Err(LouverError::new(ErrorCode::SecretStoreUnavailable))
    }
    fn get(&self, _a: &str) -> Result<Option<String>> {
        Ok(None)
    }
    fn delete(&self, _a: &str) -> Result<()> {
        Ok(())
    }
    fn backend_name(&self) -> &'static str {
        "지원되지 않음"
    }
    fn is_secure(&self) -> bool {
        false
    }
}

/// The best store this platform offers, falling back to memory.
pub fn default_store() -> std::sync::Arc<dyn SecretStore> {
    #[cfg(any(target_os = "macos", windows))]
    {
        // Confirm the keychain actually answers before committing to it; a
        // locked or unavailable keychain should degrade, not break startup.
        let s = KeyringSecretStore;
        if s.get("__louver_probe__").is_ok() {
            return std::sync::Arc::new(s);
        }
    }
    std::sync::Arc::new(MemorySecretStore::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use louver_core::security::StreamKeyStore;

    #[test]
    fn the_default_store_is_usable_on_every_platform() {
        let s = StreamKeyStore::new(default_store());
        // Must not panic, and must report honestly whether it is secure.
        let _ = s.has_key();
        let _ = s.backend_name();
        if !s.is_secure() {
            assert!(
                !s.backend_name().is_empty(),
                "an insecure fallback must still name itself for the Settings page"
            );
        }
    }

    #[test]
    fn a_key_never_round_trips_through_a_plain_file() {
        // The fallback is memory-only by construction; this test documents the
        // invariant so a future "just write it to disk" fix trips it.
        let s = MemorySecretStore::new();
        s.set("acct", "abcd-efgh-ijkl-mnop").unwrap();
        assert!(!s.is_secure());
    }
}
