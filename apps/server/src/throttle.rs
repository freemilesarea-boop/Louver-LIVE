//! How often one caller may try to sign in, or sign up.
//!
//! Not written for a determined attacker — it is written for the machine this
//! runs on. Verifying a password is 600,000 rounds of PBKDF2, deliberately, and
//! that is a third of a second of one core. The production server has two cores
//! and both of them are meant to be pushing video. A few hundred login attempts
//! a minute — a script, or a credential-stuffing list — would take the CPU away
//! from every broadcast on the server without ever guessing a password.
//!
//! So: a small in-process counter per caller, per route. No store, no
//! dependency, nothing to operate. A restart forgets everything, which is the
//! right trade for something whose job is to blunt a burst.
//!
//! Behind a proxy the caller is `X-Forwarded-For`; served directly it is the
//! peer address. A request with neither is not arriving over a socket at all —
//! that is a test driving the router in-process — and is not counted.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How far back the counter looks.
pub const WINDOW: Duration = Duration::from_secs(60);

/// Attempts allowed inside the window, per caller, per route.
///
/// Ten is far above a person mistyping a password and far below anything that
/// costs the server a core.
pub const MAX_ATTEMPTS: usize = 10;

/// Stop tracking callers past this many. A flood from thousands of addresses
/// must not become a memory leak; the oldest are simply forgotten, which at
/// worst lets that flood through — the same as having no limiter, and no worse.
const MAX_TRACKED: usize = 4096;

#[derive(Default)]
pub struct Throttle {
    seen: Mutex<HashMap<(&'static str, String), Vec<Instant>>>,
}

impl Throttle {
    /// Count this attempt. `Err(retry_after)` when the caller is over the limit.
    pub fn check(&self, route: &'static str, who: &str) -> Result<(), Duration> {
        let now = Instant::now();
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());

        if seen.len() > MAX_TRACKED {
            seen.retain(|_, hits| hits.iter().any(|t| now.duration_since(*t) < WINDOW));
        }
        let hits = seen.entry((route, who.to_string())).or_default();
        hits.retain(|t| now.duration_since(*t) < WINDOW);
        if hits.len() >= MAX_ATTEMPTS {
            // The oldest hit inside the window decides when there is room again.
            let oldest = hits.first().copied().unwrap_or(now);
            return Err(WINDOW.saturating_sub(now.duration_since(oldest)));
        }
        hits.push(now);
        Ok(())
    }

    /// For tests: forget everything.
    pub fn clear(&self) {
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

/// The one counter this process uses.
pub fn shared() -> &'static Throttle {
    static IT: OnceLock<Throttle> = OnceLock::new();
    IT.get_or_init(Throttle::default)
}

/// Who is asking, for counting purposes. `None` when the request did not come
/// over a socket, which is only ever a test.
pub fn caller_key(headers: &axum::http::HeaderMap, peer: Option<SocketAddr>) -> Option<String> {
    if let Some(v) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        // A chain appends, so the first entry is the client the proxy saw.
        if let Some(first) = v.split(',').next().map(str::trim).filter(|s| !s.is_empty()) {
            return Some(first.to_string());
        }
    }
    peer.map(|p| p.ip().to_string())
}

/// The peer address, when this request came over a socket.
///
/// Its own extractor rather than `Option<ConnectInfo<_>>`, which axum does not
/// offer: the router is also driven directly by the tests, where there is no
/// connection and therefore no extension to read.
#[derive(Debug, Clone, Copy)]
pub struct Peer(pub Option<SocketAddr>);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for Peer {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self(parts.extensions.get::<axum::extract::ConnectInfo<SocketAddr>>().map(|c| c.0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_from_one_caller_is_cut_off_and_lets_go_on_its_own() {
        let t = Throttle::default();
        for i in 0..MAX_ATTEMPTS {
            t.check("login", "1.2.3.4").unwrap_or_else(|_| panic!("attempt {i} was refused"));
        }
        let waited = t.check("login", "1.2.3.4").expect_err("the eleventh attempt was allowed");
        assert!(waited <= WINDOW && waited > Duration::ZERO, "{waited:?}");
    }

    #[test]
    fn one_caller_being_throttled_does_not_touch_anybody_else() {
        let t = Throttle::default();
        for _ in 0..MAX_ATTEMPTS {
            t.check("login", "1.2.3.4").unwrap();
        }
        assert!(t.check("login", "1.2.3.4").is_err());
        t.check("login", "5.6.7.8").expect("a different caller was caught in someone else's limit");
        // And a different route is a different budget: filling the signup form
        // in must not lock a user out of signing in.
        t.check("register", "1.2.3.4").expect("one route's limit spilled into another's");
    }

    #[test]
    fn the_proxys_header_wins_over_the_proxys_own_address() {
        let mut h = axum::http::HeaderMap::new();
        h.insert("x-forwarded-for", "203.0.113.9, 10.0.0.2".parse().unwrap());
        let peer: SocketAddr = "10.0.0.2:53124".parse().unwrap();
        assert_eq!(caller_key(&h, Some(peer)).as_deref(), Some("203.0.113.9"));
        // Without the header, the peer. Without either, nothing to count.
        assert_eq!(caller_key(&axum::http::HeaderMap::new(), Some(peer)).as_deref(), Some("10.0.0.2"));
        assert_eq!(caller_key(&axum::http::HeaderMap::new(), None), None);
    }
}
