//! What an operator needs to see, and the few things they may change.
//!
//! Every figure here is read from the same rows the service itself runs on —
//! there is no second store, no nightly rollup and no estimate. Two rules
//! follow from that, and they are the whole design:
//!
//! * **Revenue is `billing_events`, and only the rows that are payments.** A
//!   plan's price multiplied by a headcount is a forecast, not money. The
//!   ledger has one row per *verified* provider notification, deduplicated by
//!   the provider's own event key, so a callback delivered ten times is one
//!   payment here exactly as it is one payment in the bank.
//! * **Nothing is invented.** Where the data cannot answer a question — refunds,
//!   for instance, which PayApp does not tell us about — the answer is absent
//!   rather than zero, and the caller says so.
//!
//! Aggregation is done by SQLite. An admin page must not read a table into
//! memory to count it; on a server whose other job is pushing video, that is
//! the difference between a dashboard and an outage.

use crate::db::CloudDb;
use crate::models::*;
use crate::{CloudError, Result};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

/// Where the service lives, for turning UTC timestamps into local days.
///
/// Every timestamp in this database is UTC, written by SQLite's
/// `datetime('now')`. 247streams is operated from Korea, so "today's revenue"
/// means the Korean day — and a payment at 08:00 UTC belongs to the next Korean
/// day, not this one. Rather than store local time (which would break every
/// comparison already written), the aggregations shift on the way out.
pub const DISPLAY_OFFSET_HOURS: i64 = 9;

/// The SQL that turns a stored UTC timestamp into the operator's local date.
const LOCAL_DATE: &str = "date(processed_at, '+9 hours')";

/// `pay_state` of a completed payment. The one state that is money.
const PAID: &str = "4";

/// How many rows a list endpoint will return at most, whatever it is asked for.
pub const MAX_PAGE: i64 = 100;
pub const DEFAULT_PAGE: i64 = 50;

// --- what the dashboard shows ---------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct AdminDashboard {
    /// Entitlement an operator handed out. Deliberately its own field, next to
    /// nothing financial: a grant is not a sale and must never be read as one.
    pub grants: crate::grants::GrantCounts,
    pub users: UserCounts,
    pub subscriptions: SubscriptionCounts,
    pub broadcasts: BroadcastCounts,
    pub revenue: RevenueSummary,
    /// Won per month the provider is currently set to collect. See [`mrr`].
    pub mrr_krw: i64,
    pub youtube_accounts: i64,
    pub storage_bytes: i64,
    /// Free bytes on the volume the media live on, and the floor uploads stop
    /// at. `None` when the mount could not be identified.
    pub disk_free_bytes: Option<u64>,
    pub disk_floor_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct UserCounts {
    pub total: i64,
    pub today: i64,
    pub last_7_days: i64,
    pub last_30_days: i64,
    pub disabled: i64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct SubscriptionCounts {
    /// Paying accounts, by plan id, in the plans' own order.
    pub by_plan: Vec<PlanCount>,
    pub paid_total: i64,
    pub unsubscribed: i64,
    /// This month, from the billing ledger rather than from today's state.
    pub new_this_month: i64,
    pub renewed_this_month: i64,
    pub cancelled_this_month: i64,
    pub failed_this_month: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanCount {
    pub plan_id: String,
    pub label: String,
    pub monthly_price_krw: i64,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct BroadcastCounts {
    pub running: i64,
    pub scheduled: i64,
    pub failed: i64,
    pub total: i64,
}

/// Money that actually arrived, over the periods an operator asks about first.
#[derive(Debug, Clone, Serialize, Default)]
pub struct RevenueSummary {
    pub today: i64,
    pub yesterday: i64,
    pub this_week: i64,
    pub this_month: i64,
    pub last_month: i64,
    pub this_year: i64,
    pub all_time: i64,
    /// Month-on-month, as a percentage, `None` when last month was zero — a
    /// division by nothing is not a 100% rise.
    pub month_change_pct: Option<f64>,
}

/// One day, week or month of takings.
#[derive(Debug, Clone, Serialize)]
pub struct RevenueBucket {
    /// `YYYY-MM-DD` for a day, the Monday for a week, `YYYY-MM` for a month.
    pub period: String,
    pub krw: i64,
    pub payments: i64,
    /// A payment that is the first one on its billing record.
    pub new_krw: i64,
    pub renewal_krw: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanRevenue {
    pub plan_id: String,
    pub label: String,
    pub krw: i64,
    pub payments: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RevenueReport {
    pub from: String,
    pub to: String,
    pub grain: String,
    pub summary: RevenueSummary,
    pub mrr_krw: i64,
    pub active_payers: i64,
    /// Average revenue per paying account this month. `None` with no payers.
    pub arpu_krw: Option<i64>,
    pub buckets: Vec<RevenueBucket>,
    pub by_plan: Vec<PlanRevenue>,
    pub recent: Vec<PaymentRow>,
    /// Payments this range that were reversed at the provider. Recorded because
    /// the ledger has them; **not** subtracted from the figures above, which are
    /// gross receipts.
    pub reversals: i64,
}

/// One row of the payment ledger, as an operator may read it.
#[derive(Debug, Clone, Serialize)]
pub struct PaymentRow {
    pub at: String,
    pub user_id: Option<String>,
    pub email: Option<String>,
    pub plan_id: Option<String>,
    pub amount_krw: i64,
    pub pay_state: String,
    /// `paid`, `reversal`, `waiting`, `other` — the ledger's own words made
    /// into something a table can filter on.
    pub kind: String,
    /// New payment on this billing record, or a renewal. `None` when the event
    /// is not a payment.
    pub is_first: Option<bool>,
    pub pay_type: Option<String>,
    pub outcome: String,
    /// The provider's subscription reference, masked. Never a credential.
    pub provider_ref: Option<String>,
}

/// One account, as the list shows it.
#[derive(Debug, Clone, Serialize)]
pub struct AdminUserRow {
    pub id: String,
    pub email: String,
    pub name: Option<String>,
    pub created_at: String,
    pub role: String,
    pub disabled_at: Option<String>,
    /// What the account pays for. Unchanged by a grant.
    pub plan_id: String,
    pub plan_label: String,
    /// The plan actually in force, once an operator's grant is taken into
    /// account. Equal to `plan_id` whenever there is no grant.
    pub effective_plan_id: String,
    pub effective_plan_label: String,
    /// `paid`, `grant` or `none` — why `effective_plan_id` is what it is.
    pub entitlement_source: String,
    /// The live grant's plan and remaining days, for a `Business 지급 · D-29`
    /// badge. `None` when there is no grant in force.
    pub grant_plan_id: Option<String>,
    pub grant_expires_at: Option<String>,
    pub grant_days_left: Option<i64>,
    pub subscription_status: String,
    pub billing_status: Option<String>,
    pub next_charge_at: Option<String>,
    pub storage_bytes: i64,
    pub storage_limit_bytes: i64,
    pub youtube_accounts: i64,
    pub running_broadcasts: i64,
    pub total_paid_krw: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminUserDetail {
    pub user: AdminUserRow,
    pub media_count: i64,
    pub payments: Vec<PaymentRow>,
    pub payment_count: i64,
    pub first_paid_at: Option<String>,
    pub last_paid_at: Option<String>,
    pub billing: Vec<BillingSubscription>,
    /// Every grant this account has had, newest first. The live one is whichever
    /// has `state == active`; the rest are its history.
    pub grants: Vec<crate::grants::Grant>,
    /// Channel titles only. No token, ever.
    pub youtube_channels: Vec<String>,
    pub broadcasts: Vec<AdminBroadcastRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminBroadcastRow {
    pub id: String,
    pub name: String,
    pub user_id: String,
    pub email: String,
    pub desired_state: String,
    pub runtime_state: String,
    pub restart_count: i64,
    pub started_at: Option<String>,
    pub stopped_at: Option<String>,
    pub last_heartbeat: Option<String>,
    pub last_error: Option<String>,
    pub scheduled: bool,
    pub loop_forever: bool,
    pub item_count: i64,
    /// The connected channel's title, when there is one. Never a stream key.
    pub youtube_channel: Option<String>,
    pub youtube_status: Option<String>,
    /// Is a worker thread alive for this broadcast in this process?
    pub worker_alive: bool,
}

/// An account whose recurring payment has ended.
#[derive(Debug, Clone, Serialize)]
pub struct CancellationRow {
    pub user_id: String,
    pub email: String,
    pub plan_id: String,
    pub cancelled_at: Option<String>,
    pub last_paid_at: Option<String>,
    pub amount_krw: i64,
    pub entitlement_plan: String,
    pub entitlement_active: bool,
    pub running_broadcasts: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageUser {
    pub user_id: String,
    pub email: String,
    pub plan_id: String,
    pub bytes: i64,
    pub limit_bytes: i64,
    pub files: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditRow {
    pub id: i64,
    pub at: String,
    pub admin_id: String,
    pub admin_email: String,
    pub action: String,
    pub target_type: String,
    pub target_id: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub note: Option<String>,
}

/// What an operator did, on its way into the log.
#[derive(Debug, Clone)]
pub struct AuditEntry<'a> {
    pub admin_id: &'a str,
    pub admin_email: &'a str,
    pub action: &'a str,
    pub target_type: &'a str,
    pub target_id: &'a str,
    pub before: Option<String>,
    pub after: Option<String>,
    pub note: Option<String>,
}

/// Anything in a note that looks like a credential, replaced before storage.
///
/// The note is the one free-text field an operator fills in, and an audit log is
/// the table most likely to be pasted into a support thread. Belt as well as
/// braces: nothing in this codebase *puts* a secret here, and this makes sure
/// nothing ever does by accident.
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for word in text.split_inclusive(char::is_whitespace) {
        let bare = word.trim();
        let looks_secret = bare.len() >= 16
            && (bare.starts_with("GOCSPX-")
                || bare.starts_with("ya29.")
                || bare.starts_with("1//")
                || bare.chars().all(|c| c.is_ascii_hexdigit()) && bare.len() >= 32);
        if looks_secret {
            out.push_str("[REDACTED]");
            if word.len() > bare.len() {
                out.push(' ');
            }
        } else {
            out.push_str(word);
        }
    }
    out
}

/// PayApp's own reference, shortened so support can quote it and nobody can use
/// it. It is not a credential to begin with; this keeps the habit anyway.
fn mask_ref(v: Option<String>) -> Option<String> {
    v.map(|s| if s.len() <= 4 { "…".to_string() } else { format!("…{}", &s[s.len() - 4..]) })
}

/// The ledger's `pay_state` in a word a table can group on. The numbers are
/// PayApp's own; see `billing::pay_state`.
fn kind_of(pay_state: &str) -> &'static str {
    match pay_state {
        PAID => "paid",
        "8" | "32" | "9" | "64" | "70" | "71" => "reversal",
        "1" | "10" => "waiting",
        _ => "other",
    }
}

impl CloudDb {
    // --- authorization ----------------------------------------------------

    /// Make this account an operator, or take it back. The CLI's whole job.
    pub fn set_role(&self, email: &str, role: &str) -> Result<User> {
        if role != ROLE_ADMIN && role != ROLE_USER {
            return Err(CloudError::Invalid("권한은 user 또는 admin 이어야 합니다".into()));
        }
        let n = self
            .raw()
            .lock()
            .unwrap()
            .execute("UPDATE users SET role=?2 WHERE email=?1 COLLATE NOCASE", params![email.trim(), role])?;
        if n == 0 {
            return Err(CloudError::NotFound("user"));
        }
        self.user_by_email(email)
    }

    pub fn admins(&self) -> Result<Vec<String>> {
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let mut st = guard.prepare("SELECT email FROM users WHERE role='admin' ORDER BY email")?;
        let rows = st.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Switch an account off, or back on. Keeps every row the account owns.
    pub fn set_disabled(&self, user_id: &str, disabled: bool) -> Result<User> {
        let n = self.raw().lock().unwrap().execute(
            "UPDATE users SET disabled_at = CASE WHEN ?2 THEN COALESCE(disabled_at, datetime('now'))
                                                 ELSE NULL END
             WHERE id=?1",
            params![user_id, disabled],
        )?;
        if n == 0 {
            return Err(CloudError::NotFound("user"));
        }
        // Signing out everywhere is part of disabling: a cookie issued an hour
        // ago would otherwise keep working until it expired.
        if disabled {
            self.raw().lock().unwrap().execute("DELETE FROM auth_sessions WHERE user_id=?1", [user_id])?;
        }
        self.user(user_id)
    }

    /// Refuse anything an account has been switched off for.
    pub fn require_enabled(&self, user_id: &str) -> Result<()> {
        let disabled: Option<Option<String>> = self
            .raw()
            .lock()
            .unwrap()
            .query_row("SELECT disabled_at FROM users WHERE id=?1", [user_id], |r| r.get(0))
            .optional()?;
        match disabled {
            Some(Some(_)) => Err(CloudError::Disabled),
            Some(None) => Ok(()),
            None => Err(CloudError::NotFound("user")),
        }
    }

    // --- audit ------------------------------------------------------------

    pub fn record_admin_action(&self, e: &AuditEntry<'_>) -> Result<()> {
        self.raw().lock().unwrap().execute(
            "INSERT INTO admin_audit (admin_id, admin_email, action, target_type, target_id,
                                      before, after, note)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                e.admin_id,
                e.admin_email,
                e.action,
                e.target_type,
                e.target_id,
                e.before.as_deref().map(redact),
                e.after.as_deref().map(redact),
                e.note.as_deref().map(redact),
            ],
        )?;
        Ok(())
    }

    pub fn audit_log(&self, limit: i64, before_id: Option<i64>) -> Result<Vec<AuditRow>> {
        let limit = limit.clamp(1, MAX_PAGE);
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let mut st = guard.prepare(
            "SELECT id, at, admin_id, admin_email, action, target_type, target_id, before, after, note
             FROM admin_audit WHERE (?2 IS NULL OR id < ?2) ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = st.query_map(params![limit, before_id], |r| {
            Ok(AuditRow {
                id: r.get(0)?,
                at: r.get(1)?,
                admin_id: r.get(2)?,
                admin_email: r.get(3)?,
                action: r.get(4)?,
                target_type: r.get(5)?,
                target_id: r.get(6)?,
                before: r.get(7)?,
                after: r.get(8)?,
                note: r.get(9)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // --- counts -----------------------------------------------------------

    fn scalar(&self, sql: &str) -> Result<i64> {
        Ok(self.raw().lock().unwrap().query_row(sql, [], |r| r.get(0))?)
    }

    pub fn user_counts(&self) -> Result<UserCounts> {
        // Days are the operator's, not UTC's: `created_at` is shifted the same
        // way every other figure on this page is.
        Ok(UserCounts {
            total: self.scalar("SELECT COUNT(*) FROM users")?,
            today: self.scalar(
                "SELECT COUNT(*) FROM users
                 WHERE date(created_at, '+9 hours') = date('now', '+9 hours')",
            )?,
            last_7_days: self.scalar(
                "SELECT COUNT(*) FROM users
                 WHERE date(created_at, '+9 hours') > date('now', '+9 hours', '-7 days')",
            )?,
            last_30_days: self.scalar(
                "SELECT COUNT(*) FROM users
                 WHERE date(created_at, '+9 hours') > date('now', '+9 hours', '-30 days')",
            )?,
            disabled: self.scalar("SELECT COUNT(*) FROM users WHERE disabled_at IS NOT NULL")?,
        })
    }

    /// Who is paying for what, counted from the entitlement the service honours.
    pub fn subscription_counts(&self) -> Result<SubscriptionCounts> {
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let mut st = guard.prepare(
            "SELECT p.id, p.label, p.monthly_price_krw, COUNT(u.id)
             FROM plans p LEFT JOIN users u ON u.plan_id = p.id
             WHERE p.active = 1
             GROUP BY p.id ORDER BY p.sort_order, p.id",
        )?;
        let by_plan: Vec<PlanCount> = st
            .query_map([], |r| {
                Ok(PlanCount {
                    plan_id: r.get(0)?,
                    label: r.get(1)?,
                    monthly_price_krw: r.get(2)?,
                    count: r.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(st);
        drop(guard);

        let paid_total = by_plan.iter().map(|p| p.count).sum();
        let month = "strftime('%Y-%m', processed_at, '+9 hours') = strftime('%Y-%m', 'now', '+9 hours')";
        Ok(SubscriptionCounts {
            unsubscribed: self.scalar(&format!(
                "SELECT COUNT(*) FROM users WHERE plan_id = '{}'",
                crate::db::UNSUBSCRIBED_PLAN
            ))?,
            by_plan,
            paid_total,
            // A payment that is the first one on its billing record is a new
            // subscriber; a later one on the same record is a renewal.
            new_this_month: self.scalar(&format!(
                "SELECT COUNT(*) FROM billing_events e
                 WHERE e.pay_state = '{PAID}' AND {month}
                   AND e.id = (SELECT f.id FROM billing_events f
                               WHERE f.billing_id = e.billing_id AND f.pay_state = '{PAID}'
                               ORDER BY f.processed_at, f.id LIMIT 1)"
            ))?,
            renewed_this_month: self.scalar(&format!(
                "SELECT COUNT(*) FROM billing_events e
                 WHERE e.pay_state = '{PAID}' AND {month}
                   AND e.id <> (SELECT f.id FROM billing_events f
                                WHERE f.billing_id = e.billing_id AND f.pay_state = '{PAID}'
                                ORDER BY f.processed_at, f.id LIMIT 1)"
            ))?,
            cancelled_this_month: self.scalar(
                "SELECT COUNT(*) FROM billing_subscriptions
                 WHERE cancelled_at IS NOT NULL
                   AND strftime('%Y-%m', cancelled_at, '+9 hours') = strftime('%Y-%m', 'now', '+9 hours')",
            )?,
            failed_this_month: self.scalar(&format!(
                "SELECT COUNT(*) FROM billing_events
                 WHERE pay_state <> '{PAID}' AND pay_state NOT IN ('1','10') AND {month}"
            ))?,
        })
    }

    pub fn broadcast_counts(&self) -> Result<BroadcastCounts> {
        Ok(BroadcastCounts {
            running: self.scalar("SELECT COUNT(*) FROM broadcasts WHERE desired_state='running'")?,
            scheduled: self
                .scalar("SELECT COUNT(*) FROM broadcasts WHERE sched_enabled=1 AND desired_state<>'running'")
                .unwrap_or(0),
            failed: self.scalar("SELECT COUNT(*) FROM broadcasts WHERE runtime_state='FAILED'")?,
            total: self.scalar("SELECT COUNT(*) FROM broadcasts")?,
        })
    }

    /// Won per month the provider is currently set to collect.
    ///
    /// **Definition.** The sum of `amount_krw` over billing records whose status
    /// is `active` — one row per account that has paid at least once and whose
    /// recurring registration PayApp still holds. Not a headcount times a price:
    /// an account is billed the amount its own record was registered at, which
    /// is what the provider will actually charge if the price list changes
    /// tomorrow.
    ///
    /// Excluded, and why:
    ///
    /// * `pending` — registered, never paid. There is no money to recur yet.
    /// * `cancelled` — the provider has stopped.
    /// * `payment_failed` — the last charge was reversed. PayApp still holds the
    ///   registration and may retry, so this is not lost revenue, but it is not
    ///   money that is coming either. Counted separately as 결제 실패.
    /// * `registration_failed` — never existed.
    ///
    /// A plan an operator granted by hand has no billing record and contributes
    /// nothing. That is correct: nobody is paying for it.
    pub fn mrr(&self) -> Result<i64> {
        self.scalar("SELECT COALESCE(SUM(amount_krw), 0) FROM billing_subscriptions WHERE status = 'active'")
    }

    pub fn active_payers(&self) -> Result<i64> {
        self.scalar("SELECT COUNT(DISTINCT user_id) FROM billing_subscriptions WHERE status = 'active'")
    }

    // --- revenue ----------------------------------------------------------

    fn paid_between(&self, from: &str, to: &str) -> Result<i64> {
        Ok(self.raw().lock().unwrap().query_row(
            &format!(
                "SELECT COALESCE(SUM(amount_krw), 0) FROM billing_events
                 WHERE pay_state = '{PAID}' AND {LOCAL_DATE} BETWEEN ?1 AND ?2"
            ),
            params![from, to],
            |r| r.get(0),
        )?)
    }

    /// The periods an operator asks about before anything else.
    pub fn revenue_summary(&self) -> Result<RevenueSummary> {
        let today = self.scalar(&format!(
            "SELECT COALESCE(SUM(amount_krw),0) FROM billing_events
             WHERE pay_state='{PAID}' AND {LOCAL_DATE} = date('now','+9 hours')"
        ))?;
        let yesterday = self.scalar(&format!(
            "SELECT COALESCE(SUM(amount_krw),0) FROM billing_events
             WHERE pay_state='{PAID}' AND {LOCAL_DATE} = date('now','+9 hours','-1 day')"
        ))?;
        // The week starts on Monday, which is what a Korean operator means.
        let this_week = self.scalar(&format!(
            "SELECT COALESCE(SUM(amount_krw),0) FROM billing_events
             WHERE pay_state='{PAID}'
               AND {LOCAL_DATE} >= date('now','+9 hours','weekday 1','-7 days')"
        ))?;
        let this_month = self.scalar(&format!(
            "SELECT COALESCE(SUM(amount_krw),0) FROM billing_events
             WHERE pay_state='{PAID}'
               AND strftime('%Y-%m', processed_at, '+9 hours') = strftime('%Y-%m','now','+9 hours')"
        ))?;
        let last_month = self.scalar(&format!(
            "SELECT COALESCE(SUM(amount_krw),0) FROM billing_events
             WHERE pay_state='{PAID}'
               AND strftime('%Y-%m', processed_at, '+9 hours')
                   = strftime('%Y-%m','now','+9 hours','start of month','-1 day')"
        ))?;
        Ok(RevenueSummary {
            today,
            yesterday,
            this_week,
            this_month,
            last_month,
            this_year: self.scalar(&format!(
                "SELECT COALESCE(SUM(amount_krw),0) FROM billing_events
                 WHERE pay_state='{PAID}'
                   AND strftime('%Y', processed_at, '+9 hours') = strftime('%Y','now','+9 hours')"
            ))?,
            all_time: self.scalar(&format!(
                "SELECT COALESCE(SUM(amount_krw),0) FROM billing_events WHERE pay_state='{PAID}'"
            ))?,
            // Nothing last month is not a 100% rise; it is no comparison at all.
            month_change_pct: (last_month > 0)
                .then(|| (this_month - last_month) as f64 * 100.0 / last_month as f64),
        })
    }

    /// Takings over a range, bucketed, split by new and renewing.
    pub fn revenue_report(&self, from: &str, to: &str, grain: &str) -> Result<RevenueReport> {
        let bucket = match grain {
            "month" => "strftime('%Y-%m', processed_at, '+9 hours')",
            // The Monday of that week, so the label is a date rather than a
            // week number nobody can place.
            "week" => "date(processed_at, '+9 hours', 'weekday 1', '-7 days')",
            _ => LOCAL_DATE,
        };
        let first_of_record = format!(
            "e.id = (SELECT f.id FROM billing_events f
                     WHERE f.billing_id = e.billing_id AND f.pay_state = '{PAID}'
                     ORDER BY f.processed_at, f.id LIMIT 1)"
        );
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let mut st = guard.prepare(&format!(
            "SELECT {bucket} AS period,
                    COALESCE(SUM(amount_krw),0),
                    COUNT(*),
                    COALESCE(SUM(CASE WHEN {first_of_record} THEN amount_krw ELSE 0 END),0),
                    COALESCE(SUM(CASE WHEN {first_of_record} THEN 0 ELSE amount_krw END),0)
             FROM billing_events e
             WHERE e.pay_state = '{PAID}' AND {LOCAL_DATE} BETWEEN ?1 AND ?2
             GROUP BY period ORDER BY period"
        ))?;
        let buckets: Vec<RevenueBucket> = st
            .query_map(params![from, to], |r| {
                Ok(RevenueBucket {
                    period: r.get(0)?,
                    krw: r.get(1)?,
                    payments: r.get(2)?,
                    new_krw: r.get(3)?,
                    renewal_krw: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(st);

        let mut st = guard.prepare(&format!(
            "SELECT COALESCE(b.plan_id, '?'), COALESCE(p.label, '(알 수 없음)'),
                    COALESCE(SUM(e.amount_krw),0), COUNT(*)
             FROM billing_events e
             LEFT JOIN billing_subscriptions b ON b.id = e.billing_id
             LEFT JOIN plans p ON p.id = b.plan_id
             WHERE e.pay_state = '{PAID}' AND {LOCAL_DATE} BETWEEN ?1 AND ?2
             GROUP BY b.plan_id ORDER BY 3 DESC"
        ))?;
        let by_plan: Vec<PlanRevenue> = st
            .query_map(params![from, to], |r| {
                Ok(PlanRevenue { plan_id: r.get(0)?, label: r.get(1)?, krw: r.get(2)?, payments: r.get(3)? })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(st);
        drop(guard);

        let reversals = self.raw().lock().unwrap().query_row(
            &format!(
                "SELECT COUNT(*) FROM billing_events
                 WHERE pay_state IN ('8','32','9','64','70','71') AND {LOCAL_DATE} BETWEEN ?1 AND ?2"
            ),
            params![from, to],
            |r| r.get(0),
        )?;

        let summary = RevenueSummary { all_time: self.paid_between(from, to)?, ..self.revenue_summary()? };
        let payers = self.active_payers()?;
        Ok(RevenueReport {
            from: from.to_string(),
            to: to.to_string(),
            grain: grain.to_string(),
            arpu_krw: (payers > 0).then(|| summary.this_month / payers),
            mrr_krw: self.mrr()?,
            active_payers: payers,
            summary,
            buckets,
            by_plan,
            recent: self.payments(None, 20, None)?,
            reversals,
        })
    }

    /// The payment ledger, newest first. One user's, or everybody's.
    pub fn payments(&self, user_id: Option<&str>, limit: i64, kind: Option<&str>) -> Result<Vec<PaymentRow>> {
        let limit = limit.clamp(1, MAX_PAGE);
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let mut st = guard.prepare(&format!(
            "SELECT e.processed_at, e.user_id, u.email, b.plan_id, e.amount_krw, e.pay_state,
                    e.pay_type, e.outcome, e.provider_subscription_id,
                    CASE WHEN e.id = (SELECT f.id FROM billing_events f
                                      WHERE f.billing_id = e.billing_id AND f.pay_state = '{PAID}'
                                      ORDER BY f.processed_at, f.id LIMIT 1)
                         THEN 1 ELSE 0 END
             FROM billing_events e
             LEFT JOIN users u ON u.id = e.user_id
             LEFT JOIN billing_subscriptions b ON b.id = e.billing_id
             WHERE (?2 IS NULL OR e.user_id = ?2)
               AND (?3 IS NULL OR
                    (?3 = 'paid' AND e.pay_state = '{PAID}') OR
                    (?3 = 'failed' AND e.pay_state NOT IN ('{PAID}','1','10')))
             ORDER BY e.processed_at DESC, e.id DESC LIMIT ?1"
        ))?;
        let rows = st.query_map(params![limit, user_id, kind], |r| {
            let pay_state: String = r.get(5)?;
            let first: i64 = r.get(9)?;
            let kind = kind_of(&pay_state);
            Ok(PaymentRow {
                at: r.get(0)?,
                user_id: r.get(1)?,
                email: r.get(2)?,
                plan_id: r.get(3)?,
                amount_krw: r.get(4)?,
                is_first: (kind == "paid").then_some(first == 1),
                pay_state,
                kind: kind.to_string(),
                pay_type: r.get(6)?,
                outcome: r.get(7)?,
                provider_ref: mask_ref(r.get(8)?),
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // --- users ------------------------------------------------------------

    /// One page of accounts, with everything the list column needs.
    ///
    /// One statement. The alternative — a query per user for storage, for
    /// channels, for broadcasts — is fifty round trips per page on a server
    /// whose other job is pushing video.
    pub fn admin_users(
        &self,
        search: Option<&str>,
        filter: Option<&str>,
        sort: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<AdminUserRow>> {
        let limit = limit.clamp(1, MAX_PAGE);
        let order = match sort.unwrap_or("created") {
            "storage" => "COALESCE(m.bytes, 0) DESC",
            "plan" => "p.sort_order, u.created_at DESC",
            "email" => "u.email COLLATE NOCASE",
            "active" => "COALESCE(b.last_heartbeat, u.created_at) DESC",
            _ => "u.created_at DESC",
        };
        let having = match filter.unwrap_or("all") {
            "paid" => "AND u.plan_id <> 'none'",
            "none" => "AND u.plan_id = 'none' AND g.plan_id IS NULL",
            "basic" | "pro" | "business" => "AND u.plan_id = :plan",
            "disabled" => "AND u.disabled_at IS NOT NULL",
            "enabled" => "AND u.disabled_at IS NULL",
            "youtube" => "AND yt.n > 0",
            "live" => "AND live.n > 0",
            // Entitlement an operator handed out, and when it runs out. The
            // last two look past the live window on purpose: an operator
            // chasing a cohort wants the ones that have already lapsed too.
            "granted" => "AND g.plan_id IS NOT NULL",
            "grant_7d" => "AND g.expires_at <= datetime('now', '+7 days')",
            "grant_30d" => "AND g.expires_at <= datetime('now', '+30 days')",
            "grant_expired" => {
                "AND g.plan_id IS NULL AND EXISTS (SELECT 1 FROM admin_grants x
                   WHERE x.user_id = u.id AND x.revoked_at IS NULL
                     AND x.expires_at <= datetime('now'))"
            }
            _ => "",
        }
        .replace(":plan", &format!("'{}'", filter.unwrap_or("all")));

        // Every plan, once. `plan_rank` needs the limits of both sides and a
        // page is up to a hundred rows; four plans fetched once beats two
        // hundred lookups.
        let catalog: std::collections::HashMap<String, crate::Plan> =
            self.all_plans()?.into_iter().map(|p| (p.id.clone(), p)).collect();

        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let sql = format!(
            "SELECT u.id, u.email, u.name, u.created_at, COALESCE(u.role,'user'), u.disabled_at,
                    u.plan_id, COALESCE(p.label, u.plan_id), COALESCE(s.status, 'active'),
                    bs.status, bs.current_period_end,
                    COALESCE(m.bytes, 0), COALESCE(p.limits, '{{}}'),
                    COALESCE(yt.n, 0), COALESCE(live.n, 0), COALESCE(paid.krw, 0),
                    g.plan_id, g.expires_at,
                    CAST(julianday(g.expires_at) - julianday('now') AS INTEGER)
             FROM users u
             LEFT JOIN plans p ON p.id = u.plan_id
             LEFT JOIN subscriptions s ON s.user_id = u.id
             LEFT JOIN (SELECT user_id, SUM(size_bytes) bytes FROM media GROUP BY user_id) m
                    ON m.user_id = u.id
             LEFT JOIN (SELECT user_id, COUNT(*) n FROM youtube_accounts GROUP BY user_id) yt
                    ON yt.user_id = u.id
             LEFT JOIN (SELECT user_id, COUNT(*) n FROM broadcasts WHERE desired_state='running'
                        GROUP BY user_id) live ON live.user_id = u.id
             LEFT JOIN (SELECT user_id, SUM(amount_krw) krw FROM billing_events
                        WHERE pay_state='{PAID}' GROUP BY user_id) paid ON paid.user_id = u.id
             LEFT JOIN (SELECT user_id, MAX(last_heartbeat) last_heartbeat FROM broadcasts
                        GROUP BY user_id) b ON b.user_id = u.id
             LEFT JOIN (SELECT user_id, status, current_period_end FROM billing_subscriptions
                        GROUP BY user_id HAVING MAX(created_at)) bs ON bs.user_id = u.id
             -- The grant in force, if any. Strongest first by the same measure
             -- `grants::plan_rank` uses, then the one that ends latest.
             LEFT JOIN (SELECT user_id, plan_id, expires_at FROM admin_grants
                        WHERE revoked_at IS NULL
                          AND starts_at <= datetime('now')
                          AND expires_at > datetime('now')
                        GROUP BY user_id
                        HAVING MAX(expires_at)) g ON g.user_id = u.id
             WHERE (?3 IS NULL OR u.email LIKE ?3 COLLATE NOCASE OR u.name LIKE ?3 COLLATE NOCASE)
             {having}
             ORDER BY {order} LIMIT ?1 OFFSET ?2"
        );
        let mut st = guard.prepare(&sql)?;
        let like = search.map(|s| format!("%{}%", s.trim()));
        let rows = st.query_map(params![limit, offset.max(0), like], |r| {
            let limits: String = r.get(12)?;
            let storage_limit = serde_json::from_str::<serde_json::Value>(&limits)
                .ok()
                .and_then(|v| v[crate::entitlement::MAX_STORAGE_BYTES].as_i64())
                .unwrap_or(0);
            // Which source is in force, decided by the one function that
            // decides it anywhere — `grants::plan_rank`, against plans loaded
            // once above rather than a query per row. A second implementation
            // of this comparison is how a list and a gate come to disagree.
            let grant_plan: Option<String> = r.get(16)?;
            let paid_id: String = r.get(6)?;
            let paid_label: String = r.get(7)?;
            let paid = catalog.get(&paid_id);
            let granted = grant_plan.as_ref().and_then(|g| catalog.get(g));
            let use_grant = match (granted, paid) {
                (Some(g), Some(p)) => crate::grants::plan_rank(g) > crate::grants::plan_rank(p),
                (Some(_), None) => true,
                _ => false,
            };
            let (eff_id, eff_label, source) = match (use_grant, granted) {
                (true, Some(g)) => (g.id.clone(), g.label.clone(), "grant"),
                _ if paid.is_some_and(|p| p.can_broadcast()) => (paid_id.clone(), paid_label.clone(), "paid"),
                _ if grant_plan.is_some() => (paid_id.clone(), paid_label.clone(), "grant"),
                _ => (paid_id.clone(), paid_label.clone(), "none"),
            };
            // The ceiling shown has to be the one in force, or the list would
            // say 15GB for an account currently allowed 60.
            let storage_limit = match (use_grant, granted) {
                (true, Some(g)) => {
                    g.limits.get(crate::entitlement::MAX_STORAGE_BYTES).copied().unwrap_or(storage_limit)
                }
                _ => storage_limit,
            };
            Ok(AdminUserRow {
                id: r.get(0)?,
                email: r.get(1)?,
                name: r.get(2)?,
                created_at: r.get(3)?,
                role: r.get(4)?,
                disabled_at: r.get(5)?,
                plan_id: paid_id,
                plan_label: paid_label,
                effective_plan_id: eff_id,
                effective_plan_label: eff_label,
                entitlement_source: source.to_string(),
                grant_plan_id: grant_plan,
                grant_expires_at: r.get(17)?,
                grant_days_left: r.get(18)?,
                subscription_status: r.get(8)?,
                billing_status: r.get(9)?,
                next_charge_at: r.get(10)?,
                storage_bytes: r.get(11)?,
                storage_limit_bytes: storage_limit,
                youtube_accounts: r.get(13)?,
                running_broadcasts: r.get(14)?,
                total_paid_krw: r.get(15)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn admin_user(&self, user_id: &str) -> Result<AdminUserDetail> {
        let row = self.admin_users(None, None, None, MAX_PAGE, 0)?.into_iter().find(|u| u.id == user_id);
        let user = match row {
            Some(u) => u,
            // Beyond the first page, or filtered out: ask for this one directly.
            None => {
                let email: String = self
                    .raw()
                    .lock()
                    .unwrap()
                    .query_row("SELECT email FROM users WHERE id=?1", [user_id], |r| r.get(0))
                    .optional()?
                    .ok_or(CloudError::NotFound("user"))?;
                self.admin_users(Some(&email), None, None, MAX_PAGE, 0)?
                    .into_iter()
                    .find(|u| u.id == user_id)
                    .ok_or(CloudError::NotFound("user"))?
            }
        };
        let payments = self.payments(Some(user_id), 50, None)?;
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let media_count: i64 =
            guard.query_row("SELECT COUNT(*) FROM media WHERE user_id=?1", [user_id], |r| r.get(0))?;
        let (payment_count, first_paid_at, last_paid_at): (i64, Option<String>, Option<String>) = guard
            .query_row(
                &format!(
                    "SELECT COUNT(*), MIN(processed_at), MAX(processed_at) FROM billing_events
                     WHERE user_id=?1 AND pay_state='{PAID}'"
                ),
                [user_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
        let mut st = guard.prepare("SELECT channel_title FROM youtube_accounts WHERE user_id=?1")?;
        let youtube_channels: Vec<String> =
            st.query_map([user_id], |r| r.get(0))?.collect::<std::result::Result<Vec<_>, _>>()?;
        drop(st);
        drop(guard);
        Ok(AdminUserDetail {
            user,
            media_count,
            payment_count,
            first_paid_at,
            last_paid_at,
            payments,
            billing: self.billing_subscriptions_for(user_id)?,
            grants: self.grants_for(user_id)?,
            youtube_channels,
            broadcasts: self.admin_broadcasts(Some(user_id), None, MAX_PAGE, 0)?,
        })
    }

    // --- broadcasts -------------------------------------------------------

    pub fn admin_broadcasts(
        &self,
        user_id: Option<&str>,
        filter: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<AdminBroadcastRow>> {
        let limit = limit.clamp(1, MAX_PAGE);
        let where_state = match filter.unwrap_or("all") {
            "running" => "AND b.desired_state = 'running'",
            "scheduled" => "AND b.sched_enabled = 1 AND b.desired_state <> 'running'",
            "stopped" => "AND b.desired_state = 'stopped'",
            "failed" => "AND b.runtime_state = 'FAILED'",
            _ => "",
        };
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let mut st = guard.prepare(&format!(
            "SELECT b.id, b.name, b.user_id, u.email, b.desired_state, b.runtime_state,
                    b.restart_count, b.started_at, b.stopped_at, b.last_heartbeat, b.last_error,
                    COALESCE(b.sched_enabled, 0), b.loop_forever,
                    COALESCE(i.n, 0), yt.channel_title, b.youtube_status
             FROM broadcasts b
             JOIN users u ON u.id = b.user_id
             LEFT JOIN (SELECT broadcast_id, COUNT(*) n FROM broadcast_items GROUP BY broadcast_id) i
                    ON i.broadcast_id = b.id
             LEFT JOIN youtube_accounts yt ON yt.id = b.youtube_account_id
             WHERE (?3 IS NULL OR b.user_id = ?3) {where_state}
             ORDER BY (b.desired_state = 'running') DESC, b.created_at DESC
             LIMIT ?1 OFFSET ?2"
        ))?;
        let rows = st.query_map(params![limit, offset.max(0), user_id], |r| {
            Ok(AdminBroadcastRow {
                id: r.get(0)?,
                name: r.get(1)?,
                user_id: r.get(2)?,
                email: r.get(3)?,
                desired_state: r.get(4)?,
                runtime_state: r.get(5)?,
                restart_count: r.get(6)?,
                started_at: r.get(7)?,
                stopped_at: r.get(8)?,
                last_heartbeat: r.get(9)?,
                last_error: r.get(10)?,
                scheduled: r.get::<_, i64>(11)? != 0,
                loop_forever: r.get::<_, i64>(12)? != 0,
                item_count: r.get(13)?,
                youtube_channel: r.get(14)?,
                youtube_status: r.get(15)?,
                worker_alive: false,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // --- billing ----------------------------------------------------------

    pub fn cancellations(&self, limit: i64) -> Result<Vec<CancellationRow>> {
        let limit = limit.clamp(1, MAX_PAGE);
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let mut st = guard.prepare(
            "SELECT b.user_id, u.email, b.plan_id, b.cancelled_at, b.last_paid_at, b.amount_krw,
                    u.plan_id, COALESCE(s.status,'active'),
                    (SELECT COUNT(*) FROM broadcasts r WHERE r.user_id=b.user_id
                     AND r.desired_state='running')
             FROM billing_subscriptions b
             JOIN users u ON u.id = b.user_id
             LEFT JOIN subscriptions s ON s.user_id = b.user_id
             WHERE b.status = 'cancelled'
             ORDER BY b.cancelled_at DESC LIMIT ?1",
        )?;
        let rows = st.query_map([limit], |r| {
            let entitlement_plan: String = r.get(6)?;
            let status: String = r.get(7)?;
            Ok(CancellationRow {
                user_id: r.get(0)?,
                email: r.get(1)?,
                plan_id: r.get(2)?,
                cancelled_at: r.get(3)?,
                last_paid_at: r.get(4)?,
                amount_krw: r.get(5)?,
                entitlement_active: status == crate::db::SUBSCRIPTION_ACTIVE
                    && entitlement_plan != crate::db::UNSUBSCRIBED_PLAN,
                entitlement_plan,
                running_broadcasts: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // --- storage ----------------------------------------------------------

    pub fn storage_leaders(&self, limit: i64) -> Result<Vec<StorageUser>> {
        let limit = limit.clamp(1, MAX_PAGE);
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let mut st = guard.prepare(
            "SELECT u.id, u.email, u.plan_id, COALESCE(SUM(m.size_bytes),0), COUNT(m.id),
                    COALESCE(p.limits, '{}')
             FROM users u
             LEFT JOIN media m ON m.user_id = u.id
             LEFT JOIN plans p ON p.id = u.plan_id
             GROUP BY u.id HAVING SUM(m.size_bytes) > 0
             ORDER BY 4 DESC LIMIT ?1",
        )?;
        let rows = st.query_map([limit], |r| {
            let limits: String = r.get(5)?;
            Ok(StorageUser {
                user_id: r.get(0)?,
                email: r.get(1)?,
                plan_id: r.get(2)?,
                bytes: r.get(3)?,
                files: r.get(4)?,
                limit_bytes: serde_json::from_str::<serde_json::Value>(&limits)
                    .ok()
                    .and_then(|v| v[crate::entitlement::MAX_STORAGE_BYTES].as_i64())
                    .unwrap_or(0),
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn youtube_account_count(&self) -> Result<i64> {
        self.scalar("SELECT COUNT(*) FROM youtube_accounts")
    }

    pub fn total_storage_bytes(&self) -> Result<i64> {
        self.scalar("SELECT COALESCE(SUM(size_bytes),0) FROM media")
    }

    /// The operational events worth showing on a status page, newest first.
    ///
    /// Bounded by the query, drawn from the per-broadcast event table the
    /// service already keeps. Nothing new is stored for this.
    pub fn recent_problems(&self, limit: i64) -> Result<Vec<ProblemRow>> {
        let limit = limit.clamp(1, MAX_PAGE);
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let mut st = guard.prepare(
            "SELECT e.at, e.level, e.message, b.id, b.name, u.email
             FROM broadcast_events e
             JOIN broadcasts b ON b.id = e.broadcast_id
             JOIN users u ON u.id = b.user_id
             WHERE e.level IN ('error', 'warn')
             ORDER BY e.id DESC LIMIT ?1",
        )?;
        let rows = st.query_map([limit], |r| {
            Ok(ProblemRow {
                at: r.get(0)?,
                level: r.get(1)?,
                // The engine masks stream keys before a line is stored; this is
                // the second pass, over anything else that looks like a secret.
                message: redact(&r.get::<_, String>(2)?),
                broadcast_id: r.get(3)?,
                broadcast_name: r.get(4)?,
                email: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProblemRow {
    pub at: String,
    pub level: String,
    pub message: String,
    pub broadcast_id: String,
    pub broadcast_name: String,
    pub email: String,
}

/// What a list endpoint was asked for. Clamped, always.
#[derive(Debug, Clone, Deserialize)]
pub struct Page {
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

impl Page {
    pub fn limit(&self) -> i64 {
        self.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE)
    }
    pub fn offset(&self) -> i64 {
        self.offset.unwrap_or(0).max(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_cannot_carry_a_credential_into_the_log() {
        assert_eq!(redact("GOCSPX-abcdefghijklmnop was here"), "[REDACTED] was here");
        assert_eq!(redact("token ya29.averylongaccesstokenvalue"), "token [REDACTED]");
        assert_eq!(redact(&format!("key {}", "a".repeat(40))), "key [REDACTED]");
        // Ordinary words, an email and a short id are left alone.
        assert_eq!(redact("환불 요청 dj@example.com 2026-09-29"), "환불 요청 dj@example.com 2026-09-29");
    }

    #[test]
    fn a_provider_reference_is_shown_as_its_last_four() {
        assert_eq!(mask_ref(Some("8891234".into())).as_deref(), Some("…1234"));
        assert_eq!(mask_ref(Some("12".into())).as_deref(), Some("…"));
        assert_eq!(mask_ref(None), None);
    }

    #[test]
    fn the_ledgers_states_are_named_the_way_the_provider_spells_them() {
        assert_eq!(kind_of("4"), "paid");
        assert_eq!(kind_of("9"), "reversal");
        assert_eq!(kind_of("64"), "reversal");
        assert_eq!(kind_of("1"), "waiting");
        assert_eq!(kind_of("99"), "other");
    }
}
