//! One constant-time shared-secret comparison, used by more than one check.
//!
//! There are two shared secrets in this service and they must not be
//! interchangeable:
//!
//!  * [`crate::gate`] — "did this request come through 247streams' proxy?"
//!  * [`crate::admin`] — "is this the operator?"
//!
//! They are different questions with very different consequences, so the same
//! string presented in the wrong header must fail. That is enforced here
//! rather than left to configuration discipline: each secret is hashed under
//! its own **domain-separation label**, so even an operator who sets both
//! environment variables to the same value gets two different digests, and the
//! gate secret offered as the admin secret does not match.
//!
//! The secret is hashed once at construction and the original string is
//! dropped, so what lives in this process for its lifetime is not the secret.
//! The comparison hashes the candidate the same way, which also makes it a
//! fixed-length compare whatever length an attacker sent — comparing raw
//! values would leak the secret's length through timing, and `==` on `str`
//! would leak its prefix.

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// The shortest shared secret this service will accept, for either use.
pub const MIN_SECRET_LEN: usize = 32;

/// A shared secret, as a labelled digest.
#[derive(Clone)]
pub(crate) struct ConstantTimeSecret {
    digest: [u8; 32],
}

impl std::fmt::Debug for ConstantTimeSecret {
    /// Nothing. Not a prefix, not a length, not a fingerprint.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConstantTimeSecret")
    }
}

impl ConstantTimeSecret {
    /// `None` when the secret is too short to be worth having.
    ///
    /// The caller turns that into its own refusal, because "the gate secret is
    /// too short" and "the admin secret is too short" need to name different
    /// environment variables for an operator to act on.
    pub(crate) fn new(label: &[u8], secret: &str) -> Option<Self> {
        (secret.len() >= MIN_SECRET_LEN).then(|| Self { digest: digest(label, secret.as_bytes()) })
    }

    /// Whether this candidate is the secret. Constant-time.
    ///
    /// Takes an `Option` so that a missing header is a case this function
    /// decides, not one a caller can forget: `None` hashes the empty string
    /// and fails, taking the same time as a wrong value of any length.
    pub(crate) fn matches(&self, label: &[u8], candidate: Option<&str>) -> bool {
        digest(label, candidate.unwrap_or("").as_bytes()).ct_eq(&self.digest).into()
    }
}

fn digest(label: &[u8], bytes: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    // The label is what keeps one secret's digest from being the other's, and
    // the NUL keeps a label from running into the secret.
    h.update(label);
    h.update(b"\0");
    h.update(bytes);
    h.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &[u8] = b"label-one";
    const B: &[u8] = b"label-two";
    const SECRET: &str = "a-shared-secret-of-at-least-32-bytes";

    #[test]
    fn the_right_secret_under_the_right_label_matches() {
        let s = ConstantTimeSecret::new(A, SECRET).unwrap();
        assert!(s.matches(A, Some(SECRET)));
    }

    #[test]
    fn the_same_secret_under_a_different_label_does_not_match() {
        // The property that makes the gate secret useless as the admin secret
        // even when an operator sets both variables to the same string.
        let s = ConstantTimeSecret::new(A, SECRET).unwrap();
        assert!(!s.matches(B, Some(SECRET)));
    }

    #[test]
    fn a_missing_or_wrong_candidate_does_not_match() {
        let s = ConstantTimeSecret::new(A, SECRET).unwrap();
        for bad in [None, Some(""), Some(" "), Some("wrong-but-also-32-bytes-long-ok!!")] {
            assert!(!s.matches(A, bad), "{bad:?} matched");
        }
        // Near misses, which a prefix compare would leak.
        for bad in [&SECRET[..SECRET.len() - 1], &format!("{SECRET}x"), &SECRET.to_ascii_uppercase()] {
            assert!(!s.matches(A, Some(bad)), "{bad:?} matched");
        }
    }

    #[test]
    fn a_secret_shorter_than_the_minimum_is_refused() {
        for short in ["", "x", &"x".repeat(MIN_SECRET_LEN - 1)] {
            assert!(ConstantTimeSecret::new(A, short).is_none(), "{} accepted", short.len());
        }
        assert!(ConstantTimeSecret::new(A, &"x".repeat(MIN_SECRET_LEN)).is_some());
    }

    #[test]
    fn nothing_about_the_secret_is_printable() {
        let s = ConstantTimeSecret::new(A, SECRET).unwrap();
        let shown = format!("{s:?}");
        assert_eq!(shown, "ConstantTimeSecret");
        assert!(!shown.contains("shared-secret"));
    }
}
