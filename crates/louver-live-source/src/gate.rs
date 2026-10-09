//! The shared secret between the reverse proxy and this worker.
//!
//! ## What it is for, and what it is not for
//!
//! Until a firewall is in front of this process, its listening port is
//! reachable by anything that can route to the machine. The gate makes a
//! request that did not come through 247streams' Caddy useless: Caddy adds
//! `X-Louver-Gate: <secret>` to everything it proxies here, and a request
//! without it is refused before any handler runs.
//!
//! It is **not** authentication. It says *this request came through the front
//! door*, not *this is Alice*. Every route that touches a user's data still
//! requires that user's own bearer token, and `POST …/session` still requires
//! their session cookie — so a leaked gate secret gets an attacker as far as an
//! unauthenticated 401 and no further. That double check is the point: one
//! secret shared by every request can only ever be the outer layer.
//!
//! ## Fail-closed
//!
//! Three ways this could fail open, and what stops each:
//!
//!  * **No secret configured.** The binary will not start. There is no
//!    "gate disabled" mode to leave on by accident, and no constructor here
//!    that produces an open gate.
//!  * **A short secret.** Refused at construction, same as the signing key.
//!  * **A missing or wrong header.** Refused with 403 and no detail. The
//!    comparison is constant-time, so a wrong value tells an attacker nothing
//!    about how wrong it was.
//!
//! The secret comes from the environment only. It is never an argv element (a
//! process listing is world-readable), never logged, and `Debug` prints no part
//! of it.

use crate::error::{LiveSourceError, Result};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Shortest gate secret this will accept.
pub const MIN_SECRET_LEN: usize = 32;

/// The header Caddy adds and this worker requires.
pub const GATE_HEADER: &str = "x-louver-gate";

/// The shared secret, as a digest.
///
/// The bytes are hashed once at construction and the original is dropped, so
/// what sits in this process's memory for its whole life is not the secret
/// itself. The comparison hashes the candidate the same way, which also makes
/// it a fixed-length compare whatever length the attacker sent.
#[derive(Clone)]
pub struct Gate {
    digest: [u8; 32],
}

impl std::fmt::Debug for Gate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Gate")
    }
}

impl Gate {
    /// Build from the configured secret, refusing one too short to matter.
    pub fn new(secret: &str) -> Result<Self> {
        if secret.len() < MIN_SECRET_LEN {
            return Err(LiveSourceError::invalid(format!(
                "게이트 비밀값은 {MIN_SECRET_LEN}바이트 이상이어야 합니다."
            )));
        }
        Ok(Self { digest: digest(secret.as_bytes()) })
    }

    /// Accept this request's gate header, or refuse it.
    ///
    /// `None` — the header is absent — is refused here rather than by a caller,
    /// so there is no code path where forgetting to check means allowing.
    pub fn check(&self, header: Option<&str>) -> Result<()> {
        let got = header.unwrap_or("");
        // Constant-time over equal-length digests. Hashing first is what makes
        // the lengths equal: comparing the raw values would leak the secret's
        // length through timing, and `==` on `str` would leak its prefix.
        if digest(got.as_bytes()).ct_eq(&self.digest).into() {
            return Ok(());
        }
        // No detail at all. "Missing" and "wrong" are the same answer, because
        // telling them apart tells an attacker whether the header name is
        // right.
        Err(LiveSourceError::new(crate::ErrorKind::Forbidden, "이 경로로는 접근할 수 없습니다."))
    }
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    // Domain separation: this digest is only ever compared with another of its
    // own kind, and the prefix keeps it from colliding with any other use of
    // SHA-256 in this process.
    h.update(b"louver-live-source-gate\0");
    h.update(bytes);
    h.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "a-gate-secret-of-at-least-32-bytes-long";

    #[test]
    fn the_configured_secret_is_accepted() {
        assert!(Gate::new(SECRET).unwrap().check(Some(SECRET)).is_ok());
    }

    #[test]
    fn a_missing_header_is_refused_exactly_like_a_wrong_one() {
        let g = Gate::new(SECRET).unwrap();
        let absent = g.check(None).unwrap_err();
        let wrong = g.check(Some("not-the-secret-but-also-32-bytes-long")).unwrap_err();
        // Same kind and same words: the difference would say whether the header
        // name was right.
        assert_eq!(absent.kind, crate::ErrorKind::Forbidden);
        assert_eq!(absent.kind, wrong.kind);
        assert_eq!(absent.message, wrong.message);
    }

    #[test]
    fn a_near_miss_is_refused() {
        let g = Gate::new(SECRET).unwrap();
        for bad in [
            "",
            " ",
            &SECRET[..SECRET.len() - 1],
            &format!("{SECRET}x"),
            &format!(" {SECRET}"),
            &SECRET.to_ascii_uppercase(),
            &SECRET[1..],
        ] {
            assert!(g.check(Some(bad)).is_err(), "{bad:?} accepted");
        }
    }

    #[test]
    fn a_secret_too_short_to_matter_is_refused_at_construction() {
        // Fail-closed: there is no constructor that yields an open gate, so a
        // weak secret has to be a refusal to start.
        for short in ["", "x", &"x".repeat(MIN_SECRET_LEN - 1)] {
            assert!(Gate::new(short).is_err(), "{} accepted", short.len());
        }
        assert!(Gate::new(&"x".repeat(MIN_SECRET_LEN)).is_ok());
    }

    #[test]
    fn nothing_prints_the_secret() {
        let g = Gate::new(SECRET).unwrap();
        assert_eq!(format!("{g:?}"), "Gate");
        for s in [format!("{g:?}"), g.check(Some("x")).unwrap_err().message] {
            assert!(!s.contains("gate-secret"), "leaked in {s}");
            assert!(!s.contains(SECRET), "leaked in {s}");
        }
    }

    #[test]
    fn two_gates_with_the_same_secret_agree_and_with_different_ones_do_not() {
        let other = "a-different-gate-secret-32-bytes-ok!!";
        assert!(Gate::new(SECRET).unwrap().check(Some(SECRET)).is_ok());
        assert!(Gate::new(other).unwrap().check(Some(SECRET)).is_err());
        assert!(Gate::new(SECRET).unwrap().check(Some(other)).is_err());
    }
}
