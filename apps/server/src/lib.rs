//! Louver Live's server: the same broadcast engine, without a desktop.
//!
//! A broadcast's life belongs to this process, not to the request that started
//! it and not to the browser that sent it. Closing the tab, or the laptop, does
//! nothing to a running stream; a restart of this process brings back whatever
//! was still meant to be running.

pub mod admin;
pub mod api;
pub mod auth;
pub mod billing;
pub mod diagnose;
pub mod error;
pub mod health;
pub mod state;
pub mod throttle;
pub mod youtube;

use crate::state::App;
use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, post};
use axum::Router;
use error::ApiError;
use louver_cloud::CloudError;

/// Run a blocking piece of the cloud crate without stalling the runtime.
///
/// Everything below the API is synchronous — SQLite, FFmpeg, the filesystem —
/// and deliberately so, because it is the code the desktop has been running for
/// months. This is the one place that bridges the two worlds.
pub async fn blocking<T, F>(f: F) -> std::result::Result<T, ApiError>
where
    F: FnOnce() -> louver_cloud::Result<T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(r) => r.map_err(ApiError::from),
        Err(_) => Err(ApiError(CloudError::Io(std::io::Error::other("요청 처리가 중단되었습니다")))),
    }
}

pub fn router(app: App) -> Router {
    let media = Router::new()
        .route("/api/media", get(api::list_media))
        .route("/api/media/{id}", get(api::get_media).delete(api::delete_media))
        .route("/api/media/upload", post(api::upload_media))
        // An upload is a video, not a form field. The per-plan ceiling is what
        // bounds it, and the handler enforces that as the bytes arrive.
        .layer(DefaultBodyLimit::disable());

    Router::new()
        // Unauthenticated on purpose: the caller is usually a healthcheck.
        // `/api/health` is the same answer, for a proxy that only forwards
        // `/api`.
        .route("/health", get(health::health))
        .route("/api/health", get(health::health))
        .route("/api/auth/register", post(auth::register))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/me", get(auth::me))
        .route("/api/me/subscription", get(auth::subscription))
        // Public on purpose: a price list nobody can read before signing up is
        // not a price list. There is deliberately **no** route that writes a
        // plan — see `CloudDb::activate_subscription`.
        .route("/api/plans", get(auth::plans))
        // Billing. `checkout` and `cancel` need a session; the two notification
        // routes do not, because PayApp posts to them server-to-server and what
        // authenticates those is the link keys in the body. There is still no
        // route that activates a subscription — `feedback` does, and only after
        // the provider has verified what it was sent.
        .route("/api/billing/status", get(billing::status))
        .route("/api/billing/checkout", post(billing::checkout))
        .route("/api/billing/cancel", post(billing::cancel))
        .route("/api/billing/payapp/feedback", post(billing::feedback))
        .route("/api/billing/payapp/failure", post(billing::failure))
        .merge(media)
        .route("/api/stream-destinations", get(api::list_destinations).post(api::create_destination))
        .route("/api/stream-destinations/{id}", delete(api::delete_destination))
        .route("/api/broadcasts", get(api::list_broadcasts).post(api::create_broadcast))
        .route(
            "/api/broadcasts/{id}",
            get(api::get_broadcast)
                .patch(api::update_broadcast)
                .put(api::update_broadcast)
                .delete(api::delete_broadcast),
        )
        .route("/api/broadcasts/{id}/items", get(api::list_items).put(api::replace_items))
        .route("/api/broadcasts/{id}/start", post(api::start_broadcast))
        .route("/api/broadcasts/{id}/stop", post(api::stop_broadcast))
        .route("/api/broadcasts/{id}/restart", post(api::restart_broadcast))
        .route("/api/broadcasts/{id}/logs", get(api::broadcast_logs))
        // §2. `oauth/callback` is the one authenticated-by-state route: see the
        // module for why a session cookie cannot reach it.
        .route("/api/youtube", get(youtube::availability))
        .route("/api/youtube/oauth/start", get(youtube::start))
        .route("/api/youtube/oauth/callback", get(youtube::callback))
        .route("/api/youtube/accounts", get(youtube::list_accounts))
        .route("/api/youtube/accounts/{id}", delete(youtube::delete_account))
        // Every one of these goes through the `Admin` extractor, which answers
        // 401 without a session and 403 for an ordinary user. There is
        // deliberately no admin route that writes `users.role`: an operator is
        // made at the command line, on the server.
        .route("/api/admin/dashboard", get(admin::dashboard))
        .route("/api/admin/revenue", get(admin::revenue))
        .route("/api/admin/users", get(admin::users))
        .route("/api/admin/users/{id}", get(admin::user))
        .route("/api/admin/users/{id}/payments", get(admin::user_payments))
        .route("/api/admin/users/{id}/disabled", post(admin::set_disabled))
        .route("/api/admin/broadcasts", get(admin::broadcasts))
        .route("/api/admin/broadcasts/{id}/stop", post(admin::force_stop))
        .route("/api/admin/billing", get(admin::billing))
        .route("/api/admin/system", get(admin::system))
        .route("/api/admin/audit", get(admin::audit))
        .route("/api/metrics", get(api::metrics))
        .route("/api/events", get(api::events))
        .with_state(app)
}

/// Serves the built web UI when there is one, so a single container is enough.
pub fn with_web_ui(router: Router) -> Router {
    let Ok(dir) = std::env::var("LOUVER_WEB_DIR") else {
        return router;
    };
    let index = std::path::Path::new(&dir).join("index.html");
    router.fallback_service(
        tower_http::services::ServeDir::new(&dir).fallback(tower_http::services::ServeFile::new(index)),
    )
}
