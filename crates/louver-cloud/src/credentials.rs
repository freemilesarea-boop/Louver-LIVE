//! Sealed secrets for a server that has no keychain.
//!
//! The desktop puts a stream key in the OS keychain through
//! [`louver_core::security::SecretStore`]. A Linux container has no keychain, so
//! the cloud implements the *same trait* over a database table holding
//! ciphertext. Nothing above this module changes: `StreamKeyStore` and the
//! broadcast runtime keep talking to a `SecretStore` and neither knows which
//! one it got.
//!
//! Sealing uses ChaCha20-Poly1305 from `ring`, already in the tree via rustls,
//! under a master key supplied by the environment. Passwords use
//! PBKDF2-HMAC-SHA256 from the same crate. No new crypto dependency, and no new
//! code implementing a primitive.

use crate::{CloudError, Result};
use louver_core::error::{ErrorCode, LouverError};
use louver_core::security::SecretStore;
use ring::aead::{self, BoundKey, Nonce, NonceSequence, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::Connection;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};

/// PBKDF2 rounds for a password. Deliberately slow.
///
/// 600,000 is OWASP's 2023 figure for PBKDF2-HMAC-SHA256. A login costs a few
/// hundred milliseconds of server CPU, which is the point.
const PBKDF2_ROUNDS: u32 = 600_000;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;

/// Length of the master key, in bytes, when hex-decoded.
pub const MASTER_KEY_LEN: usize = 32;

/// A nonce used once, then refused.
///
/// `ring`'s API asks for a sequence so that reuse is hard to write by accident.
/// Every seal here makes a fresh key and a fresh random nonce, and this returns
/// it exactly once; a second call is a programming error and fails loudly
/// rather than encrypting twice under one nonce.
struct OneNonce(Option<[u8; NONCE_LEN]>);

impl NonceSequence for OneNonce {
    fn advance(&mut self) -> std::result::Result<Nonce, ring::error::Unspecified> {
        self.0.take().map(Nonce::assume_unique_for_key).ok_or(ring::error::Unspecified)
    }
}

/// Seal `plaintext`, returning `nonce || ciphertext || tag`.
pub fn seal(master: &[u8; MASTER_KEY_LEN], plaintext: &[u8]) -> Result<Vec<u8>> {
    let mut nonce = [0u8; NONCE_LEN];
    SystemRandom::new().fill(&mut nonce).map_err(|_| CloudError::Crypto)?;

    let key = UnboundKey::new(&aead::CHACHA20_POLY1305, master).map_err(|_| CloudError::Crypto)?;
    let mut sealing = aead::SealingKey::new(key, OneNonce(Some(nonce)));
    let mut body = plaintext.to_vec();
    sealing.seal_in_place_append_tag(aead::Aad::empty(), &mut body).map_err(|_| CloudError::Crypto)?;

    let mut out = Vec::with_capacity(NONCE_LEN + body.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&body);
    Ok(out)
}

/// Open what [`seal`] produced. Any tampering fails rather than returning bytes.
pub fn open(master: &[u8; MASTER_KEY_LEN], sealed: &[u8]) -> Result<Vec<u8>> {
    if sealed.len() <= NONCE_LEN {
        return Err(CloudError::Crypto);
    }
    let (n, body) = sealed.split_at(NONCE_LEN);
    let nonce: [u8; NONCE_LEN] = n.try_into().map_err(|_| CloudError::Crypto)?;

    let key = UnboundKey::new(&aead::CHACHA20_POLY1305, master).map_err(|_| CloudError::Crypto)?;
    let mut opening = aead::OpeningKey::new(key, OneNonce(Some(nonce)));
    let mut body = body.to_vec();
    let plain = opening.open_in_place(aead::Aad::empty(), &mut body).map_err(|_| CloudError::Crypto)?;
    Ok(plain.to_vec())
}

/// Read the master key from the environment.
///
/// Refuses to invent one. A server started without `LOUVER_MASTER_KEY` cannot
/// read the keys it stored yesterday, and generating a fresh key would silently
/// turn every saved destination into garbage — better to fail at boot with a
/// sentence saying what to set.
pub fn master_key_from_env() -> Result<[u8; MASTER_KEY_LEN]> {
    let raw = std::env::var("LOUVER_MASTER_KEY").map_err(|_| {
        CloudError::Invalid("LOUVER_MASTER_KEY is not set. Generate one with `openssl rand -hex 32`.".into())
    })?;
    let bytes =
        hex::decode(raw.trim()).map_err(|_| CloudError::Invalid("LOUVER_MASTER_KEY must be hex".into()))?;
    bytes.try_into().map_err(|_| CloudError::Invalid("LOUVER_MASTER_KEY must be 32 bytes of hex".into()))
}

/// Hash a password for storage. Returns `salt_hex:hash_hex`.
pub fn hash_password(password: &str) -> Result<String> {
    let mut salt = [0u8; SALT_LEN];
    SystemRandom::new().fill(&mut salt).map_err(|_| CloudError::Crypto)?;
    let mut out = [0u8; ring::digest::SHA256_OUTPUT_LEN];
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        NonZeroU32::new(PBKDF2_ROUNDS).unwrap(),
        &salt,
        password.as_bytes(),
        &mut out,
    );
    Ok(format!("{}:{}", hex::encode(salt), hex::encode(out)))
}

/// Check a password against a stored hash, in constant time.
pub fn verify_password(stored: &str, password: &str) -> bool {
    let Some((salt_hex, hash_hex)) = stored.split_once(':') else { return false };
    let (Ok(salt), Ok(expected)) = (hex::decode(salt_hex), hex::decode(hash_hex)) else {
        return false;
    };
    ring::pbkdf2::verify(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        NonZeroU32::new(PBKDF2_ROUNDS).unwrap(),
        &salt,
        password.as_bytes(),
        &expected,
    )
    .is_ok()
}

/// A random opaque token, and the hash by which it is stored.
///
/// The plaintext is shown to its owner once and never written down, so a stolen
/// database yields no usable session.
pub fn new_token() -> Result<(String, String)> {
    let mut raw = [0u8; 32];
    SystemRandom::new().fill(&mut raw).map_err(|_| CloudError::Crypto)?;
    let token = hex::encode(raw);
    Ok((token.clone(), token_hash(&token)))
}

pub fn token_hash(token: &str) -> String {
    use ring::digest;
    hex::encode(digest::digest(&digest::SHA256, token.as_bytes()))
}

/// [`SecretStore`] over a table of sealed blobs.
///
/// Implements the trait the desktop already uses, so `StreamKeyStore` and the
/// runtime work unchanged. `account` is the caller's key — the cloud uses
/// `destination:<id>` — which is why one store serves every tenant without any
/// per-user instance.
#[derive(Clone)]
pub struct CredentialStore {
    conn: Arc<Mutex<Connection>>,
    master: [u8; MASTER_KEY_LEN],
}

impl std::fmt::Debug for CredentialStore {
    /// No key material, no secret, not even a count.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CredentialStore")
    }
}

impl CredentialStore {
    pub fn new(conn: Arc<Mutex<Connection>>, master: [u8; MASTER_KEY_LEN]) -> Self {
        Self { conn, master }
    }

    fn to_louver(e: CloudError) -> LouverError {
        LouverError::with_detail(ErrorCode::SecretStoreUnavailable, e.to_string())
    }
}

impl SecretStore for CredentialStore {
    fn set(&self, account: &str, secret: &str) -> louver_core::error::Result<()> {
        let sealed = seal(&self.master, secret.as_bytes()).map_err(Self::to_louver)?;
        self.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO credentials (account, sealed) VALUES (?1, ?2)
                 ON CONFLICT(account) DO UPDATE SET sealed=excluded.sealed",
                rusqlite::params![account, sealed],
            )
            .map_err(|e| Self::to_louver(CloudError::Db(e)))?;
        Ok(())
    }

    fn get(&self, account: &str) -> louver_core::error::Result<Option<String>> {
        let sealed: Option<Vec<u8>> = self
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT sealed FROM credentials WHERE account=?1", [account], |r| r.get(0))
            .ok();
        let Some(sealed) = sealed else { return Ok(None) };
        let plain = open(&self.master, &sealed).map_err(Self::to_louver)?;
        Ok(Some(String::from_utf8_lossy(&plain).into_owned()))
    }

    fn delete(&self, account: &str) -> louver_core::error::Result<()> {
        self.conn
            .lock()
            .unwrap()
            .execute("DELETE FROM credentials WHERE account=?1", [account])
            .map_err(|e| Self::to_louver(CloudError::Db(e)))?;
        Ok(())
    }

    fn backend_name(&self) -> &'static str {
        "Louver Cloud (sealed)"
    }
}

/// The account name a destination's key is stored under.
pub fn destination_account(destination_id: &str) -> String {
    format!("destination:{destination_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> [u8; MASTER_KEY_LEN] {
        [7u8; MASTER_KEY_LEN]
    }

    #[test]
    fn a_sealed_secret_comes_back_exactly() {
        let sealed = seal(&key(), b"abcd-efgh-ijkl-mnop").unwrap();
        assert_ne!(sealed, b"abcd-efgh-ijkl-mnop", "the key is sitting there in the clear");
        assert_eq!(open(&key(), &sealed).unwrap(), b"abcd-efgh-ijkl-mnop");
    }

    #[test]
    fn the_same_secret_seals_differently_every_time() {
        // A fresh nonce per seal. Equal ciphertexts would tell an observer with
        // read access to the table which users share a stream key.
        let a = seal(&key(), b"same").unwrap();
        let b = seal(&key(), b"same").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn a_tampered_or_wrongly_keyed_secret_does_not_open() {
        let mut sealed = seal(&key(), b"abcd-efgh-ijkl-mnop").unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0x01;
        assert!(open(&key(), &sealed).is_err(), "a flipped bit must not decrypt");

        let clean = seal(&key(), b"abcd-efgh-ijkl-mnop").unwrap();
        assert!(open(&[9u8; MASTER_KEY_LEN], &clean).is_err(), "another key must not open it");
        assert!(open(&key(), b"tooshort").is_err());
    }

    #[test]
    fn a_password_verifies_against_its_own_hash_and_nothing_else() {
        let h = hash_password("correct horse battery staple").unwrap();
        assert!(!h.contains("correct"), "the password is in its own hash");
        assert!(verify_password(&h, "correct horse battery staple"));
        assert!(!verify_password(&h, "Correct horse battery staple"));
        assert!(!verify_password("not-a-hash", "anything"));
    }

    #[test]
    fn two_hashes_of_one_password_differ() {
        let a = hash_password("same").unwrap();
        let b = hash_password("same").unwrap();
        assert_ne!(a, b, "the salt is not doing anything");
        assert!(verify_password(&a, "same") && verify_password(&b, "same"));
    }

    #[test]
    fn a_token_is_stored_only_as_its_hash() {
        let (token, hash) = new_token().unwrap();
        assert_eq!(token.len(), 64);
        assert_ne!(token, hash);
        assert_eq!(token_hash(&token), hash);
    }

    /// The store must not print what it holds, however it is logged.
    #[test]
    fn debugging_the_store_reveals_nothing() {
        let conn = Connection::open_in_memory().unwrap();
        let s = CredentialStore::new(Arc::new(Mutex::new(conn)), key());
        let shown = format!("{s:?}");
        assert_eq!(shown, "CredentialStore");
        assert!(!shown.contains('7'), "the master key leaked into Debug");
    }
}
