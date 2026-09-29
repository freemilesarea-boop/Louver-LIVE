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
use louver_cloud::db::{clean_name, Signup};
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

/// What signup sends.
///
/// `deny_unknown_fields` is the reason a request cannot smuggle in a plan. It is
/// belt as well as braces — nothing below reads a plan from the body, so an
/// extra field would be ignored anyway — but "ignored" is a property of today's
/// code, and a rejection is a property of the type. A body carrying
/// `"plan_id": "business"` now fails outright instead of quietly succeeding and
/// leaving the reader of a future diff to work out whether it mattered.
///
/// There is no `password_confirmation`: the two boxes have to match in the
/// browser, and sending the password twice would only mean one more copy of it
/// in one more log the server does not control.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registration {
    /// Defaulted so that a body with no `name` reaches [`clean_name`] and gets
    /// "이름을 입력해주세요." — serde's own "missing field `name`" is English, and
    /// a 422 carrying it would be the one error on this form a user could not
    /// read.
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub password: String,
}

/// What a browser is told about itself. No token, no password hash.
///
/// `name` is additive and may be `null`: every account that existed before
/// signup asked for one has no name, and the UI falls back to the email.
#[derive(Serialize)]
pub struct Me {
    pub id: String,
    pub email: String,
    /// `user` or `admin`. The browser reads it only to decide whether to offer
    /// the console; every admin route checks the database for itself.
    pub role: String,
    pub plan_id: String,
    pub name: Option<String>,
}

impl Me {
    fn of(u: louver_cloud::User) -> Self {
        Self { id: u.id, email: u.email, role: u.role, plan_id: u.plan_id, name: u.name }
    }
}

/// Which version of the terms a signup today agrees to.
///
/// A constant rather than a column's default, so that changing the documents and
/// re-asking existing users is a matter of bumping this and comparing it against
/// `users.terms_version`.
pub const TERMS_VERSION: &str = "2026-09-27";

type Answer = std::result::Result<(HeaderMap, Json<Me>), ApiError>;

/// Make an account, and sign in with it.
///
/// Every check here is also made in the browser. That is not duplication to be
/// removed: the browser's copy is there to answer quickly, and this copy is the
/// one that decides, because a request does not have to come from the form.
pub async fn register(
    State(app): State<App>,
    peer: crate::throttle::Peer,
    headers: HeaderMap,
    Json(body): Json<Registration>,
) -> Answer {
    guard("register", &headers, peer)?;
    let (me, token) = crate::blocking(move || {
        let name = clean_name(&body.name)?;
        let email = normalize_email(&body.email)?;
        check_password(&body.password)?;
        let password_hash = hash_password(&body.password)?;

        // No plan argument at all, by design: a new account is unsubscribed, and
        // choosing a plan is what a verified payment does. That is a stronger
        // guarantee than validating one would be — signing up cannot grant an
        // entitlement because there is no parameter through which it could. See
        // `CloudDb::register_user`.
        //
        // Agreeing to the terms is a condition of reaching this endpoint at all,
        // and the moment it happened is the server's clock's to record — a
        // client-supplied timestamp could claim any date it liked.
        let user = app.db.register_user(&Signup {
            name: &name,
            email: &email,
            password_hash: &password_hash,
            terms_version: TERMS_VERSION,
        })?;

        let (token, token_hash) = new_token()?;
        app.db.create_auth_session(&user.id, &token_hash, SESSION_DAYS)?;
        Ok((Me::of(user), token))
    })
    .await?;
    Ok((with_cookie(session_cookie(&token, &headers)), Json(me)))
}

pub async fn login(
    State(app): State<App>,
    peer: crate::throttle::Peer,
    headers: HeaderMap,
    Json(body): Json<Credentials>,
) -> Answer {
    // Before the hash, not after: the point is to not spend the CPU.
    guard("login", &headers, peer)?;
    let (me, token) = crate::blocking(move || {
        let email = normalize_email(&body.email)?;
        // The same error for an unknown address as for a wrong password, so the
        // endpoint cannot be used to find out who has an account.
        let (user_id, stored) = app.db.password_hash_for(&email)?;
        if !verify_password(&stored, &body.password) {
            return Err(CloudError::BadCredentials);
        }
        let user = app.db.user(&user_id)?;
        // A switched-off account is refused *after* the password check, so this
        // route still cannot be used to find out which addresses exist.
        if user.is_disabled() {
            return Err(CloudError::Disabled);
        }
        let (token, token_hash) = new_token()?;
        app.db.create_auth_session(&user.id, &token_hash, SESSION_DAYS)?;
        Ok((Me::of(user), token))
    })
    .await?;
    Ok((with_cookie(session_cookie(&token, &headers)), Json(me)))
}

/// Refuse a caller who is hammering this route.
///
/// Placed in front of the password hash rather than behind it, because the cost
/// being defended is the hash itself. A request with no socket behind it — the
/// tests, driving the router in process — is not counted; see `throttle`.
fn guard(
    route: &'static str,
    headers: &HeaderMap,
    peer: crate::throttle::Peer,
) -> std::result::Result<(), ApiError> {
    let Some(who) = crate::throttle::caller_key(headers, peer.0) else {
        return Ok(());
    };
    match crate::throttle::shared().check(route, &who) {
        Ok(()) => Ok(()),
        Err(wait) => {
            // The address is in the server's log, not in the answer.
            eprintln!("[louver] {route}: 시도가 너무 잦아 거부했습니다 ({}초 후 재시도)", wait.as_secs());
            Err(CloudError::TooManyAttempts.into())
        }
    }
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
    Ok(Json(Me::of(u)))
}

/// What this account is entitled to. The front end reads `active` and `plan`.
pub async fn subscription(
    State(app): State<App>,
    Caller(user_id): Caller,
) -> std::result::Result<Json<louver_cloud::Subscription>, ApiError> {
    Ok(Json(crate::blocking(move || app.db.subscription(&user_id)).await?))
}

/// The plans on offer, for the pricing page.
///
/// Unauthenticated: a price list is public, and requiring a session to read one
/// would mean nobody could see what the service costs before signing up.
///
/// The prices and the concurrency figures come from here and are never written
/// into the front end, so there is exactly one place they can disagree with what
/// the server charges: none.
pub async fn plans(State(app): State<App>) -> std::result::Result<Json<Vec<louver_cloud::Plan>>, ApiError> {
    Ok(Json(crate::blocking(move || app.db.plans_for_sale()).await?))
}

fn with_cookie(value: String) -> HeaderMap {
    let mut h = HeaderMap::new();
    if let Ok(v) = value.parse() {
        h.insert(SET_COOKIE, v);
    }
    h
}

/// Lower-cased and trimmed, which is also how it is stored and compared.
///
/// Deliberately not a full RFC 5322 parser. The only thing an address has to be
/// here is something a person could receive mail at and type again the same way;
/// whether it actually exists is a question only sending mail can answer, and
/// that is a later step. What this does refuse is the shapes that are certainly
/// mistakes — no `@`, nothing before or after it, a domain with no dot, spaces
/// in the middle — because each of those is a typo the user can fix now.
fn normalize_email(raw: &str) -> Result<String> {
    let e = raw.trim().to_lowercase();
    let bad = || CloudError::Invalid("올바른 이메일 주소를 입력해주세요.".into());
    if e.chars().count() > 254 || e.contains(char::is_whitespace) {
        return Err(bad());
    }
    let (local, domain) = e.split_once('@').ok_or_else(bad)?;
    if local.is_empty() || domain.contains('@') {
        return Err(bad());
    }
    // A dot with something either side of it. `user@localhost` is a valid
    // address in the abstract and never one somebody signs up with.
    match domain.rsplit_once('.') {
        Some((host, tld)) if !host.is_empty() && tld.chars().count() >= 2 => Ok(e),
        _ => Err(bad()),
    }
}

fn check_password(p: &str) -> Result<()> {
    // Unchanged: 10 characters, counted as characters. The hashing behind it is
    // not touched by any of this.
    if p.chars().count() < MIN_PASSWORD_CHARS {
        return Err(CloudError::Invalid(format!("비밀번호는 {MIN_PASSWORD_CHARS}자 이상이어야 합니다.")));
    }
    Ok(())
}

/// The floor the server enforces. The form shows the same number.
pub const MIN_PASSWORD_CHARS: usize = 10;

#[cfg(test)]
mod validation_tests {
    use super::*;

    #[test]
    fn an_email_has_to_look_like_one() {
        assert_eq!(normalize_email("  Me@Example.COM ").unwrap(), "me@example.com");
        for bad in [
            "",
            "me",
            "me@",
            "@example.com",
            "me@example",          // no dot in the domain
            "me@example.c",        // a one-letter TLD is a typo
            "me@.com",             // nothing before the dot
            "me @example.com",     // a space is always a mistake
            "a@b.com\nbcc: x@y.z", // a header injection attempt is also whitespace
        ] {
            assert!(normalize_email(bad).is_err(), "{bad:?} was accepted");
        }
        // And the message is the one the form shows, not a database word.
        let e = normalize_email("nope").unwrap_err().to_string();
        assert!(e.contains("올바른 이메일"), "{e}");
    }

    #[test]
    fn a_name_is_trimmed_bounded_and_never_a_control_character() {
        assert_eq!(clean_name("  홍길동  ").unwrap(), "홍길동");
        assert_eq!(clean_name("Ada Lovelace").unwrap(), "Ada Lovelace");
        // A name that would break a log line or a terminal is cleaned, not stored.
        assert_eq!(clean_name("홍\u{7}길동\n").unwrap(), "홍길동");

        assert!(clean_name("").is_err());
        assert!(clean_name("      ").is_err());
        assert!(clean_name("\n\t").is_err());
        // Counted in characters: 60 Korean characters is fine, 61 is not.
        assert!(clean_name(&"가".repeat(louver_cloud::db::MAX_NAME_CHARS)).is_ok());
        assert!(clean_name(&"가".repeat(louver_cloud::db::MAX_NAME_CHARS + 1)).is_err());
    }

    #[test]
    fn the_password_floor_is_the_one_the_form_shows() {
        assert!(check_password(&"a".repeat(MIN_PASSWORD_CHARS)).is_ok());
        assert!(check_password(&"a".repeat(MIN_PASSWORD_CHARS - 1)).is_err());
        // Characters, not bytes: nine Korean characters is still nine.
        assert!(check_password("비밀번호짧아요").is_err());
    }

    #[test]
    fn a_registration_body_cannot_smuggle_in_a_plan() {
        let ok: Registration =
            serde_json::from_str(r#"{"name":"홍길동","email":"a@b.com","password":"0123456789"}"#).unwrap();
        assert_eq!(ok.name, "홍길동");

        // §12: not merely ignored — refused, so that a future reader cannot be
        // left wondering whether some code path started honouring it.
        for smuggled in [
            r#"{"name":"n","email":"a@b.com","password":"0123456789","plan_id":"business"}"#,
            r#"{"name":"n","email":"a@b.com","password":"0123456789","plan":"business"}"#,
            r#"{"name":"n","email":"a@b.com","password":"0123456789","is_admin":true}"#,
            r#"{"name":"n","email":"a@b.com","password":"0123456789","terms_accepted_at":"2001-01-01"}"#,
        ] {
            assert!(
                serde_json::from_str::<Registration>(smuggled).is_err(),
                "accepted an extra field: {smuggled}"
            );
        }
    }
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
