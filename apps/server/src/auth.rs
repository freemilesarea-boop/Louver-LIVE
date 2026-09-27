//! Who is calling, established once, in one place.
//!
//! Every handler that touches user data takes a [`Caller`]. There is no way to
//! write a handler that forgets, because the user id it needs to pass to the
//! `*_owned` database functions can only come from here.
//!
//! The session token travels in an `HttpOnly` cookie, so a script running in
//! the page cannot read it, and `Authorization: Bearer` is also accepted for
//! clients that are not a browser.

use crate::error::ApiError;
use crate::state::App;
use axum::extract::{FromRequestParts, State};
use axum::http::header::{AUTHORIZATION, COOKIE, SET_COOKIE};
use axum::http::request::Parts;
use axum::http::HeaderMap;
use axum::Json;
use louver_cloud::credentials::{hash_password, new_token, token_hash, verify_password};
use louver_cloud::{CloudError, Result};
use serde::{Deserialize, Serialize};

pub const COOKIE_NAME: &str = "louver_session";
const SESSION_DAYS: i64 = 30;

/// The authenticated user's id. Nothing else — a handler that wants the row
/// reads it, so a stale copy cannot be carried around.
#[derive(Debug, Clone)]
pub struct Caller(pub String);

impl FromRequestParts<App> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, app: &App) -> std::result::Result<Self, Self::Rejection> {
        let token = token_from(&parts.headers).ok_or(CloudError::BadCredentials)?;
        let hash = token_hash(&token);
        let db = app.db.clone();
        // A token that resolves to nothing is a session problem, not a missing
        // resource: the caller is told to sign in, never that some id exists.
        let id =
            crate::blocking(move || db.user_for_token(&hash).map_err(|_| CloudError::BadCredentials)).await?;
        Ok(Self(id))
    }
}

fn token_from(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(rest) = v.strip_prefix("Bearer ") {
            let rest = rest.trim();
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    for raw in headers.get_all(COOKIE) {
        let Ok(text) = raw.to_str() else { continue };
        for pair in text.split(';') {
            let pair = pair.trim();
            if let Some(v) = pair.strip_prefix(&format!("{COOKIE_NAME}=")) {
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// When the session cookie carries `Secure`.
///
/// This exists because "always Secure" silently breaks the most common first
/// deployment there is. A `Secure` cookie is never returned by a browser over
/// plain `http://`, so a server reached at `http://<ip>:8080` accepts the
/// password, sets a cookie the browser throws away, and answers 401 to
/// everything after it. The login page then reappears with no error — the worst
/// kind of failure, because nothing looks broken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CookiePolicy {
    /// `Secure` when the request arrived over HTTPS, and not when it did not.
    /// The default: correct behind a TLS proxy, and working over a bare IP.
    Auto,
    /// Always `Secure`. What to set once HTTPS is in front of this.
    Always,
    /// Never `Secure`. Plain HTTP only.
    Never,
}

impl CookiePolicy {
    pub fn from_env() -> Self {
        // The older flag keeps working; it was documented before this existed.
        if std::env::var("LOUVER_INSECURE_COOKIES").as_deref() == Ok("1") {
            return Self::Never;
        }
        match std::env::var("LOUVER_COOKIE_SECURE").unwrap_or_default().trim().to_lowercase().as_str() {
            "always" | "1" | "true" => Self::Always,
            "never" | "0" | "false" => Self::Never,
            _ => Self::Auto,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Never => "never",
        }
    }
}

/// Did this request reach us over TLS?
///
/// Read from the proxy's own headers, because the server itself always speaks
/// plain HTTP — TLS is terminated by Caddy, nginx or whatever is in front.
fn arrived_over_https(headers: &HeaderMap) -> bool {
    if let Some(proto) = headers.get("x-forwarded-proto").and_then(|v| v.to_str().ok()) {
        // A chain of proxies appends, so the first entry is the client's.
        if proto.split(',').next().map(str::trim).is_some_and(|p| p.eq_ignore_ascii_case("https")) {
            return true;
        }
    }
    // RFC 7239, for proxies that send the standard header instead.
    headers
        .get("forwarded")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|f| f.to_ascii_lowercase().contains("proto=https"))
}

pub fn secure_flag(policy: CookiePolicy, headers: &HeaderMap) -> bool {
    match policy {
        CookiePolicy::Always => true,
        CookiePolicy::Never => false,
        CookiePolicy::Auto => arrived_over_https(headers),
    }
}

/// Is the browser somewhere that plain HTTP is not a real exposure?
///
/// `localhost` is a secure context in every current browser — a `Secure` cookie
/// is both accepted and returned there — and nothing leaves the machine.
fn host_is_local(headers: &HeaderMap) -> bool {
    let host = headers.get(axum::http::header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    let name = host.split(':').next().unwrap_or("");
    matches!(name, "localhost" | "127.0.0.1" | "::1" | "[::1]") || name.is_empty()
}

/// A session cookie the page's JavaScript cannot read.
fn session_cookie(token: &str, headers: &HeaderMap) -> String {
    let mut c = format!(
        "{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        SESSION_DAYS * 24 * 60 * 60
    );
    if secure_flag(CookiePolicy::from_env(), headers) {
        c.push_str("; Secure");
    } else if !host_is_local(headers) {
        // Said once per sign-in rather than hidden in a doc: the token just
        // travelled in the clear to something that is not this machine.
        eprintln!(
            "[louver] 경고: 세션 쿠키가 암호화되지 않은 연결로 전달되었습니다. \
             공개 서버라면 HTTPS를 앞에 두고 LOUVER_COOKIE_SECURE=always 를 설정하세요."
        );
    }
    c
}

fn cleared_cookie() -> String {
    format!("{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0")
}

#[derive(Deserialize)]
pub struct Credentials {
    pub email: String,
    pub password: String,
}

/// What a browser is told about itself. No token, no password hash.
#[derive(Serialize)]
pub struct Me {
    pub id: String,
    pub email: String,
    pub plan_id: String,
}

type Answer = std::result::Result<(HeaderMap, Json<Me>), ApiError>;

pub async fn register(State(app): State<App>, headers: HeaderMap, Json(body): Json<Credentials>) -> Answer {
    let (me, token) = crate::blocking(move || {
        check_password(&body.password)?;
        let email = normalize_email(&body.email)?;
        let hash = hash_password(&body.password)?;
        let plan = std::env::var("LOUVER_DEFAULT_PLAN").unwrap_or_else(|_| "basic".into());
        let user = app.db.create_user(&email, &hash, &plan)?;
        let (token, hash) = new_token()?;
        app.db.create_auth_session(&user.id, &hash, SESSION_DAYS)?;
        Ok((Me { id: user.id, email: user.email, plan_id: user.plan_id }, token))
    })
    .await?;
    Ok((with_cookie(session_cookie(&token, &headers)), Json(me)))
}

pub async fn login(State(app): State<App>, headers: HeaderMap, Json(body): Json<Credentials>) -> Answer {
    let (me, token) = crate::blocking(move || {
        let email = normalize_email(&body.email)?;
        // The same error for an unknown address as for a wrong password, so the
        // endpoint cannot be used to find out who has an account.
        let (user_id, stored) = app.db.password_hash_for(&email)?;
        if !verify_password(&stored, &body.password) {
            return Err(CloudError::BadCredentials);
        }
        let user = app.db.user(&user_id)?;
        let (token, hash) = new_token()?;
        app.db.create_auth_session(&user.id, &hash, SESSION_DAYS)?;
        Ok((Me { id: user.id, email: user.email, plan_id: user.plan_id }, token))
    })
    .await?;
    Ok((with_cookie(session_cookie(&token, &headers)), Json(me)))
}

/// Ends this session server-side, not only in the browser.
pub async fn logout(State(app): State<App>, headers: HeaderMap) -> std::result::Result<HeaderMap, ApiError> {
    if let Some(token) = token_from(&headers) {
        let hash = token_hash(&token);
        crate::blocking(move || app.db.delete_auth_session(&hash)).await?;
    }
    Ok(with_cookie(cleared_cookie()))
}

pub async fn me(State(app): State<App>, Caller(user_id): Caller) -> std::result::Result<Json<Me>, ApiError> {
    let u = crate::blocking(move || app.db.user(&user_id)).await?;
    Ok(Json(Me { id: u.id, email: u.email, plan_id: u.plan_id }))
}

pub async fn subscription(
    State(app): State<App>,
    Caller(user_id): Caller,
) -> std::result::Result<Json<louver_cloud::Subscription>, ApiError> {
    Ok(Json(crate::blocking(move || app.db.subscription(&user_id)).await?))
}

fn with_cookie(value: String) -> HeaderMap {
    let mut h = HeaderMap::new();
    if let Ok(v) = value.parse() {
        h.insert(SET_COOKIE, v);
    }
    h
}

fn normalize_email(raw: &str) -> Result<String> {
    let e = raw.trim().to_lowercase();
    if e.len() < 3 || !e.contains('@') {
        return Err(CloudError::Invalid("이메일 주소를 확인해 주세요".into()));
    }
    Ok(e)
}

fn check_password(p: &str) -> Result<()> {
    if p.chars().count() < 10 {
        return Err(CloudError::Invalid("비밀번호는 10자 이상이어야 합니다".into()));
    }
    Ok(())
}

#[cfg(test)]
mod cookie_policy_tests {
    use super::*;
    use axum::http::header::HeaderValue;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn auto_follows_the_proxy_and_therefore_works_over_a_bare_ip() {
        let plain = headers(&[("host", "203.0.113.10:8080")]);
        let tls = headers(&[("host", "live.example.com"), ("x-forwarded-proto", "https")]);

        // No TLS in front: a Secure cookie would be discarded by the browser
        // and every request after the login would answer 401.
        assert!(!secure_flag(CookiePolicy::Auto, &plain));
        assert!(secure_flag(CookiePolicy::Auto, &tls));

        // A chain of proxies appends; the client's entry comes first.
        let chained = headers(&[("x-forwarded-proto", "https, http")]);
        assert!(secure_flag(CookiePolicy::Auto, &chained));

        // And the RFC 7239 spelling, for proxies that send that instead.
        let forwarded = headers(&[("forwarded", "for=192.0.2.1;proto=https;by=proxy")]);
        assert!(secure_flag(CookiePolicy::Auto, &forwarded));
    }

    #[test]
    fn always_and_never_ignore_the_request() {
        let plain = headers(&[("host", "203.0.113.10:8080")]);
        assert!(secure_flag(CookiePolicy::Always, &plain));
        assert!(!secure_flag(CookiePolicy::Never, &headers(&[("x-forwarded-proto", "https")])));
    }

    #[test]
    fn a_cookie_is_httponly_and_samesite_whatever_the_policy() {
        let c = session_cookie("t", &headers(&[("host", "localhost:8080")]));
        assert!(c.contains("HttpOnly"), "{c}");
        assert!(c.contains("SameSite=Strict"), "{c}");
        assert!(c.contains("Path=/"), "{c}");
    }

    #[test]
    fn localhost_is_not_treated_as_an_exposure() {
        assert!(host_is_local(&headers(&[("host", "localhost:8080")])));
        assert!(host_is_local(&headers(&[("host", "127.0.0.1:8080")])));
        assert!(!host_is_local(&headers(&[("host", "203.0.113.10:8080")])));
    }
}
