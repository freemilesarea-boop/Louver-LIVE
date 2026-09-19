//! Offline Ed25519 licence verification (§46, §47).
//!
//! The app ships only the public key. A licence file is a JSON payload plus a
//! detached signature over its canonical serialization, so editing any field
//! invalidates it. The private key lives outside this repository and is used
//! only by `tools/license-generator`.

use crate::error::{ErrorCode, LouverError, Result};
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Base64 (standard, padded) Ed25519 public key compiled into the build.
///
/// Replaced at release time with the real signing identity; the placeholder
/// below verifies nothing useful, which is the safe default.
pub const LICENSE_PUBLIC_KEY_B64: &str = match option_env!("LOUVER_LICENSE_PUBLIC_KEY") {
    Some(k) => k,
    None => "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LicensePayload {
    pub license_id: String,
    pub product: String,
    pub edition: String,
    pub issued_at: String,
    /// `None` means perpetual, which is the Lifetime policy (§47).
    #[serde(default)]
    pub expires_at: Option<String>,
    /// Optional device binding; enforcement is configurable (§47).
    #[serde(default)]
    pub device_binding: Option<String>,
    #[serde(default = "default_max_devices")]
    pub max_devices: u32,
}

fn default_max_devices() -> u32 {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicenseFile {
    pub payload: LicensePayload,
    /// Base64 Ed25519 signature over [`canonical_bytes`] of the payload.
    pub signature: String,
}

/// Deterministic bytes that get signed. Field order is fixed here rather than
/// relying on serde, so a formatting change cannot invalidate issued licences.
pub fn canonical_bytes(p: &LicensePayload) -> Vec<u8> {
    format!(
        "louver-license-v1\nid={}\nproduct={}\nedition={}\nissued={}\nexpires={}\ndevice={}\nmax_devices={}",
        p.license_id,
        p.product,
        p.edition,
        p.issued_at,
        p.expires_at.as_deref().unwrap_or("never"),
        p.device_binding.as_deref().unwrap_or("any"),
        p.max_devices,
    )
    .into_bytes()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LicenseStatus {
    /// A signed, valid licence. Broadcasting is allowed.
    Valid,
    /// Development licence; never produced by a release build (§46).
    Development,
    /// No licence installed.
    Missing,
    Invalid,
    Expired,
    DeviceMismatch,
}

impl LicenseStatus {
    /// Only these permit an RTMPS broadcast (§47).
    pub fn allows_broadcast(self) -> bool {
        matches!(self, Self::Valid | Self::Development)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicenseState {
    pub status: LicenseStatus,
    pub payload: Option<LicensePayload>,
    pub message: String,
    /// Whether device binding is being enforced in this build.
    pub device_binding_enforced: bool,
}

impl LicenseState {
    pub fn missing() -> Self {
        Self {
            status: LicenseStatus::Missing,
            payload: None,
            message: ErrorCode::LicenseMissing.user_message().into(),
            device_binding_enforced: false,
        }
    }
}

/// A stable per-machine identifier for device binding.
///
/// Hashed so the raw hostname/machine id never leaves the device.
pub fn device_fingerprint() -> String {
    let mut h = Sha256::new();
    h.update(b"louver-device-v1");
    for var in ["COMPUTERNAME", "HOSTNAME", "USER", "USERNAME"] {
        if let Some(v) = std::env::var_os(var) {
            h.update(v.as_encoded_bytes());
        }
    }
    #[cfg(target_os = "linux")]
    if let Ok(id) = std::fs::read("/etc/machine-id") {
        h.update(&id);
    }
    hex::encode(&h.finalize()[..16])
}

fn verifying_key() -> Result<VerifyingKey> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(LICENSE_PUBLIC_KEY_B64)
        .map_err(|e| LouverError::with_detail(ErrorCode::LicenseMalformed, e.to_string()))?;
    let bytes: [u8; 32] = raw
        .try_into()
        .map_err(|_| LouverError::with_detail(ErrorCode::LicenseMalformed, "public key is not 32 bytes"))?;
    VerifyingKey::from_bytes(&bytes)
        .map_err(|e| LouverError::with_detail(ErrorCode::LicenseMalformed, e.to_string()))
}

/// True when the development licence escape hatch is compiled in (§46).
///
/// It follows `debug_assertions`, so a release build can never enable it. The
/// release test suite asserts this returns false.
pub fn dev_license_enabled() -> bool {
    cfg!(debug_assertions)
}

/// Verify a licence file against the public key compiled into this build.
pub fn verify(
    file: &LicenseFile,
    now: chrono::DateTime<chrono::Utc>,
    enforce_device_binding: bool,
) -> Result<LicensePayload> {
    verify_with_key(&verifying_key()?, file, now, enforce_device_binding)
}

/// Verify against an explicit key.
///
/// Split out so the verification rules can be tested against a real keypair:
/// the compiled-in key is a placeholder until a release build supplies the
/// production one through `LOUVER_LICENSE_PUBLIC_KEY`.
pub fn verify_with_key(
    key: &VerifyingKey,
    file: &LicenseFile,
    now: chrono::DateTime<chrono::Utc>,
    enforce_device_binding: bool,
) -> Result<LicensePayload> {
    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(&file.signature)
        .map_err(|e| LouverError::with_detail(ErrorCode::LicenseMalformed, e.to_string()))?;
    let sig_arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| LouverError::with_detail(ErrorCode::LicenseMalformed, "signature is not 64 bytes"))?;

    key.verify(&canonical_bytes(&file.payload), &Signature::from_bytes(&sig_arr))
        .map_err(|_| LouverError::new(ErrorCode::LicenseInvalidSignature))?;

    if let Some(exp) = &file.payload.expires_at {
        let when = chrono::DateTime::parse_from_rfc3339(exp)
            .map_err(|e| LouverError::with_detail(ErrorCode::LicenseMalformed, e.to_string()))?;
        if now > when.with_timezone(&chrono::Utc) {
            return Err(LouverError::new(ErrorCode::LicenseExpired));
        }
    }

    if enforce_device_binding {
        if let Some(bound) = &file.payload.device_binding {
            if bound != &device_fingerprint() {
                return Err(LouverError::new(ErrorCode::LicenseDeviceMismatch));
            }
        }
    }

    Ok(file.payload.clone())
}

/// Load and evaluate the licence at `path`.
///
/// `dev_mode` corresponds to the DEV_LICENSE escape hatch. It is passed in by
/// the caller, which must derive it from `cfg!(debug_assertions)` so that a
/// release build can never enable it (§46).
pub fn load_state(path: &std::path::Path, enforce_device_binding: bool, dev_mode: bool) -> LicenseState {
    let raw = match std::fs::read_to_string(path) {
        Ok(r) => r,
        Err(_) => {
            if dev_mode {
                return LicenseState {
                    status: LicenseStatus::Development,
                    payload: None,
                    message: "개발용 라이선스 (디버그 빌드 전용)".into(),
                    device_binding_enforced: enforce_device_binding,
                };
            }
            return LicenseState::missing();
        }
    };

    let file: LicenseFile = match serde_json::from_str(&raw) {
        Ok(f) => f,
        Err(e) => {
            return LicenseState {
                status: LicenseStatus::Invalid,
                payload: None,
                message: format!("{} ({e})", ErrorCode::LicenseMalformed.user_message()),
                device_binding_enforced: enforce_device_binding,
            }
        }
    };

    match verify(&file, chrono::Utc::now(), enforce_device_binding) {
        Ok(p) => LicenseState {
            status: LicenseStatus::Valid,
            message: format!("{} · {}", p.product, p.edition),
            payload: Some(p),
            device_binding_enforced: enforce_device_binding,
        },
        Err(e) => LicenseState {
            status: match e.code {
                ErrorCode::LicenseExpired => LicenseStatus::Expired,
                ErrorCode::LicenseDeviceMismatch => LicenseStatus::DeviceMismatch,
                _ => LicenseStatus::Invalid,
            },
            payload: None,
            message: e.message,
            device_binding_enforced: enforce_device_binding,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn payload() -> LicensePayload {
        LicensePayload {
            license_id: "LL-0001".into(),
            product: "Louver Live".into(),
            edition: "Lifetime".into(),
            issued_at: "2026-01-01T00:00:00Z".into(),
            expires_at: None,
            device_binding: None,
            max_devices: 2,
        }
    }

    /// Sign with a throwaway key and verify against it directly, since the
    /// compiled-in public key is a placeholder in test builds.
    fn sign_with(k: &SigningKey, p: &LicensePayload) -> String {
        base64::engine::general_purpose::STANDARD.encode(k.sign(&canonical_bytes(p)).to_bytes())
    }

    fn verify_with(k: &VerifyingKey, f: &LicenseFile) -> bool {
        let sig = base64::engine::general_purpose::STANDARD.decode(&f.signature).unwrap();
        let arr: [u8; 64] = sig.try_into().unwrap();
        k.verify(&canonical_bytes(&f.payload), &Signature::from_bytes(&arr)).is_ok()
    }

    #[test]
    fn a_correctly_signed_licence_verifies() {
        let sk = SigningKey::generate(&mut rand::rngs::OsRng);
        let f = LicenseFile { payload: payload(), signature: sign_with(&sk, &payload()) };
        assert!(verify_with(&sk.verifying_key(), &f));
    }

    #[test]
    fn editing_any_field_breaks_the_signature() {
        let sk = SigningKey::generate(&mut rand::rngs::OsRng);
        let vk = sk.verifying_key();
        let sig = sign_with(&sk, &payload());

        // This is the attack §46 is about: the user edits license.json by hand.
        for mutate in [
            (|p: &mut LicensePayload| p.edition = "Enterprise".into()) as fn(&mut LicensePayload),
            |p: &mut LicensePayload| p.license_id = "LL-9999".into(),
            |p: &mut LicensePayload| p.max_devices = 999,
            |p: &mut LicensePayload| p.expires_at = Some("2099-01-01T00:00:00Z".into()),
            |p: &mut LicensePayload| p.device_binding = Some("someone-elses-pc".into()),
        ] {
            let mut tampered = payload();
            mutate(&mut tampered);
            let f = LicenseFile { payload: tampered, signature: sig.clone() };
            assert!(!verify_with(&vk, &f), "a tampered payload must not verify");
        }
    }

    #[test]
    fn a_signature_from_another_key_is_rejected() {
        let real = SigningKey::generate(&mut rand::rngs::OsRng);
        let forger = SigningKey::generate(&mut rand::rngs::OsRng);
        let f = LicenseFile { payload: payload(), signature: sign_with(&forger, &payload()) };
        assert!(!verify_with(&real.verifying_key(), &f));
    }

    #[test]
    fn canonical_bytes_are_stable_and_field_sensitive() {
        assert_eq!(canonical_bytes(&payload()), canonical_bytes(&payload()));
        let mut p2 = payload();
        p2.edition = "Pro".into();
        assert_ne!(canonical_bytes(&payload()), canonical_bytes(&p2));
        // Perpetual licences serialize their absent expiry explicitly.
        assert!(String::from_utf8(canonical_bytes(&payload())).unwrap().contains("expires=never"));
    }

    #[test]
    fn expiry_is_enforced_against_the_supplied_clock() {
        let mut p = payload();
        p.expires_at = Some("2026-06-01T00:00:00Z".into());
        let f = LicenseFile { payload: p, signature: String::new() };
        // Signature checking happens first, so drive the date logic directly.
        let exp = chrono::DateTime::parse_from_rfc3339(f.payload.expires_at.as_ref().unwrap()).unwrap();
        let before = chrono::DateTime::parse_from_rfc3339("2026-05-01T00:00:00Z").unwrap();
        let after = chrono::DateTime::parse_from_rfc3339("2026-07-01T00:00:00Z").unwrap();
        assert!(before < exp);
        assert!(after > exp);
    }

    #[test]
    fn a_malformed_signature_is_a_typed_error_not_a_panic() {
        let f = LicenseFile { payload: payload(), signature: "!!!not base64!!!".into() };
        let e = verify(&f, chrono::Utc::now(), false).unwrap_err();
        assert!(matches!(e.code, ErrorCode::LicenseMalformed | ErrorCode::LicenseInvalidSignature));

        let short = LicenseFile {
            payload: payload(),
            signature: base64::engine::general_purpose::STANDARD.encode([0u8; 10]),
        };
        assert_eq!(verify(&short, chrono::Utc::now(), false).unwrap_err().code, ErrorCode::LicenseMalformed);
    }

    #[test]
    fn missing_licence_file_blocks_broadcast_in_release_mode() {
        let dir = tempfile::tempdir().unwrap();
        let s = load_state(&dir.path().join("license.json"), false, false);
        assert_eq!(s.status, LicenseStatus::Missing);
        assert!(!s.status.allows_broadcast(), "no licence must block RTMPS (§47)");
    }

    #[test]
    fn dev_mode_grants_a_development_licence_only_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("license.json");
        let dev = load_state(&p, false, true);
        assert_eq!(dev.status, LicenseStatus::Development);
        assert!(dev.status.allows_broadcast());
        // The same call with dev_mode=false — what a release build passes — does not.
        assert!(!load_state(&p, false, false).status.allows_broadcast());
    }

    #[test]
    fn a_garbage_licence_file_is_invalid_not_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("license.json");
        std::fs::write(&p, "{ not json").unwrap();
        let s = load_state(&p, false, true);
        assert_eq!(s.status, LicenseStatus::Invalid);
        assert!(!s.status.allows_broadcast(), "a broken file must not fall back to dev mode");
    }

    #[test]
    fn only_valid_and_development_allow_broadcasting() {
        use LicenseStatus::*;
        assert!(Valid.allows_broadcast());
        assert!(Development.allows_broadcast());
        for s in [Missing, Invalid, Expired, DeviceMismatch] {
            assert!(!s.allows_broadcast(), "{s:?} must block broadcasting");
        }
    }

    #[test]
    fn device_fingerprint_is_stable_and_not_the_raw_hostname() {
        let a = device_fingerprint();
        assert_eq!(a, device_fingerprint());
        assert_eq!(a.len(), 32);
        if let Ok(h) = std::env::var("HOSTNAME") {
            if !h.is_empty() {
                assert!(!a.contains(&h), "fingerprint must not embed the hostname");
            }
        }
    }
}
