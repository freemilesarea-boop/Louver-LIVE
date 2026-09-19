//! Production licence verification (§16, §47).
//!
//! The public key compiled into a normal build is a placeholder, so these
//! tests generate a real keypair and drive [`verify_with_key`] — the same code
//! path a release build uses, with the key supplied rather than baked in.
//!
//! Every case §16 lists is covered: a valid licence, a modified licence, an
//! expired licence, a wrong signature, and a missing licence.

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use louver_core::license::{
    canonical_bytes, dev_license_enabled, load_state, verify_with_key, LicenseFile, LicensePayload,
    LicenseStatus, LICENSE_PUBLIC_KEY_B64,
};
use louver_core::ErrorCode;

fn b64() -> base64::engine::general_purpose::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn keypair() -> (SigningKey, VerifyingKey) {
    let sk = SigningKey::generate(&mut rand::rngs::OsRng);
    let vk = sk.verifying_key();
    (sk, vk)
}

fn payload() -> LicensePayload {
    LicensePayload {
        license_id: "LL-RC-0001".into(),
        product: "Louver Live".into(),
        edition: "Lifetime".into(),
        issued_at: "2026-01-01T00:00:00Z".into(),
        expires_at: None,
        device_binding: None,
        max_devices: 2,
    }
}

fn sign(sk: &SigningKey, p: &LicensePayload) -> LicenseFile {
    LicenseFile { signature: b64().encode(sk.sign(&canonical_bytes(p)).to_bytes()), payload: p.clone() }
}

fn now() -> chrono::DateTime<chrono::Utc> {
    "2026-06-01T00:00:00Z".parse().unwrap()
}

// --- the five cases §16 names ---------------------------------------------

#[test]
fn case_valid_license_is_accepted() {
    let (sk, vk) = keypair();
    let f = sign(&sk, &payload());
    let got = verify_with_key(&vk, &f, now(), false).expect("a correctly signed licence must verify");
    assert_eq!(got.license_id, "LL-RC-0001");
    assert_eq!(got.edition, "Lifetime");
    assert_eq!(got.max_devices, 2);
}

#[test]
fn case_modified_license_is_rejected_field_by_field() {
    let (sk, vk) = keypair();
    let signed = sign(&sk, &payload());

    // Each of these is an edit a buyer might make by hand to upgrade themselves.
    /// A field edit a buyer might attempt, and a label for the failure message.
    type Edit = (&'static str, fn(&mut LicensePayload));
    let edits: Vec<Edit> = vec![
        ("edition", |p| p.edition = "Enterprise".into()),
        ("license_id", |p| p.license_id = "LL-9999".into()),
        ("max_devices", |p| p.max_devices = 999),
        ("expiry removed/extended", |p| p.expires_at = Some("2099-01-01T00:00:00Z".into())),
        ("device binding", |p| p.device_binding = Some("another-machine".into())),
        ("product", |p| p.product = "Louver Live Pro".into()),
        ("issued_at", |p| p.issued_at = "2020-01-01T00:00:00Z".into()),
    ];
    for (what, edit) in edits {
        let mut tampered = payload();
        edit(&mut tampered);
        let f = LicenseFile { payload: tampered, signature: signed.signature.clone() };
        let e = verify_with_key(&vk, &f, now(), false)
            .expect_err(&format!("a licence with a modified {what} must be rejected"));
        assert_eq!(e.code, ErrorCode::LicenseInvalidSignature, "{what}");
    }
}

#[test]
fn case_expired_license_is_rejected_and_a_future_one_is_not() {
    let (sk, vk) = keypair();

    let mut expired = payload();
    expired.expires_at = Some("2026-03-01T00:00:00Z".into());
    let e = verify_with_key(&vk, &sign(&sk, &expired), now(), false).unwrap_err();
    assert_eq!(e.code, ErrorCode::LicenseExpired);

    let mut future = payload();
    future.expires_at = Some("2027-01-01T00:00:00Z".into());
    verify_with_key(&vk, &sign(&sk, &future), now(), false).expect("a licence valid until 2027 must pass");

    // A perpetual licence never expires.
    verify_with_key(&vk, &sign(&sk, &payload()), "2099-01-01T00:00:00Z".parse().unwrap(), false)
        .expect("a Lifetime licence must not expire");
}

#[test]
fn case_wrong_signature_is_rejected() {
    let (real, vk) = keypair();
    let (forger, _) = keypair();

    // Signed with someone else's key.
    let e = verify_with_key(&vk, &sign(&forger, &payload()), now(), false).unwrap_err();
    assert_eq!(e.code, ErrorCode::LicenseInvalidSignature);

    // Correct key, corrupted signature bytes.
    let mut f = sign(&real, &payload());
    let mut raw = b64().decode(&f.signature).unwrap();
    raw[0] ^= 0xFF;
    f.signature = b64().encode(raw);
    assert_eq!(verify_with_key(&vk, &f, now(), false).unwrap_err().code, ErrorCode::LicenseInvalidSignature);

    // Structurally invalid signatures are typed errors, not panics.
    for bad in ["", "not base64!!", &b64().encode([0u8; 10])] {
        let f = LicenseFile { payload: payload(), signature: bad.to_string() };
        let e = verify_with_key(&vk, &f, now(), false).unwrap_err();
        assert!(
            matches!(e.code, ErrorCode::LicenseMalformed | ErrorCode::LicenseInvalidSignature),
            "{bad:?}"
        );
    }
}

#[test]
fn case_missing_license_blocks_broadcasting_in_a_release_build() {
    let dir = tempfile::tempdir().unwrap();
    // dev_mode=false is what a release build passes.
    let s = load_state(&dir.path().join("license.json"), false, false);
    assert_eq!(s.status, LicenseStatus::Missing);
    assert!(!s.status.allows_broadcast(), "§47: no licence means no RTMPS broadcast");
    assert!(!s.message.is_empty());
}

// --- device binding (§47) --------------------------------------------------

#[test]
fn device_binding_is_only_enforced_when_configured() {
    let (sk, vk) = keypair();
    let mut bound = payload();
    bound.device_binding = Some("a-different-machine".into());
    let f = sign(&sk, &bound);

    // Off by default: a bound licence still works elsewhere.
    verify_with_key(&vk, &f, now(), false).expect("binding off means any device");

    // On: the mismatch is reported with its own code.
    let e = verify_with_key(&vk, &f, now(), true).unwrap_err();
    assert_eq!(e.code, ErrorCode::LicenseDeviceMismatch);

    // A licence bound to *this* machine passes even when enforcement is on.
    let mut mine = payload();
    mine.device_binding = Some(louver_core::license::device_fingerprint());
    verify_with_key(&vk, &sign(&sk, &mine), now(), true).expect("this machine's own licence must pass");
}

// --- build-time guarantees -------------------------------------------------

#[test]
fn the_shipped_public_key_is_still_a_placeholder_in_this_build() {
    // A real key must arrive through LOUVER_LICENSE_PUBLIC_KEY at build time,
    // never by being committed. If this ever fails, a key was hard-coded.
    if std::env::var("LOUVER_LICENSE_PUBLIC_KEY").is_err() {
        assert_eq!(
            LICENSE_PUBLIC_KEY_B64, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "a licence public key appears to be hard-coded in the source"
        );
    }
}

#[test]
fn the_development_licence_follows_the_build_profile() {
    // In a debug build the escape hatch exists; `cargo test --release` runs the
    // same assertion and proves it is compiled out (§46).
    assert_eq!(dev_license_enabled(), cfg!(debug_assertions));
    #[cfg(not(debug_assertions))]
    {
        assert!(!dev_license_enabled(), "DEV_LICENSE must be disabled in a release build");
        let dir = tempfile::tempdir().unwrap();
        let s = load_state(&dir.path().join("license.json"), false, dev_license_enabled());
        assert_eq!(s.status, LicenseStatus::Missing);
        assert!(!s.status.allows_broadcast());
    }
}

#[test]
fn a_corrupt_licence_file_never_falls_back_to_a_development_licence() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("license.json");
    for bad in ["{ not json", "", "[]", "{\"payload\":{}}"] {
        std::fs::write(&p, bad).unwrap();
        // Even with dev mode on, a present-but-broken file is invalid.
        let s = load_state(&p, false, true);
        assert_eq!(s.status, LicenseStatus::Invalid, "input {bad:?}");
        assert!(!s.status.allows_broadcast(), "input {bad:?}");
    }
}

#[test]
fn only_valid_and_development_states_permit_a_broadcast() {
    use LicenseStatus::*;
    assert!(Valid.allows_broadcast());
    assert!(Development.allows_broadcast());
    for s in [Missing, Invalid, Expired, DeviceMismatch] {
        assert!(!s.allows_broadcast(), "{s:?} must block broadcasting");
    }
}
