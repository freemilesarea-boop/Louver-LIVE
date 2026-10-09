//! The HTTP surface.
//!
//! Every request passes the same gauntlet, in this order, and the order is the
//! design:
//!
//!  1. **[`crate::gate`]** — `X-Louver-Gate`, the secret Caddy adds to
//!     everything it proxies here. A request that reached this port some other
//!     way stops at a 403 having told the attacker nothing. Not
//!     authentication: see step 3.
//!
//!     Enforced as a layer around the whole router — the beta page included —
//!     so a route added later cannot forget it, and checked again inside
//!     [`enter`] so a route moved out from under that layer would fail closed
//!     rather than open.
//!  2. **No `Cookie`** — on every route except `POST …/session`. Caddy strips
//!     it, and this refuses it as well, so a misconfigured proxy that started
//!     forwarding the session cookie to this machine would fail loudly on the
//!     next request rather than quietly hand this worker a credential it has no
//!     use for. The raw header is never logged or echoed.
//!  3. **Identity** — the bearer token on everything, or, on `POST …/session`
//!     alone, the session cookie once, with an exact `Origin` check. **Nothing
//!     here reads an identity from a request body.** There is no `user_id`
//!     field anywhere in this module's input types.
//!  4. **Ownership** — the caller's id goes into every registry, destination
//!     and media call, so a name or an id belonging to someone else resolves to
//!     nothing rather than to their data.
//!
//! Errors map to status codes so a client can act on them without parsing
//! Korean: 400 for a bad request, 401 for no usable credential, 403 for a
//! request that did not come through the front door, 404 for somebody else's
//! job as well as a missing one, 409 for an id in use, 413 for an upload over
//! the cap, 429 for a concurrency ceiling, 502 for a source this worker could
//! not reach.
//!
//! What is deliberately absent:
//!
//!  * no route that takes or returns a destination URL;
//!  * no route that takes a file path — media is named, and resolved inside the
//!    caller's own directory;
//!  * no route that reports another user's existence;
//!  * no CORS headers, so a page on another origin cannot read a reply from
//!    here at all. The beta page is served from this same origin through Caddy,
//!    so it needs none — and adding them would undo the `SameSite=Strict`
//!    cookie's work.

use crate::auth::{bearer, Identity, IdentitySource};
use crate::error::{ErrorKind, LiveSourceError};
use crate::gate::{Gate, GATE_HEADER};
use crate::jobs::{JobView, NewJob, Registry};
use crate::media::StoredMedia;
use crate::origin::AllowedOrigins;
use crate::token::Signer;
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::middleware::Next;
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
    /// The shared secret with the proxy. Not optional: there is no constructor
    /// for an open gate, so this API cannot be run without one.
    pub gate: Arc<Gate>,
    /// Where a session may be started from.
    pub origins: Arc<AllowedOrigins>,
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
            // 403, and it means one thing only: this request did not come
            // through the proxy, or it carried a cookie it should not have.
            // Nothing in this crate reports *ownership* as `Forbidden` —
            // somebody else's job is `NotFound`, because a 403 there would
            // confirm the job exists.
            ErrorKind::Forbidden => StatusCode::FORBIDDEN,
            ErrorKind::NotFound => StatusCode::NOT_FOUND,
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

/// Step 1: did this come through the proxy?
fn gate(api: &Api, headers: &HeaderMap) -> std::result::Result<(), Fail> {
    let got = headers.get(GATE_HEADER).and_then(|v| v.to_str().ok());
    api.gate.check(got).map_err(Fail)
}

/// The same check as a layer, around every route including the static page.
///
/// A per-handler call can be forgotten by whoever adds the next route; a layer
/// cannot. Both exist: this one is the enforcement, and the call inside
/// [`enter`] means a route that ever ends up outside this layer still refuses
/// rather than opens.
async fn gate_layer(State(api): State<Api>, request: Request, next: Next) -> Response {
    if let Err(fail) = gate(&api, request.headers()) {
        return fail.into_response();
    }
    next.run(request).await
}

/// Step 2: a `Cookie` header is refused everywhere but the handshake.
///
/// The value is looked at only to know that it is there. It is not parsed, not
/// copied into the error, and not logged — the one place in this crate that
/// reads a cookie is [`crate::auth::session_cookie_only`], on the one route
/// that needs it.
fn no_cookie(headers: &HeaderMap) -> std::result::Result<(), Fail> {
    if headers.contains_key(axum::http::header::COOKIE) {
        return Err(Fail(LiveSourceError::new(ErrorKind::Forbidden, "이 요청에는 쿠키를 보낼 수 없습니다.")));
    }
    Ok(())
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

/// Steps 1–3 for every route but the handshake: gate, no cookie, bearer token.
///
/// One function so that none of them can be forgotten on a route added later.
fn enter(api: &Api, headers: &HeaderMap) -> std::result::Result<Identity, Fail> {
    gate(api, headers)?;
    no_cookie(headers)?;
    caller(api, headers)
}

#[derive(Debug, Serialize)]
pub struct SessionBody {
    pub token: String,
    pub expires_in: i64,
    /// What **this** user may send to, by name. Never a URL, and never another
    /// user's name.
    pub destinations: Vec<String>,
    pub max_concurrent: usize,
    pub max_per_user: usize,
    pub max_width: u32,
    pub max_height: u32,
    pub max_upload_bytes: u64,
    pub max_storage_bytes: u64,
    pub allowed_extensions: Vec<&'static str>,
}

/// `POST /api/live-source/session` — the one endpoint that reads the cookie.
///
/// It is used once per token lifetime, and the token lives five minutes. The
/// `Origin` check is here and nowhere else, because this is the only route
/// where a cross-site request could achieve anything: it is the only one that
/// uses a credential the browser attaches on its own.
async fn session(State(api): State<Api>, headers: HeaderMap) -> Out<SessionBody> {
    gate(&api, &headers)?;
    // Before the cookie is read: a request from the wrong page does not get as
    // far as having its credential used.
    api.origins.check(headers.get(axum::http::header::ORIGIN).and_then(|v| v.to_str().ok())).map_err(Fail)?;

    let cookie = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| Fail(LiveSourceError::unauthorized("로그인이 필요합니다.")))?
        .to_string();
    let api2 = api.clone();
    // Blocking: the handshake calls production over HTTP.
    let (who, token) = tokio::task::spawn_blocking(move || {
        Identity::from_production(api2.identity.as_ref(), &cookie, &api2.signer, now())
    })
    .await
    .map_err(|_| Fail(LiveSourceError::unauthorized("로그인 확인이 중단되었습니다.")))?
    .map_err(Fail)?;

    let (w, h) = crate::jobs::output_cap();
    let s = api.jobs.settings();
    Ok(Json(SessionBody {
        token,
        expires_in: crate::token::TOKEN_TTL_SECS,
        // Keyed by the id production just vouched for, so this list cannot
        // contain anybody else's destination.
        destinations: s.destinations.names_for(&who.user_id),
        max_concurrent: s.limits.max_concurrent,
        max_per_user: s.limits.max_per_user,
        max_width: w,
        max_height: h,
        max_upload_bytes: crate::media::MAX_UPLOAD_BYTES,
        max_storage_bytes: crate::media::MAX_USER_BYTES,
        allowed_extensions: crate::media::ALLOWED_EXTENSIONS.to_vec(),
    }))
}

#[derive(Debug, Serialize)]
pub struct JobsBody {
    pub jobs: Vec<JobView>,
    pub running: usize,
    pub running_mine: usize,
    pub max_concurrent: usize,
    pub max_per_user: usize,
}

async fn list_jobs(State(api): State<Api>, headers: HeaderMap) -> Out<JobsBody> {
    let who = enter(&api, &headers)?;
    Ok(Json(JobsBody {
        jobs: api.jobs.list(&who.user_id),
        running: api.jobs.running_count(),
        running_mine: api.jobs.running_count_for(&who.user_id),
        max_concurrent: api.jobs.settings().limits.max_concurrent,
        max_per_user: api.jobs.settings().limits.max_per_user,
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
    let who = enter(&api, &headers)?;
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
    let who = enter(&api, &headers)?;
    api.jobs.get(&who.user_id, &id).map(Json).map_err(Fail)
}

async fn cancel_job(State(api): State<Api>, headers: HeaderMap, Path(id): Path<String>) -> Out<JobView> {
    let who = enter(&api, &headers)?;
    let jobs = Arc::clone(&api.jobs);
    let owner = who.user_id.clone();
    tokio::task::spawn_blocking(move || jobs.cancel(&owner, &id))
        .await
        .map_err(|_| Fail(LiveSourceError::ffmpeg("작업 취소가 중단되었습니다.")))?
        .map(Json)
        .map_err(Fail)
}

#[derive(Debug, Serialize)]
pub struct MediaBody {
    pub media: Vec<StoredMedia>,
    pub used_bytes: u64,
    pub max_storage_bytes: u64,
    pub max_upload_bytes: u64,
    pub allowed_extensions: Vec<&'static str>,
}

fn media_body(api: &Api, user_id: &str) -> std::result::Result<MediaBody, Fail> {
    let m = &api.jobs.settings().media;
    let media = m.list(user_id).map_err(Fail)?;
    Ok(MediaBody {
        used_bytes: media.iter().map(|x| x.bytes).sum(),
        media,
        max_storage_bytes: crate::media::MAX_USER_BYTES,
        max_upload_bytes: crate::media::MAX_UPLOAD_BYTES,
        allowed_extensions: crate::media::ALLOWED_EXTENSIONS.to_vec(),
    })
}

/// `GET …/media` — what this caller has stored. Theirs only.
async fn list_media(State(api): State<Api>, headers: HeaderMap) -> Out<MediaBody> {
    let who = enter(&api, &headers)?;
    media_body(&api, &who.user_id).map(Json)
}

/// `PUT …/media/{name}` — the raw bytes of one file.
///
/// The name comes from the path, so axum's own routing refuses one containing a
/// `/` before this handler exists; [`crate::media::MediaRoot::check_name`] then
/// refuses the rest, and the file is written, probed and only then renamed into
/// place inside this caller's own directory.
async fn put_media(
    State(api): State<Api>,
    headers: HeaderMap,
    Path(name): Path<String>,
    raw: axum::body::Bytes,
) -> std::result::Result<(StatusCode, Json<StoredMedia>), Fail> {
    let who = enter(&api, &headers)?;
    let media = api.jobs.settings().media.clone();
    let owner = who.user_id.clone();
    // Blocking: this writes up to half a gigabyte and runs ffprobe over it.
    let stored = tokio::task::spawn_blocking(move || media.store(&owner, &name, &raw))
        .await
        .map_err(|_| Fail(LiveSourceError::ffmpeg("업로드가 중단되었습니다.")))?
        .map_err(Fail)?;
    Ok((StatusCode::CREATED, Json(stored)))
}

/// `DELETE …/media/{name}` — one of this caller's files, and only theirs.
async fn delete_media(
    State(api): State<Api>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Out<MediaBody> {
    let who = enter(&api, &headers)?;
    api.jobs.settings().media.delete(&who.user_id, &name).map_err(Fail)?;
    media_body(&api, &who.user_id).map(Json)
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
    let _who = enter(&api, &headers)?;
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

/// Gated but not authenticated, and it says nothing about any user.
///
/// The gate applies here too: an upstream check runs through the same proxy and
/// can carry the same header, and an ungated route would be a liveness oracle
/// for anything that can reach the port.
async fn health(State(api): State<Api>, headers: HeaderMap) -> Out<Health> {
    gate(&api, &headers)?;
    no_cookie(&headers)?;
    Ok(Json(Health {
        ok: true,
        running: api.jobs.running_count(),
        max_concurrent: api.jobs.settings().limits.max_concurrent,
    }))
}

/// Headers on every response, including the beta page's.
///
/// The content policy is strict because it can be: the beta page has no inline
/// script and no inline style — they are `app.js` and `app.css` beside it,
/// which is what lets `script-src` be `'self'` instead of `'unsafe-inline'`.
/// `default-src 'none'` means anything added later has to be allowed on
/// purpose.
const SECURITY_HEADERS: &[(&str, &str)] = &[
    (
        "content-security-policy",
        "default-src 'none'; \
         script-src 'self'; \
         style-src 'self'; \
         img-src 'self' data:; \
         font-src 'self'; \
         connect-src 'self'; \
         form-action 'none'; \
         base-uri 'none'; \
         frame-ancestors 'none'",
    ),
    // `frame-ancestors` above covers a modern browser; this covers the rest.
    ("x-frame-options", "DENY"),
    ("x-content-type-options", "nosniff"),
    // No referrer at all: a URL here can carry a broadcast id.
    ("referrer-policy", "no-referrer"),
    ("cross-origin-opener-policy", "same-origin"),
    ("cross-origin-resource-policy", "same-origin"),
    ("permissions-policy", "camera=(), microphone=(), geolocation=()"),
    // A session reply contains a bearer token, and a job list contains a
    // customer's broadcast ids. Neither belongs in a shared cache or on disk.
    ("cache-control", "no-store"),
];

async fn add_security_headers(mut res: Response) -> Response {
    let h = res.headers_mut();
    for (name, value) in SECURITY_HEADERS {
        if let (Ok(n), Ok(v)) = (HeaderName::from_bytes(name.as_bytes()), HeaderValue::from_str(value)) {
            h.insert(n, v);
        }
    }
    res
}

impl Api {
    /// The router, without the static beta page, so tests can drive the API
    /// alone.
    pub fn router(self) -> Router {
        self.routes(None)
    }

    /// The router plus `/beta/`, which is what the binary serves.
    pub fn router_with_beta(self, beta_dir: &std::path::Path) -> Router {
        self.routes(Some(beta_dir))
    }

    /// One place where the routes and the two layers are assembled, so the
    /// beta page is inside the gate and gets the security headers rather than
    /// being bolted on outside them.
    fn routes(self, beta_dir: Option<&std::path::Path>) -> Router {
        let upload_limit = crate::media::MAX_UPLOAD_BYTES as usize;
        let mut router = Router::new()
            .route("/api/live-source/health", get(health))
            .route("/api/live-source/session", post(session))
            .route("/api/live-source/check", post(check))
            .route("/api/live-source/jobs", get(list_jobs).post(create_job))
            .route("/api/live-source/jobs/{id}", get(get_job))
            .route("/api/live-source/jobs/{id}", delete(cancel_job))
            .route("/api/live-source/media", get(list_media))
            // The only route with a large body, and the limit is the same
            // number `MediaRoot::store` enforces — so an oversized upload is
            // refused by the server before it is buffered, and again by the
            // store if it ever gets that far.
            .route(
                "/api/live-source/media/{name}",
                axum::routing::put(put_media).layer(DefaultBodyLimit::max(upload_limit)),
            )
            .route("/api/live-source/media/{name}", delete(delete_media));

        if let Some(dir) = beta_dir {
            let index = dir.join("index.html");
            router = router.nest_service(
                "/beta",
                tower_http::services::ServeDir::new(dir)
                    .fallback(tower_http::services::ServeFile::new(index)),
            );
        }

        router
            // Inner: the gate, around everything above.
            .layer(axum::middleware::from_fn_with_state(self.clone(), gate_layer))
            // Outer, so the gate's own 403 carries them too.
            .layer(axum::middleware::map_response(add_security_headers))
            .with_state(self)
    }
}
