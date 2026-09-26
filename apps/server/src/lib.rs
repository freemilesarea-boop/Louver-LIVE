//! Louver Live's server: the same broadcast engine, without a desktop.
//!
//! A broadcast's life belongs to this process, not to the request that started
//! it and not to the browser that sent it. Closing the tab, or the laptop, does
//! nothing to a running stream; a restart of this process brings back whatever
//! was still meant to be running.

pub mod api;
pub mod auth;
pub mod error;
pub mod state;

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
        .route("/api/health", get(health))
        .route("/api/auth/register", post(auth::register))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/me", get(auth::me))
        .route("/api/me/subscription", get(auth::subscription))
        .merge(media)
        .route("/api/stream-destinations", get(api::list_destinations).post(api::create_destination))
        .route("/api/stream-destinations/{id}", delete(api::delete_destination))
        .route("/api/broadcasts", get(api::list_broadcasts).post(api::create_broadcast))
        .route("/api/broadcasts/{id}", get(api::get_broadcast).delete(api::delete_broadcast))
        .route("/api/broadcasts/{id}/start", post(api::start_broadcast))
        .route("/api/broadcasts/{id}/stop", post(api::stop_broadcast))
        .route("/api/broadcasts/{id}/restart", post(api::restart_broadcast))
        .route("/api/broadcasts/{id}/logs", get(api::broadcast_logs))
        .route("/api/events", get(api::events))
        .with_state(app)
}

pub async fn health() -> &'static str {
    "ok"
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
