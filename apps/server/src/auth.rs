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

/// A session cookie the page's JavaScript cannot read.
///
/// `Secure` is on unless a developer asks for plain HTTP explicitly, because
/// the safe default has to be the one you get by doing nothing.
fn session_cookie(token: &str) -> String {
    let mut c = format!(
        "{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        SESSION_DAYS * 24 * 60 * 60
    );
    if std::env::var("LOUVER_INSECURE_COOKIES").as_deref() != Ok("1") {
        c.push_str("; Secure");
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

pub async fn register(State(app): State<App>, Json(body): Json<Credentials>) -> Answer {
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
    Ok((with_cookie(session_cookie(&token)), Json(me)))
}

pub async fn login(State(app): State<App>, Json(body): Json<Credentials>) -> Answer {
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
    Ok((with_cookie(session_cookie(&token)), Json(me)))
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
