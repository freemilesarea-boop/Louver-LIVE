//! The operator's console: what is happening, and the few things they may change.
//!
//! Two rules run through every handler below.
//!
//! **Authorization is server-side and it is a database column.** [`Admin`] is
//! the only way to reach any of this, it resolves the caller's row and reads
//! `role`, and there is no route anywhere in this service that writes that
//! column. Hiding a menu is not access control; neither is an email address the
//! code recognises.
//!
//! **Nothing here reimplements the service.** Stopping a broadcast goes through
//! `BroadcastManager::stop_with`, the same call the STOP button makes, because
//! that is what knows about `desired_state`, the watchdog, the concurrency slot
//! and YouTube's lifecycle. An admin console that killed a PID would leave every
//! one of those wrong.

use crate::auth::Caller;
use crate::error::ApiError;
use crate::state::App;
use axum::extract::{Path, Query, State};
use axum::Json;
use louver_cloud::admin::{self, AuditEntry, Page};
use louver_cloud::CloudError;
use serde::{Deserialize, Serialize};

/// A caller this server has established is an operator.
///
/// Carries the row rather than the id, because every action logs who did it and
/// a second lookup for the email would be a second chance to get it wrong.
#[derive(Debug, Clone)]
pub struct Admin(pub louver_cloud::User);

impl axum::extract::FromRequestParts<App> for Admin {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        app: &App,
    ) -> std::result::Result<Self, Self::Rejection> {
        // 401 first: "who are you" is answered before "may you". A caller with
        // no session must never be able to tell an admin route from a missing
        // one by its status code.
        let Caller(uid) = Caller::from_request_parts(parts, app).await?;
        let db = app.db.clone();
        let user = crate::blocking(move || db.user(&uid)).await?;
        if !user.is_admin() {
            // Deliberately not 404. An operator reading their own logs should
            // see refusals, and a user who guessed the URL learns only that
            // they are not allowed — which they already knew.
            eprintln!("[louver] admin: {} 님이 관리자 API에 접근을 시도했습니다", user.id);
            return Err(CloudError::NotAdmin.into());
        }
        Ok(Self(user))
    }
}

/// What an action changed: the thing it touched, its shape before and after,
/// and the operator's own reason. One struct rather than five parameters, so a
/// call site cannot silently swap `before` and `after`.
struct Change<'a> {
    target_type: &'a str,
    target_id: &'a str,
    before: String,
    after: String,
    note: Option<String>,
}

impl Admin {
    fn audit_change(&self, app: &App, action: &str, c: Change<'_>) {
        let entry = AuditEntry {
            admin_id: &self.0.id,
            admin_email: &self.0.email,
            action,
            target_type: c.target_type,
            target_id: c.target_id,
            before: Some(c.before),
            after: Some(c.after),
            note: c.note,
        };
        if let Err(e) = app.db.record_admin_action(&entry) {
            eprintln!("[louver] admin: 감사 로그를 쓰지 못했습니다: {e}");
        }
    }
}

type Out<T> = std::result::Result<Json<T>, ApiError>;

// --- dashboard -------------------------------------------------------------

pub async fn dashboard(State(app): State<App>, _: Admin) -> Out<admin::AdminDashboard> {
    Ok(Json(
        crate::blocking(move || {
            let probe = app.upload_tmp.clone();
            let free = louver_core::system::available_disk_bytes(&probe);
            Ok(admin::AdminDashboard {
                users: app.db.user_counts()?,
                subscriptions: app.db.subscription_counts()?,
                broadcasts: app.db.broadcast_counts()?,
                revenue: app.db.revenue_summary()?,
                mrr_krw: app.db.mrr()?,
                youtube_accounts: app.db.youtube_account_count()?,
                storage_bytes: app.db.total_storage_bytes()?,
                disk_free_bytes: (free > 0).then_some(free),
                disk_floor_bytes: louver_cloud::ingest::DISK_FLOOR_BYTES,
            })
        })
        .await?,
    ))
}

// --- revenue ---------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RevenueQuery {
    /// `YYYY-MM-DD`, in the operator's own timezone.
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    /// `day`, `week` or `month`.
    #[serde(default)]
    pub grain: Option<String>,
}

/// A date the query may safely be built from: ten characters, digits and dashes.
///
/// The range goes into SQL as a bound parameter, so this is not what stops an
/// injection — it is what stops a typo becoming an empty report that reads as
/// "no revenue".
fn clean_date(v: Option<String>, fallback: &str) -> String {
    match v {
        Some(d) if d.len() == 10 && d.chars().all(|c| c.is_ascii_digit() || c == '-') => d,
        _ => fallback.to_string(),
    }
}

pub async fn revenue(
    State(app): State<App>,
    _: Admin,
    Query(q): Query<RevenueQuery>,
) -> Out<admin::RevenueReport> {
    let grain = match q.grain.as_deref() {
        Some("week") => "week",
        Some("month") => "month",
        _ => "day",
    };
    // Thirty days back, in the operator's timezone, is what the page opens on.
    let today = chrono::Utc::now() + chrono::Duration::hours(admin::DISPLAY_OFFSET_HOURS);
    let from = clean_date(q.from, &(today - chrono::Duration::days(29)).format("%Y-%m-%d").to_string());
    let to = clean_date(q.to, &today.format("%Y-%m-%d").to_string());
    Ok(Json(crate::blocking(move || app.db.revenue_report(&from, &to, grain)).await?))
}

// --- users -----------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct UserQuery {
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub filter: Option<String>,
    #[serde(default)]
    pub sort: Option<String>,
    /// Not `#[serde(flatten)]`: a query string is deserialized from strings,
    /// and flattening forces the untyped path, where `Option<i64>` fails on a
    /// perfectly ordinary `?limit=50`.
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

impl UserQuery {
    fn page(&self) -> Page {
        Page { limit: self.limit, offset: self.offset }
    }
}

pub async fn users(
    State(app): State<App>,
    _: Admin,
    Query(q): Query<UserQuery>,
) -> Out<Vec<admin::AdminUserRow>> {
    Ok(Json(
        crate::blocking(move || {
            app.db.admin_users(
                q.q.as_deref().filter(|s| !s.trim().is_empty()),
                q.filter.as_deref(),
                q.sort.as_deref(),
                q.page().limit(),
                q.page().offset(),
            )
        })
        .await?,
    ))
}

pub async fn user(State(app): State<App>, _: Admin, Path(id): Path<String>) -> Out<admin::AdminUserDetail> {
    let mgr = app.mgr.clone();
    Ok(Json(
        crate::blocking(move || {
            let mut detail = app.db.admin_user(&id)?;
            let live = mgr.running_ids();
            for b in &mut detail.broadcasts {
                b.worker_alive = live.contains(&b.id);
            }
            Ok(detail)
        })
        .await?,
    ))
}

pub async fn user_payments(
    State(app): State<App>,
    _: Admin,
    Path(id): Path<String>,
    Query(page): Query<Page>,
) -> Out<Vec<admin::PaymentRow>> {
    Ok(Json(crate::blocking(move || app.db.payments(Some(&id), page.limit(), None)).await?))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisableRequest {
    pub disabled: bool,
    #[serde(default)]
    pub note: Option<String>,
}

/// Switch an account off, or back on. Never deletes anything.
pub async fn set_disabled(
    State(app): State<App>,
    who: Admin,
    Path(id): Path<String>,
    Json(body): Json<DisableRequest>,
) -> Out<admin::AdminUserRow> {
    let row = crate::blocking(move || {
        let before = app.db.user(&id)?;
        // An operator locking themselves out mid-incident helps nobody.
        if before.id == who.0.id && body.disabled {
            return Err(CloudError::Invalid("자기 계정은 비활성화할 수 없습니다".into()));
        }
        let after = app.db.set_disabled(&id, body.disabled)?;
        // Switching an account off stops its broadcasts. Leaving them running
        // would mean an account that cannot sign in and is still on air.
        if body.disabled {
            let stopped = app.mgr.stop_all_for(&id, louver_cloud::manager::StopReason::UserFinalStop);
            if !stopped.is_empty() {
                println!("[louver] admin: 계정 비활성화로 방송 {}개를 종료했습니다", stopped.len());
            }
        }
        who.audit_change(
            &app,
            if body.disabled { "admin.user.disable" } else { "admin.user.enable" },
            Change {
                target_type: "user",
                target_id: &id,
                before: if before.is_disabled() { "disabled" } else { "enabled" }.into(),
                after: if after.is_disabled() { "disabled" } else { "enabled" }.into(),
                note: body.note,
            },
        );
        app.db.admin_user(&id).map(|d| d.user)
    })
    .await?;
    Ok(Json(row))
}

// --- broadcasts ------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct BroadcastQuery {
    #[serde(default)]
    pub filter: Option<String>,
    /// Not `#[serde(flatten)]`: a query string is deserialized from strings,
    /// and flattening forces the untyped path, where `Option<i64>` fails on a
    /// perfectly ordinary `?limit=50`.
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

impl BroadcastQuery {
    fn page(&self) -> Page {
        Page { limit: self.limit, offset: self.offset }
    }
}

pub async fn broadcasts(
    State(app): State<App>,
    _: Admin,
    Query(q): Query<BroadcastQuery>,
) -> Out<Vec<admin::AdminBroadcastRow>> {
    let mgr = app.mgr.clone();
    Ok(Json(
        crate::blocking(move || {
            let mut rows =
                app.db.admin_broadcasts(None, q.filter.as_deref(), q.page().limit(), q.page().offset())?;
            // Whether a worker thread exists is this process's own knowledge and
            // is not in any table: a row that says running with no worker is
            // exactly the fault an operator is looking for.
            let live = mgr.running_ids();
            for b in &mut rows {
                b.worker_alive = live.contains(&b.id);
            }
            Ok(rows)
        })
        .await?,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForceStopRequest {
    #[serde(default)]
    pub note: Option<String>,
}

/// Take a broadcast off air, through the ordinary stop.
///
/// `StopReason::UserFinalStop` and not a new reason of its own: an operator
/// stopping a stream means the same thing to the rest of the system as its owner
/// pressing STOP — the intent becomes `stopped` so no watchdog and no boot
/// recovery revives it, the concurrency slot is returned, and YouTube's
/// broadcast is completed. Nothing here touches a process directly.
pub async fn force_stop(
    State(app): State<App>,
    who: Admin,
    Path(id): Path<String>,
    Json(body): Json<ForceStopRequest>,
) -> Out<admin::AdminBroadcastRow> {
    let row = crate::blocking(move || {
        let b = app.db.broadcast(&id)?;
        let before = format!("{}/{}", b.desired_state.id(), b.runtime_state.id());
        app.mgr.stop_with(&b.user_id, &id, louver_cloud::manager::StopReason::UserFinalStop)?;
        let after = app.db.broadcast(&id)?;
        who.audit_change(
            &app,
            "admin.broadcast.force_stop",
            Change {
                target_type: "broadcast",
                target_id: &id,
                before,
                after: format!("{}/{}", after.desired_state.id(), after.runtime_state.id()),
                note: body.note,
            },
        );
        app.db
            .admin_broadcasts(Some(&b.user_id), None, admin::MAX_PAGE, 0)?
            .into_iter()
            .find(|r| r.id == id)
            .ok_or(CloudError::NotFound("broadcast"))
    })
    .await?;
    Ok(Json(row))
}

// --- billing ---------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct BillingOverview {
    pub payments: Vec<admin::PaymentRow>,
    pub cancellations: Vec<admin::CancellationRow>,
    /// Records the provider still holds while the account has no entitlement,
    /// or the other way round. Read from our own rows; no provider is polled.
    pub mismatches: Vec<louver_cloud::db::BillingMismatch>,
    pub configured: bool,
}

#[derive(Debug, Deserialize)]
pub struct BillingQuery {
    /// `paid`, `failed`, or absent for everything.
    #[serde(default)]
    pub kind: Option<String>,
    /// Not `#[serde(flatten)]`: a query string is deserialized from strings,
    /// and flattening forces the untyped path, where `Option<i64>` fails on a
    /// perfectly ordinary `?limit=50`.
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

impl BillingQuery {
    fn page(&self) -> Page {
        Page { limit: self.limit, offset: self.offset }
    }
}

pub async fn billing(
    State(app): State<App>,
    _: Admin,
    Query(q): Query<BillingQuery>,
) -> Out<BillingOverview> {
    let configured = app.payapp.is_some();
    Ok(Json(
        crate::blocking(move || {
            Ok(BillingOverview {
                payments: app.db.payments(None, q.page().limit(), q.kind.as_deref())?,
                cancellations: app.db.cancellations(admin::DEFAULT_PAGE)?,
                mismatches: app.db.billing_mismatches()?,
                configured,
            })
        })
        .await?,
    ))
}

// --- system ----------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct SystemView {
    pub health: crate::health::Health,
    pub cpu_percent: f32,
    pub memory_used_mb: u64,
    pub memory_total_mb: u64,
    pub disk_free_bytes: u64,
    pub disk_floor_bytes: u64,
    pub storage_bytes: i64,
    pub workers: usize,
    pub running_broadcasts: i64,
    pub storage_leaders: Vec<admin::StorageUser>,
    pub problems: Vec<admin::ProblemRow>,
}

pub async fn system(State(app): State<App>, _: Admin) -> Out<SystemView> {
    let health = crate::health::snapshot(&app).await;
    let workers = app.mgr.running_ids().len();
    Ok(Json(
        crate::blocking(move || {
            // One sample, on this request. `sysinfo` needs two spaced samples
            // for a CPU figure, so the first reading of a fresh sampler is
            // zero — which is why the sampler lives in `App` and is reused.
            let (cpu, used, total) = app.machine.sample();
            Ok(SystemView {
                health,
                cpu_percent: cpu,
                memory_used_mb: used,
                memory_total_mb: total,
                disk_free_bytes: louver_core::system::available_disk_bytes(&app.upload_tmp),
                disk_floor_bytes: louver_cloud::ingest::DISK_FLOOR_BYTES,
                storage_bytes: app.db.total_storage_bytes()?,
                workers,
                running_broadcasts: app.db.broadcast_counts()?.running,
                storage_leaders: app.db.storage_leaders(20)?,
                problems: app.db.recent_problems(50)?,
            })
        })
        .await?,
    ))
}

// --- audit -----------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    #[serde(default)]
    pub before_id: Option<i64>,
    /// Not `#[serde(flatten)]`: a query string is deserialized from strings,
    /// and flattening forces the untyped path, where `Option<i64>` fails on a
    /// perfectly ordinary `?limit=50`.
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

impl AuditQuery {
    fn page(&self) -> Page {
        Page { limit: self.limit, offset: self.offset }
    }
}

pub async fn audit(
    State(app): State<App>,
    _: Admin,
    Query(q): Query<AuditQuery>,
) -> Out<Vec<admin::AuditRow>> {
    Ok(Json(crate::blocking(move || app.db.audit_log(q.page().limit(), q.before_id)).await?))
}
