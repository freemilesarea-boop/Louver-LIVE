//! What an operator, and a container, need to know.
//!
//! `/health` answers without a session, because the thing asking is usually a
//! healthcheck or an uptime monitor rather than a person. It therefore says
//! only whether each part is working — never a path, a count or a credential.

use crate::state::App;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;

#[derive(Serialize)]
pub struct Health {
    /// "ok" when every check passed, "degraded" when one did not.
    pub status: &'static str,
    pub version: &'static str,
    /// `local` or `cloud`. What decides whether closing a laptop ends a
    /// broadcast, so it is the first thing the UI shows.
    pub deployment: String,
    pub checks: Checks,
}

#[derive(Serialize)]
pub struct Checks {
    pub api: bool,
    pub database: bool,
    pub ffmpeg: bool,
    pub storage: bool,
}

/// Where this is running, as the operator declared it.
///
/// Defaults to `local`, which is the honest default: an unset variable means
/// nobody has said this is a server, and a laptop that sleeps is the common
/// case. The container image sets `cloud` for itself.
pub fn deployment() -> String {
    match std::env::var("LOUVER_DEPLOYMENT").unwrap_or_default().trim().to_lowercase().as_str() {
        "cloud" | "remote" | "production" | "prod" => "cloud".into(),
        _ => "local".into(),
    }
}

pub async fn health(State(app): State<App>) -> (StatusCode, Json<Health>) {
    let checks = tokio::task::spawn_blocking(move || Checks {
        api: true,
        database: app.db.ping().is_ok(),
        ffmpeg: ffmpeg_runs(&app.tools.ffmpeg),
        storage: storage_writable(&app),
    })
    .await
    .unwrap_or(Checks { api: true, database: false, ffmpeg: false, storage: false });

    let ok = checks.api && checks.database && checks.ffmpeg && checks.storage;
    let body = Health {
        status: if ok { "ok" } else { "degraded" },
        version: env!("CARGO_PKG_VERSION"),
        deployment: deployment(),
        checks,
    };
    // 503 when degraded, so an orchestrator does not have to parse the body.
    (if ok { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE }, Json(body))
}

/// Does FFmpeg actually run? Not "is there a file at this path".
///
/// Spawned every time rather than cached. A cache would have to be keyed on
/// nothing — the path cannot change while the process lives — and it would
/// mean a health answer that is up to a minute stale about the one part of the
/// system that a bad deploy actually breaks. `-version` costs milliseconds.
fn ffmpeg_runs(program: &std::path::Path) -> bool {
    std::process::Command::new(program)
        .arg("-version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Write a byte and take it back. A full or read-only volume fails here rather
/// than halfway through someone's upload.
fn storage_writable(app: &App) -> bool {
    let dir = app.storage.scratch_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let probe = dir.join(".health");
    let ok = std::fs::write(&probe, b"ok").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}
