//! Short-lived, scoped tokens for this API, and the HMAC under them.
//!
//! ## Why this exists rather than passing the session cookie around
//!
//! 247streams issues an `HttpOnly; SameSite=Strict; Path=/` session cookie that
//! is good for the **whole** production API. Handing that to a worker on another
//! machine, on every request, would mean a compromise of that machine is a
//! compromise of every beta user's entire account — uploads, billing, YouTube
//! connections, all of it.
//!
//! So the cookie is used **once**, on one endpoint, to ask production who the
//! caller is ([`crate::auth`]), and is then dropped. Everything after that
//! carries a token minted here, which:
//!
//!  * names the audience (`live-source`), so it is useless against production;
//!  * expires in minutes, not weeks;
//!  * carries only the user id and the plan, which is all this API needs;
//!  * is signed with a secret this worker and nothing else holds.
//!
//! When a louver-server change is approved, production can mint these itself and
//! [`verify`] keeps working unchanged — the format is deliberately the same.
//!
//! ## The HMAC
//!
//! RFC 2104 HMAC-SHA-256 composed over the `sha2` crate, which is already in
//! this project's tree. Written out rather than adding a dependency, and pinned
//! against the published RFC 4231 test vectors in the tests below, so its
//! correctness is a measured fact and not a claim. Swapping in the `hmac` crate
//! would be a one-line change to [`hmac_sha256`] if a reviewer prefers that.
//!
//! Comparison is constant-time ([`subtle`]). A byte-by-byte `==` on a signature
//! leaks, through timing, how much of a guess was right.

use crate::error::{LiveSourceError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// The only audience this worker accepts. A token for anything else is refused
/// even if the signature is good.
pub const AUDIENCE: &str = "live-source";

/// How long a minted token is good for.
///
/// Short enough that a leaked one is a small window, long enough that a user
/// filling in a form does not get logged out mid-edit. The beta page re-runs the
/// handshake when a call comes back 401.
pub const TOKEN_TTL_SECS: i64 = 5 * 60;

/// The smallest secret this will accept, in bytes.
///
/// Refusing a short secret at startup is the difference between a signed token
/// and a decorative one.
pub const MIN_SECRET_LEN: usize = 32;

/// What a token says. Everything here is non-secret: an internal user id, a
/// plan name, two timestamps and an audience.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    /// The 247streams user id, as `/api/me` reported it.
    pub sub: String,
    /// Their plan, so a per-plan ceiling can be applied without a database.
    pub plan: String,
    pub aud: String,
    /// Unix seconds.
    pub exp: i64,
    pub iat: i64,
}

impl Claims {
    pub fn new(sub: impl Into<String>, plan: impl Into<String>, now: i64) -> Self {
        Self {
            sub: sub.into(),
            plan: plan.into(),
            aud: AUDIENCE.to_string(),
            exp: now + TOKEN_TTL_SECS,
            iat: now,
        }
    }
}

/// The signing key, held as a value so it cannot be printed.
#[derive(Clone)]
pub struct Signer {
    secret: Vec<u8>,
}

impl std::fmt::Debug for Signer {
    /// No key material, no length, nothing.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Signer")
    }
}

impl Signer {
    /// Refuse a secret too short to be one.
    pub fn new(secret: impl AsRef<[u8]>) -> Result<Self> {
        let secret = secret.as_ref();
        if secret.len() < MIN_SECRET_LEN {
            return Err(LiveSourceError::invalid(format!(
                "서명 비밀키가 너무 짧습니다 ({}바이트 이상 필요).",
                MIN_SECRET_LEN
            )));
        }
        Ok(Self { secret: secret.to_vec() })
    }

    /// `<base64url(json)>.<base64url(mac)>`
    ///
    /// Not JWT: there is no algorithm field, so there is no "alg: none" and no
    /// algorithm confusion. One format, one algorithm, both fixed in code.
    pub fn mint(&self, claims: &Claims) -> String {
        let body = serde_json::to_vec(claims).expect("claims are plain data");
        let payload = b64_encode(&body);
        let mac = hmac_sha256(&self.secret, payload.as_bytes());
        format!("{payload}.{}", b64_encode(&mac))
    }

    /// Read a token, or say why it cannot be used.
    ///
    /// The order matters: signature first, then expiry, then audience. Checking
    /// the claims of an unverified token would be acting on attacker-supplied
    /// data.
    pub fn verify(&self, token: &str, now: i64) -> Result<Claims> {
        let (payload, sig) = token.split_once('.').ok_or_else(|| {
            LiveSourceError::new(crate::ErrorKind::Unauthorized, "토큰 형식이 올바르지 않습니다.")
        })?;
        let given = b64_decode(sig).ok_or_else(|| {
            LiveSourceError::new(crate::ErrorKind::Unauthorized, "토큰 서명을 읽을 수 없습니다.")
        })?;
        let want = hmac_sha256(&self.secret, payload.as_bytes());
        if given.ct_eq(&want).unwrap_u8() != 1 {
            return Err(LiveSourceError::new(
                crate::ErrorKind::Unauthorized,
                "토큰 서명이 일치하지 않습니다.",
            ));
        }
        let body = b64_decode(payload).ok_or_else(|| {
            LiveSourceError::new(crate::ErrorKind::Unauthorized, "토큰 본문을 읽을 수 없습니다.")
        })?;
        let claims: Claims = serde_json::from_slice(&body).map_err(|_| {
            LiveSourceError::new(crate::ErrorKind::Unauthorized, "토큰 본문을 해석할 수 없습니다.")
        })?;
        if claims.aud != AUDIENCE {
            return Err(LiveSourceError::new(
                crate::ErrorKind::Unauthorized,
                "이 토큰은 여기에서 사용할 수 없습니다.",
            ));
        }
        if claims.exp <= now {
            return Err(LiveSourceError::new(
                crate::ErrorKind::Unauthorized,
                "토큰이 만료되었습니다. 새로고침해 주세요.",
            ));
        }
        if claims.sub.trim().is_empty() {
            return Err(LiveSourceError::new(crate::ErrorKind::Unauthorized, "토큰에 사용자가 없습니다."));
        }
        Ok(claims)
    }
}

/// RFC 2104 HMAC-SHA-256.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    // A key longer than the block size is hashed first; a shorter one is
    // zero-padded. Both are the RFC's own rules.
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        let digest = Sha256::digest(key);
        k[..32].copy_from_slice(&digest);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    outer.finalize().into()
}

/// base64url without padding. Written here because the token format has to be
/// URL-safe and stable, and because `base64`'s API has changed often enough
/// that pinning the behaviour in this file is the cheaper option.
fn b64_encode(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((data.len() * 4).div_ceil(3));
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(A[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(A[n as usize & 63] as char);
        }
    }
    out
}

fn b64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a') as u32 + 26,
            b'0'..=b'9' => (c - b'0') as u32 + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        })
    }
    let bytes = s.as_bytes();
    if bytes.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            n |= val(*c)? << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer() -> Signer {
        Signer::new("a-secret-long-enough-to-be-accepted-32").unwrap()
    }

    /// RFC 4231 §4. The published vectors, so this HMAC is measured against the
    /// standard rather than against itself.
    #[test]
    fn the_hmac_matches_the_rfc_4231_vectors() {
        // Case 1
        let mac = hmac_sha256(&[0x0b; 20], b"Hi There");
        assert_eq!(hex(&mac), "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7");
        // Case 2 — a key shorter than the block, an ASCII message.
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(hex(&mac), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
        // Case 3
        let mac = hmac_sha256(&[0xaa; 20], &[0xdd; 50]);
        assert_eq!(hex(&mac), "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe");
        // Case 6 — a key *longer* than the block size, which takes the
        // hash-the-key branch.
        let mac = hmac_sha256(&[0xaa; 131], b"Test Using Larger Than Block-Size Key - Hash Key First");
        assert_eq!(hex(&mac), "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54");
        // Case 7 — long key and long message.
        let mac = hmac_sha256(
            &[0xaa; 131],
            b"This is a test using a larger than block-size key and a larger than block-size data. The key needs to be hashed before being used by the HMAC algorithm.",
        );
        assert_eq!(hex(&mac), "9b09ffa71b942fcb27635fbcd5b0e944bfdc63644f0713938a7f51535c3a35e2");
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn base64url_round_trips_including_the_awkward_lengths() {
        for n in 0..40usize {
            let data: Vec<u8> = (0..n).map(|i| (i * 7 + 3) as u8).collect();
            let e = b64_encode(&data);
            assert!(!e.contains('='), "no padding: {e}");
            assert!(!e.contains('+') && !e.contains('/'), "url-safe: {e}");
            assert_eq!(b64_decode(&e).as_deref(), Some(data.as_slice()), "n={n}");
        }
        assert_eq!(b64_decode("not base64 !!"), None);
    }

    #[test]
    fn a_minted_token_reads_back() {
        let s = signer();
        let c = Claims::new("user-1", "business", 1_000_000);
        let t = s.mint(&c);
        assert_eq!(s.verify(&t, 1_000_010).unwrap(), c);
    }

    #[test]
    fn a_tampered_payload_is_refused() {
        let s = signer();
        let t = s.mint(&Claims::new("user-1", "basic", 1_000_000));
        let (payload, sig) = t.split_once('.').unwrap();
        // Re-sign is impossible without the key, so an attacker edits the body
        // and keeps the signature.
        let forged = Claims {
            sub: "user-2".into(),
            plan: "business".into(),
            aud: AUDIENCE.into(),
            exp: 9_000_000,
            iat: 1,
        };
        let other = b64_encode(&serde_json::to_vec(&forged).unwrap());
        for bad in [format!("{other}.{sig}"), format!("{payload}x.{sig}"), format!("{payload}.{sig}x")] {
            let e = s.verify(&bad, 1_000_010).unwrap_err();
            assert_eq!(e.kind, crate::ErrorKind::Unauthorized, "{bad}");
        }
    }

    #[test]
    fn a_token_signed_with_another_key_is_refused() {
        let other = Signer::new("a-completely-different-secret-key-32!").unwrap();
        let t = other.mint(&Claims::new("user-1", "basic", 1_000_000));
        assert_eq!(signer().verify(&t, 1_000_010).unwrap_err().kind, crate::ErrorKind::Unauthorized);
    }

    #[test]
    fn an_expired_token_is_refused() {
        let s = signer();
        let t = s.mint(&Claims::new("user-1", "basic", 1_000_000));
        assert!(s.verify(&t, 1_000_000 + TOKEN_TTL_SECS - 1).is_ok());
        let e = s.verify(&t, 1_000_000 + TOKEN_TTL_SECS).unwrap_err();
        assert!(e.message.contains("만료"), "{}", e.message);
    }

    #[test]
    fn a_token_for_another_audience_is_refused_even_when_properly_signed() {
        // The point of the audience: a token minted for production must not open
        // this API, and vice versa.
        let s = signer();
        let mut c = Claims::new("user-1", "basic", 1_000_000);
        c.aud = "louver-session".into();
        let t = s.mint(&c);
        let e = s.verify(&t, 1_000_010).unwrap_err();
        assert!(e.message.contains("사용할 수 없습니다"), "{}", e.message);
    }

    #[test]
    fn there_is_no_algorithm_field_to_confuse() {
        // Not JWT on purpose. A header with `alg` is the single most exploited
        // part of that format, so the format here has no header at all.
        let t = signer().mint(&Claims::new("u", "basic", 1));
        assert_eq!(t.split('.').count(), 2, "payload and mac, nothing else");
        let body = b64_decode(t.split('.').next().unwrap()).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v.get("alg").is_none());
        assert!(v.get("typ").is_none());
    }

    #[test]
    fn a_short_secret_is_refused_at_construction() {
        assert!(Signer::new("too-short").is_err());
        assert!(Signer::new("x".repeat(MIN_SECRET_LEN - 1)).is_err());
        assert!(Signer::new("x".repeat(MIN_SECRET_LEN)).is_ok());
    }

    #[test]
    fn a_signer_cannot_be_printed() {
        assert_eq!(format!("{:?}", signer()), "Signer");
        assert!(!format!("{:?}", signer()).contains("secret"));
    }

    #[test]
    fn a_subject_less_token_is_refused() {
        let s = signer();
        let mut c = Claims::new("", "basic", 1_000_000);
        c.sub = "   ".into();
        let e = s.verify(&s.mint(&c), 1_000_010).unwrap_err();
        assert!(e.message.contains("사용자가 없습니다"), "{}", e.message);
    }
}
