//! Who is calling, established without giving this machine the keys to the
//! whole account.
//!
//! ## The two ways to do this, and why this is the one here
//!
//! **A — production mints the token.** `louver-server` gains an endpoint that
//! reads its own session cookie and returns a short-lived token for this API.
//! The cookie never leaves the origin server; this worker only ever sees a
//! token that is scoped, expiring and useless against production. This is the
//! better design and the one to move to.
//! It needs a `louver-server` code change, which means a new image and a
//! container recreate — not approved, and the thing this whole project is
//! organised around avoiding.
//!
//! **B — Caddy gates the request.** `forward_auth louver:8080 { uri /api/me }`
//! refuses unauthenticated requests at the edge, with no production code change
//! at all. But `forward_auth` decides *whether*, not *who*: `/api/me` answers
//! with a JSON body and Caddy cannot read a body into a header. So B alone
//! cannot tell this API which user is calling, and an API that cannot do that
//! cannot keep one user's jobs away from another's.
//!
//! **What is implemented: B as the outer gate, plus a one-shot handshake for
//! identity.** The session cookie crosses to this worker on exactly one
//! endpoint, is used immediately to ask production `/api/me` who it belongs to,
//! and is then dropped — never stored, never logged, never reused. The reply is
//! a token from [`crate::token`], and every other endpoint takes that token and
//! has the cookie stripped by Caddy. So the cookie is not passed
//! *unconditionally*: one request per 30 minutes, one endpoint, one use.
//!
//! Moving to A later changes [`Identity::from_production`] and nothing else.
//!
//! ## Logout, expiry and suspension — what this actually does
//!
//! Read from production's own code rather than assumed, because a wrong answer
//! here is the difference between a bounded window and an open door:
//!
//!  * **Logout** — `apps/server/src/auth.rs` `logout` calls
//!    `delete_auth_session(hash)`, so that cookie's row is gone and the cookie
//!    stops working on the next request.
//!  * **Session expiry** — `db.user_for_token` is
//!    `… WHERE token_hash=?1 AND expires_at > datetime('now')`, so an expired
//!    cookie resolves to nothing.
//!  * **Suspension** — `louver_cloud::admin::set_disabled` sets `disabled_at`
//!    **and** runs `DELETE FROM auth_sessions WHERE user_id=?1`. So disabling
//!    an account destroys every one of its cookies immediately; it does not
//!    merely mark a flag that `/api/me` would have to report.
//!
//! In all three cases `GET /api/me` with that cookie answers 401, so the next
//! handshake here fails and this page's `connect()` tells the user to sign in
//! again. The worker needs no new production endpoint for that.
//!
//! **What is left, stated precisely.** A bearer token this worker already
//! minted is *not* revoked by any of the three: it is self-contained and this
//! worker does not ask production again until the next handshake. So a user who
//! logs out, or is suspended, keeps a usable token for **at most the remainder
//! of [`crate::token::TOKEN_TTL_SECS`]** — five minutes, and on average less.
//! That window is why the lifetime is five minutes and not thirty.
//!
//! **What is left that five minutes does not bound.** A beta job that is
//! already running keeps running. Production's `set_disabled` stops the
//! broadcasts *it* manages (`mgr.stop_all_for`); it knows nothing about this
//! worker, so a suspended account's live-source job sends until it ends or is
//! cancelled. Closing that needs either a production change (option A, or a
//! revocation list this worker can read) or an operator step, and until one
//! exists this is a **blocker for opening the beta to customers** rather than
//! something the five-minute token fixes. It is recorded here, not papered
//! over: nothing in this crate claims suspension is handled.
//!
//! ## What this module will not do
//!
//! It will not accept an identity from the caller. There is no "user_id" field
//! in any request body. The only ways to become somebody here are a cookie that
//! production vouches for, or a token this worker signed.

use crate::error::{LiveSourceError, Result};
use crate::token::{Claims, Signer};
use std::time::Duration;

/// How long to wait for production to answer the handshake.
pub const ME_TIMEOUT: Duration = Duration::from_secs(8);

/// The session cookie's name, as `apps/server` sets it.
pub const SESSION_COOKIE: &str = "louver_session";

/// Who the caller is, once established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub user_id: String,
    pub plan: String,
}

/// Asking production who a cookie belongs to.
///
/// A trait so the tests never need a 247streams instance, and so option A above
/// is a swap of one implementation.
pub trait IdentitySource: Send + Sync {
    /// `cookie_header` is the raw `Cookie:` value. Implementations must not log
    /// it, store it, or send it anywhere but the production origin.
    fn whoami(&self, cookie_header: &str) -> Result<Identity>;
}

/// The real one: `GET {origin}/api/me` with the caller's cookie.
#[derive(Debug, Clone)]
pub struct ProductionMe {
    /// e.g. `https://247streams.kr`. Fixed in configuration, never from a
    /// request — otherwise this endpoint would be an SSRF primitive that
    /// forwards a session cookie to an address an attacker chose.
    origin: String,
}

impl ProductionMe {
    /// Only an `https://` origin with no path, query or credentials.
    pub fn new(origin: impl Into<String>) -> Result<Self> {
        let origin = origin.into().trim_end_matches('/').to_string();
        let rest = origin
            .strip_prefix("https://")
            .ok_or_else(|| LiveSourceError::invalid("origin 은 https:// 로 시작해야 합니다."))?;
        if rest.is_empty() || rest.contains('/') || rest.contains('@') || rest.contains('?') {
            return Err(LiveSourceError::invalid("origin 은 호스트만 지정해야 합니다."));
        }
        Ok(Self { origin })
    }
}

impl IdentitySource for ProductionMe {
    fn whoami(&self, cookie_header: &str) -> Result<Identity> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(ME_TIMEOUT))
            .http_status_as_error(false)
            .build()
            .into();
        let url = format!("{}/api/me", self.origin);
        // The cookie goes to the production origin and to nowhere else. It is
        // not in any log line in this crate: `cookie_header` is not a parameter
        // to anything that prints.
        let mut resp = agent
            .get(&url)
            .header("Cookie", cookie_header)
            .header("Accept", "application/json")
            .call()
            .map_err(|e| {
                // `e` can contain the URL but never the cookie — ureq does not
                // put request headers in its errors.
                LiveSourceError::unauthorized(format!("로그인 상태를 확인할 수 없습니다: {e}"))
            })?;
        let status = resp.status().as_u16();
        if status == 401 || status == 403 {
            return Err(LiveSourceError::unauthorized("로그인이 필요합니다."));
        }
        if !(200..300).contains(&status) {
            return Err(LiveSourceError::unauthorized(format!(
                "로그인 상태를 확인할 수 없습니다 (HTTP {status})."
            )));
        }
        let body = resp.body_mut().read_to_string().unwrap_or_default();
        parse_me(&body)
    }
}

/// Read `/api/me`'s answer. Split out so every branch is tested without a
/// server.
pub fn parse_me(body: &str) -> Result<Identity> {
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| LiveSourceError::unauthorized("로그인 응답을 해석할 수 없습니다."))?;
    let user_id = v.get("id").and_then(|x| x.as_str()).unwrap_or_default().trim().to_string();
    if user_id.is_empty() {
        return Err(LiveSourceError::unauthorized("로그인 응답에 사용자가 없습니다."));
    }
    let plan = v.get("plan_id").and_then(|x| x.as_str()).unwrap_or("basic").trim().to_string();
    Ok(Identity { user_id, plan: if plan.is_empty() { "basic".into() } else { plan } })
}

/// Pull the session cookie out of a `Cookie:` header.
///
/// Returns the whole `name=value` pair, because that is what gets forwarded,
/// and only for the one cookie this flow needs — a browser sends every cookie
/// for the origin and the rest are none of this worker's business.
pub fn session_cookie_only(cookie_header: &str) -> Option<String> {
    for part in cookie_header.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix(&format!("{SESSION_COOKIE}=")) {
            if v.is_empty() {
                return None;
            }
            return Some(format!("{SESSION_COOKIE}={v}"));
        }
    }
    None
}

/// Read a `Bearer` token out of an `Authorization` header.
pub fn bearer(header: &str) -> Option<&str> {
    let rest = header.strip_prefix("Bearer ").or_else(|| header.strip_prefix("bearer "))?;
    let rest = rest.trim();
    (!rest.is_empty()).then_some(rest)
}

impl Identity {
    /// The handshake: a cookie in, a scoped token out.
    pub fn from_production(
        source: &dyn IdentitySource,
        cookie_header: &str,
        signer: &Signer,
        now: i64,
    ) -> Result<(Self, String)> {
        let only = session_cookie_only(cookie_header)
            .ok_or_else(|| LiveSourceError::unauthorized("로그인이 필요합니다."))?;
        let id = source.whoami(&only)?;
        let token = signer.mint(&Claims::new(&id.user_id, &id.plan, now));
        Ok((id, token))
    }

    /// Every other request: a token in, an identity out.
    pub fn from_token(signer: &Signer, token: &str, now: i64) -> Result<Self> {
        let c = signer.verify(token, now)?;
        Ok(Self { user_id: c.sub, plan: c.plan })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;

    struct Fixed(&'static str);
    impl IdentitySource for Fixed {
        fn whoami(&self, cookie_header: &str) -> Result<Identity> {
            // The handshake must forward only the session cookie.
            assert!(cookie_header.starts_with("louver_session="), "{cookie_header}");
            assert!(!cookie_header.contains("other="), "unrelated cookies must not travel");
            parse_me(self.0)
        }
    }

    fn signer() -> Signer {
        Signer::new("a-secret-long-enough-to-be-accepted-32").unwrap()
    }

    #[test]
    fn the_handshake_turns_a_cookie_into_a_scoped_token() {
        let src = Fixed(r#"{"id":"user-1","email":"a@b.c","role":"user","plan_id":"business"}"#);
        let (id, token) =
            Identity::from_production(&src, "other=1; louver_session=abc123; x=2", &signer(), 1_000).unwrap();
        assert_eq!(id, Identity { user_id: "user-1".into(), plan: "business".into() });
        // The token stands on its own afterwards.
        assert_eq!(Identity::from_token(&signer(), &token, 1_010).unwrap(), id);
        // And it is not the cookie.
        assert!(!token.contains("abc123"), "{token}");
    }

    #[test]
    fn only_the_session_cookie_is_forwarded() {
        assert_eq!(session_cookie_only("louver_session=v"), Some("louver_session=v".into()));
        assert_eq!(
            session_cookie_only("a=1; louver_session=v; b=2"),
            Some("louver_session=v".into()),
            "the other cookies are none of this worker's business"
        );
        assert_eq!(session_cookie_only("a=1; b=2"), None);
        assert_eq!(session_cookie_only("louver_session="), None);
        assert_eq!(session_cookie_only(""), None);
    }

    #[test]
    fn a_request_with_no_cookie_cannot_authenticate() {
        let src = Fixed(r#"{"id":"user-1"}"#);
        let e = Identity::from_production(&src, "nothing=here", &signer(), 1_000).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Unauthorized);
    }

    #[test]
    fn production_saying_no_is_not_an_identity() {
        struct Refuses;
        impl IdentitySource for Refuses {
            fn whoami(&self, _: &str) -> Result<Identity> {
                Err(LiveSourceError::unauthorized("로그인이 필요합니다."))
            }
        }
        let e = Identity::from_production(&Refuses, "louver_session=x", &signer(), 1).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Unauthorized);
    }

    #[test]
    fn a_me_response_without_a_user_is_refused() {
        for body in [r#"{}"#, r#"{"id":""}"#, r#"{"id":"   "}"#, "not json", r#"[]"#] {
            assert_eq!(parse_me(body).unwrap_err().kind, ErrorKind::Unauthorized, "{body}");
        }
    }

    #[test]
    fn a_missing_plan_reads_as_basic_rather_than_as_unlimited() {
        // Defaulting the other way would give an unknown plan the largest
        // ceiling, which is the wrong direction to be wrong in.
        assert_eq!(parse_me(r#"{"id":"u"}"#).unwrap().plan, "basic");
        assert_eq!(parse_me(r#"{"id":"u","plan_id":""}"#).unwrap().plan, "basic");
    }

    #[test]
    fn the_origin_cannot_be_pointed_anywhere_interesting() {
        // This endpoint forwards a session cookie, so the destination must be
        // fixed configuration and a narrow shape.
        for bad in [
            "http://247streams.kr",
            "https://evil.example@247streams.kr",
            "https://247streams.kr/api/me",
            "https://247streams.kr/?x=1",
            "https://",
            "247streams.kr",
            "file:///etc/passwd",
        ] {
            assert!(ProductionMe::new(bad).is_err(), "{bad}");
        }
        assert!(ProductionMe::new("https://247streams.kr").is_ok());
        assert!(ProductionMe::new("https://247streams.kr/").is_ok());
    }

    #[test]
    fn a_bearer_header_is_read_strictly() {
        assert_eq!(bearer("Bearer abc"), Some("abc"));
        assert_eq!(bearer("bearer abc"), Some("abc"));
        assert_eq!(bearer("Bearer   abc  "), Some("abc"));
        for bad in ["abc", "Basic abc", "Bearer", "Bearer ", "BearerX abc"] {
            assert_eq!(bearer(bad), None, "{bad}");
        }
    }

    #[test]
    fn an_identity_cannot_be_asserted_by_the_caller() {
        // There is no constructor that takes a user id from a request. The only
        // two ways in are a cookie production vouched for and a token this
        // worker signed — which this test documents by exhausting the API.
        let s = signer();
        assert!(Identity::from_token(&s, "made.up", 1).is_err());
        assert!(Identity::from_token(&s, "", 1).is_err());
    }
}
