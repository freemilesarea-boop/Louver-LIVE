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
    /// `auto`, `always` or `never` — when the session cookie carries `Secure`.
    /// Here because a login that appears to succeed and then 401s is almost
    /// always this, and this is the fastest place to see it.
    pub cookies: &'static str,
    pub checks: Checks,
}

#[derive(Serialize)]
pub struct Checks {
    pub api: bool,
    pub database: bool,
    pub ffmpeg: bool,
    /// Can this FFmpeg speak `rtmps`? YouTube accepts nothing else, and a build
    /// without TLS support fails only at the moment a broadcast starts — which
    /// is the worst time to find out. Checked here so a deploy says it up front.
    pub ffmpeg_rtmps: bool,
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
    let checks = tokio::task::spawn_blocking(move || {
        let (ffmpeg, rtmps) = ffmpeg_state(&app.tools.ffmpeg);
        Checks {
            api: true,
            database: app.db.ping().is_ok(),
            ffmpeg,
            ffmpeg_rtmps: rtmps,
            storage: storage_writable(&app),
        }
    })
    .await
    .unwrap_or(Checks {
        api: true,
        database: false,
        ffmpeg: false,
        ffmpeg_rtmps: false,
        storage: false,
    });

    let ok = checks.api && checks.database && checks.ffmpeg && checks.ffmpeg_rtmps && checks.storage;
    let body = Health {
        status: if ok { "ok" } else { "degraded" },
        version: env!("CARGO_PKG_VERSION"),
        deployment: deployment(),
        cookies: crate::auth::CookiePolicy::from_env().name(),
        checks,
    };
    // 503 when degraded, so an orchestrator does not have to parse the body.
    (if ok { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE }, Json(body))
}

/// Does FFmpeg run, and can it reach an RTMPS ingest?
///
/// Spawned every time rather than cached. A cache would have to be keyed on
/// nothing — the path cannot change while the process lives — and it would mean
/// a health answer that is up to a minute stale about the one part of the system
/// a bad deploy actually breaks. Listing protocols costs milliseconds.
fn ffmpeg_state(program: &std::path::Path) -> (bool, bool) {
    let Ok(out) = std::process::Command::new(program)
        .args(["-hide_banner", "-protocols"])
        .stdin(std::process::Stdio::null())
        .output()
    else {
        return (false, false);
    };
    if !out.status.success() {
        return (false, false);
    }
    let listed = String::from_utf8_lossy(&out.stdout);
    let rtmps = listed.split_whitespace().any(|p| p == "rtmps");
    (true, rtmps)
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
