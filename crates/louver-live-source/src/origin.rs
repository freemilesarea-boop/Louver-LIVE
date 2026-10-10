//! Which pages may start a session here.
//!
//! `POST …/session` is the one route that reads the browser's session cookie,
//! which makes it the one route a cross-site request could aim: the cookie is
//! `SameSite=Strict`, so a third-party page cannot carry it, but a worker that
//! trusted the cookie alone would be relying on one cookie attribute set by a
//! server this crate does not control. So the `Origin` header is checked as
//! well, against an exact string.
//!
//! Exact, and nothing cleverer. No suffix match (`evil-247streams.kr` ends with
//! `247streams.kr` under a naive one, and `247streams.kr.evil.test` contains
//! it), no scheme-insensitive compare, no port guessing. A configured origin is
//! compared byte for byte with the header, and anything else is refused.
//!
//! Three refusals matter and each is tested:
//!
//!  * **missing** — a non-browser client, or a browser on a request shape that
//!    carries no `Origin`. Refused, because the one thing this route accepts is
//!    a browser on the beta page.
//!  * **mismatched** — another site.
//!  * **`null`** — what a browser sends from a sandboxed iframe, a `data:` URL
//!    or a redirect-stripped origin. It is a value, not an absence, and a
//!    comparison that normalised it would accept all three.

use crate::error::{LiveSourceError, Result};

/// The origins a session may be started from. Operator-configured.
#[derive(Debug, Clone, Default)]
pub struct AllowedOrigins {
    exact: Vec<String>,
}

impl AllowedOrigins {
    /// Parse a comma-separated list. Each entry must be a bare origin:
    /// `scheme://host` or `scheme://host:port`, with no path and no trailing
    /// slash, because that is what a browser sends and anything else could
    /// never match.
    pub fn parse(raw: &str) -> Result<Self> {
        let mut exact = Vec::new();
        for part in raw.split(',') {
            let o = part.trim();
            if o.is_empty() {
                continue;
            }
            check_shape(o)?;
            if !exact.iter().any(|e| e == o) {
                exact.push(o.to_string());
            }
        }
        if exact.is_empty() {
            return Err(LiveSourceError::invalid("허용할 Origin 이 하나도 없습니다."));
        }
        Ok(Self { exact })
    }

    /// Accept this request's `Origin`, or say why not.
    ///
    /// Takes an `Option` so a missing header is a case this function decides
    /// rather than a case a caller might forget.
    pub fn check(&self, header: Option<&str>) -> Result<()> {
        let Some(got) = header else {
            return Err(LiveSourceError::unauthorized(
                "이 요청은 247streams 페이지에서만 보낼 수 있습니다. (Origin 없음)",
            ));
        };
        // `null` is a value a browser really sends, from a sandboxed iframe or
        // a `data:` document. Named here so it can never fall through a
        // normalising compare.
        if got == "null" {
            return Err(LiveSourceError::unauthorized(
                "이 요청은 247streams 페이지에서만 보낼 수 있습니다. (Origin null)",
            ));
        }
        if self.exact.iter().any(|e| e == got) {
            return Ok(());
        }
        // The rejected origin is attacker-controlled, so it is not echoed.
        Err(LiveSourceError::unauthorized(
            "이 요청은 247streams 페이지에서만 보낼 수 있습니다. (Origin 불일치)",
        ))
    }

    pub fn len(&self) -> usize {
        self.exact.len()
    }

    pub fn is_empty(&self) -> bool {
        self.exact.is_empty()
    }
}

/// A bare origin, as a browser sends it.
fn check_shape(o: &str) -> Result<()> {
    let refuse = || {
        Err(LiveSourceError::invalid(
            "허용 Origin 은 scheme://host[:port] 형태여야 합니다 (경로·슬래시 없음).",
        ))
    };
    let Some(rest) = o.strip_prefix("https://").or_else(|| o.strip_prefix("http://")) else {
        return refuse();
    };
    if rest.is_empty() || rest.contains('/') || rest.contains('?') || rest.contains('#') {
        return refuse();
    }
    if rest.contains('@') || o.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return refuse();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one() -> AllowedOrigins {
        AllowedOrigins::parse("https://247streams.kr").unwrap()
    }

    #[test]
    fn the_configured_origin_is_accepted() {
        assert!(one().check(Some("https://247streams.kr")).is_ok());
    }

    #[test]
    fn a_missing_origin_is_refused() {
        let e = one().check(None).unwrap_err();
        assert_eq!(e.kind, crate::ErrorKind::Unauthorized);
        assert!(e.message.contains("Origin 없음"), "{}", e.message);
    }

    #[test]
    fn a_null_origin_is_refused() {
        // A sandboxed iframe or a `data:` document sends this literal string.
        let e = one().check(Some("null")).unwrap_err();
        assert_eq!(e.kind, crate::ErrorKind::Unauthorized);
        assert!(e.message.contains("Origin null"), "{}", e.message);
    }

    #[test]
    fn a_mismatched_origin_is_refused_and_not_echoed() {
        for bad in [
            "https://evil.test",
            // The ones a suffix or substring match would let through.
            "https://evil-247streams.kr",
            "https://247streams.kr.evil.test",
            "https://x.247streams.kr",
            // Scheme and port are part of an origin.
            "http://247streams.kr",
            "https://247streams.kr:8443",
            // Not a bare origin at all.
            "https://247streams.kr/",
            "https://247streams.kr/beta/",
            " https://247streams.kr",
            "HTTPS://247STREAMS.KR",
            "",
        ] {
            let e = one().check(Some(bad)).unwrap_err();
            assert_eq!(e.kind, crate::ErrorKind::Unauthorized, "{bad}");
            assert!(!e.message.contains(bad) || bad.is_empty(), "{bad} echoed in {}", e.message);
        }
    }

    #[test]
    fn several_origins_can_be_configured_for_local_testing() {
        let a = AllowedOrigins::parse("https://247streams.kr, http://127.0.0.1:9080").unwrap();
        assert_eq!(a.len(), 2);
        assert!(a.check(Some("https://247streams.kr")).is_ok());
        assert!(a.check(Some("http://127.0.0.1:9080")).is_ok());
        assert!(a.check(Some("http://127.0.0.1:9081")).is_err());
    }

    #[test]
    fn a_configuration_that_could_never_match_is_refused_at_startup() {
        for raw in [
            "",
            ",",
            "247streams.kr",
            "https://247streams.kr/",
            "https://247streams.kr/beta",
            "https://user@247streams.kr",
            "https://",
            "ftp://247streams.kr",
            "https://247streams.kr?x=1",
        ] {
            assert!(AllowedOrigins::parse(raw).is_err(), "{raw:?} should not parse");
        }
    }

    #[test]
    fn a_duplicate_is_collapsed() {
        let a = AllowedOrigins::parse("https://a.test,https://a.test").unwrap();
        assert_eq!(a.len(), 1);
    }
}
