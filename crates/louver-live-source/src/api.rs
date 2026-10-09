//! The HTTP surface.
//!
//! Five routes and one rule: **nothing here reads an identity from a request
//! body.** The caller is whoever the token says, and the token is either minted
//! by the cookie handshake or rejected. There is no `user_id` field anywhere in
//! this module's input types.
//!
//! Errors map to status codes so a client can act on them without parsing
//! Korean: 400 for a bad request, 401 for no usable credential, 404 for
//! somebody else's job as well as a missing one, 409 for an id in use, 429 for
//! the concurrency ceiling, 502 for a source this worker could not reach.
//!
//! What is deliberately absent:
//!
//!  * no route that takes or returns a destination URL;
//!  * no route that takes a file path;
//!  * no route that reports another user's existence;
//!  * no CORS headers, so a page on another origin cannot call this at all.
//!    The beta page is served from this same origin through Caddy, so it needs
//!    none — and adding them would undo the `SameSite=Strict` cookie's work.

use crate::auth::{bearer, Identity, IdentitySource};
use crate::error::{ErrorKind, LiveSourceError};
use crate::jobs::{JobView, NewJob, Registry};
use crate::token::Signer;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Serialize;
use std::sync::Arc;

/// Everything a handler needs.
#[derive(Clone)]
pub struct Api {
    pub jobs: Arc<Registry>,
    pub signer: Arc<Signer>,
    pub identity: Arc<dyn IdentitySource>,
}

/// The body of every failure. One shape, so a client has one thing to parse.
#[derive(Debug, Serialize)]
struct ErrBody {
    error: &'static str,
    message: String,
}

struct Fail(LiveSourceError);

impl IntoResponse for Fail {
    fn into_response(self) -> Response {
        let status = match self.0.kind {
            ErrorKind::Invalid => StatusCode::BAD_REQUEST,
            ErrorKind::Unauthorized => StatusCode::UNAUTHORIZED,
            // Reported as 404: a 403 would confirm the job exists.
            ErrorKind::Forbidden | ErrorKind::NotFound => StatusCode::NOT_FOUND,
            ErrorKind::Conflict => StatusCode::CONFLICT,
            ErrorKind::Limit => StatusCode::TOO_MANY_REQUESTS,
            ErrorKind::NotLive | ErrorKind::Unavailable | ErrorKind::ResolverFailed => {
                StatusCode::BAD_GATEWAY
            }
            ErrorKind::Ffmpeg => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = ErrBody { error: self.0.kind.as_str(), message: self.0.message };
        (status, Json(body)).into_response()
    }
}

impl From<LiveSourceError> for Fail {
    fn from(e: LiveSourceError) -> Self {
        Fail(e)
    }
}

type Out<T> = std::result::Result<Json<T>, Fail>;

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Who is calling, from the `Authorization` header and nothing else.
fn caller(api: &Api, headers: &HeaderMap) -> std::result::Result<Identity, Fail> {
    let raw = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| Fail(LiveSourceError::unauthorized("인증이 필요합니다.")))?;
    let token = bearer(raw).ok_or_else(|| Fail(LiveSourceError::unauthorized("인증이 필요합니다.")))?;
    Identity::from_token(&api.signer, token, now()).map_err(Fail)
}

#[derive(Debug, Serialize)]
pub struct SessionBody {
    pub token: String,
    pub expires_in: i64,
    /// What this user may send to, by name. Never a URL.
    pub destinations: Vec<String>,
    pub max_concurrent: usize,
    pub max_width: u32,
    pub max_height: u32,
}

/// `POST /api/live-source/session` — the one endpoint that reads the cookie.
///
/// It is used once per token lifetime. Everything else takes the bearer token,
/// and Caddy strips `Cookie` from those routes, so the session cookie is not
/// handed to this machine on every request.
async fn session(State(api): State<Api>, headers: HeaderMap) -> Out<SessionBody> {
    let cookie = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| Fail(LiveSourceError::unauthorized("로그인이 필요합니다.")))?
        .to_string();
    let api2 = api.clone();
    // Blocking: the handshake calls production over HTTP.
    let (_id, token) = tokio::task::spawn_blocking(move || {
        Identity::from_production(api2.identity.as_ref(), &cookie, &api2.signer, now())
    })
    .await
    .map_err(|_| Fail(LiveSourceError::unauthorized("로그인 확인이 중단되었습니다.")))?
    .map_err(Fail)?;

    let (w, h) = crate::jobs::output_cap();
    Ok(Json(SessionBody {
        token,
        expires_in: crate::token::TOKEN_TTL_SECS,
        destinations: api.jobs.settings().media.destination_names(),
        max_concurrent: api.jobs.settings().limits.max_concurrent,
        max_width: w,
        max_height: h,
    }))
}

#[derive(Debug, Serialize)]
pub struct JobsBody {
    pub jobs: Vec<JobView>,
    pub running: usize,
    pub max_concurrent: usize,
}

async fn list_jobs(State(api): State<Api>, headers: HeaderMap) -> Out<JobsBody> {
    let who = caller(&api, &headers)?;
    Ok(Json(JobsBody {
        jobs: api.jobs.list(&who.user_id),
        running: api.jobs.running_count(),
        max_concurrent: api.jobs.settings().limits.max_concurrent,
    }))
}

/// Read a JSON body **after** the caller has been authenticated.
///
/// Taking `Json<T>` in the signature would have axum deserialise before this
/// module's code runs, so a request with no credential and a malformed body
/// answered 422 — the server had already parsed attacker-supplied JSON and the
/// reply told them their body was the problem rather than their absence of a
/// token. Taking `Bytes` and decoding here keeps the order: authenticate, then
/// parse.
fn body_json<T: serde::de::DeserializeOwned>(raw: &axum::body::Bytes) -> std::result::Result<T, Fail> {
    serde_json::from_slice(raw)
        .map_err(|e| Fail(LiveSourceError::invalid(format!("요청 본문을 해석할 수 없습니다: {e}"))))
}

async fn create_job(
    State(api): State<Api>,
    headers: HeaderMap,
    raw: axum::body::Bytes,
) -> std::result::Result<(StatusCode, Json<JobView>), Fail> {
    let who = caller(&api, &headers)?;
    let req: NewJob = body_json(&raw)?;
    let jobs = Arc::clone(&api.jobs);
    let (owner, plan) = (who.user_id.clone(), who.plan.clone());
    // Blocking: this writes files and starts a process.
    let view = tokio::task::spawn_blocking(move || jobs.create(&owner, &plan, &req))
        .await
        .map_err(|_| Fail(LiveSourceError::ffmpeg("작업 생성이 중단되었습니다.")))?
        .map_err(Fail)?;
    Ok((StatusCode::CREATED, Json(view)))
}

async fn get_job(State(api): State<Api>, headers: HeaderMap, Path(id): Path<String>) -> Out<JobView> {
    let who = caller(&api, &headers)?;
    api.jobs.get(&who.user_id, &id).map(Json).map_err(Fail)
}

async fn cancel_job(State(api): State<Api>, headers: HeaderMap, Path(id): Path<String>) -> Out<JobView> {
    let who = caller(&api, &headers)?;
    let jobs = Arc::clone(&api.jobs);
    let owner = who.user_id.clone();
    tokio::task::spawn_blocking(move || jobs.cancel(&owner, &id))
        .await
        .map_err(|_| Fail(LiveSourceError::ffmpeg("작업 취소가 중단되었습니다.")))?
        .map(Json)
        .map_err(Fail)
}

#[derive(Debug, serde::Deserialize)]
pub struct CheckReq {
    pub source_url: String,
}

#[derive(Debug, Serialize)]
pub struct CheckBody {
    pub ok: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

/// `POST /api/live-source/check` — is this address usable?
///
/// Authenticated, because it makes this worker open a network connection and an
/// unauthenticated version of that is a probe anyone could aim.
async fn check(State(api): State<Api>, headers: HeaderMap, raw: axum::body::Bytes) -> Out<CheckBody> {
    let _who = caller(&api, &headers)?;
    let req: CheckReq = body_json(&raw)?;
    // The same boundary check `POST /jobs` makes, and for the same reason: this
    // endpoint makes the server open an address, so an unusable one is a 400
    // before any subprocess exists.
    let kind = crate::jobs::validate_source(&req.source_url).map_err(Fail)?;
    let video_id = crate::resolver::video_id(&req.source_url);

    let out = match kind {
        crate::resolver::SourceKind::YoutubeWatch => {
            let resolver = Arc::clone(api.jobs.resolver());
            let url = req.source_url.clone();
            tokio::task::spawn_blocking(move || resolver.resolve(&url))
                .await
                .map_err(|_| Fail(LiveSourceError::unavailable("확인이 중단되었습니다.")))?
                .map(|r| (r.width, r.height))
        }
        // A direct stream is probed with production's own ffprobe check, which
        // revalidates the address and reports the codec and size.
        crate::resolver::SourceKind::DirectStream => {
            let tools = api.jobs.settings().tools.clone();
            let url = req.source_url.clone();
            tokio::task::spawn_blocking(move || louver_cloud::cctv::test_connection(&tools, &url))
                .await
                .map_err(|_| Fail(LiveSourceError::unavailable("확인이 중단되었습니다.")))?
                .map_err(|e| LiveSourceError::unavailable(e.to_string()))
                .and_then(|c| {
                    if c.ok {
                        Ok((c.width.map(|w| w as u32), c.height.map(|h| h as u32)))
                    } else {
                        Err(LiveSourceError::unavailable(c.message))
                    }
                })
        }
    };

    match out {
        Ok((width, height)) => Ok(Json(CheckBody {
            ok: true,
            message: format!(
                "확인됨{}",
                match (width, height) {
                    (Some(w), Some(h)) => format!(" · 원본 {w}×{h}"),
                    _ => String::new(),
                }
            ),
            video_id,
            width,
            height,
        })),
        // A failure here is information the user needs, and these messages are
        // already written for them.
        Err(e) => Ok(Json(CheckBody { ok: false, message: e.message, video_id, width: None, height: None })),
    }
}

#[derive(Debug, Serialize)]
struct Health {
    ok: bool,
    running: usize,
    max_concurrent: usize,
}

/// Unauthenticated on purpose, and it says nothing about any user.
async fn health(State(api): State<Api>) -> Json<Health> {
    Json(Health {
        ok: true,
        running: api.jobs.running_count(),
        max_concurrent: api.jobs.settings().limits.max_concurrent,
    })
}

impl Api {
    /// The router, without the static beta page, so tests can drive the API
    /// alone.
    pub fn router(self) -> Router {
        Router::new()
            .route("/api/live-source/health", get(health))
            .route("/api/live-source/session", post(session))
            .route("/api/live-source/check", post(check))
            .route("/api/live-source/jobs", get(list_jobs).post(create_job))
            .route("/api/live-source/jobs/{id}", get(get_job))
            .route("/api/live-source/jobs/{id}", delete(cancel_job))
            .with_state(self)
    }

    /// The router plus `/beta/`, which is what the binary serves.
    pub fn router_with_beta(self, beta_dir: &std::path::Path) -> Router {
        let index = beta_dir.join("index.html");
        self.router().nest_service(
            "/beta",
            tower_http::services::ServeDir::new(beta_dir)
                .fallback(tower_http::services::ServeFile::new(index)),
        )
    }
}
