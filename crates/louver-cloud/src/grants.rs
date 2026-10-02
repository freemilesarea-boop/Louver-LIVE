//! Entitlement an operator hands out, kept entirely apart from the one people pay for.
//!
//! A manual grant is a row saying "this account may use this plan between these
//! two instants, because an operator said so". It is **not** a payment, not a
//! subscription, and not a plan change: `users.plan_id`, `subscriptions`,
//! `billing_subscriptions` and `billing_events` are never written from here.
//! That separation is the whole design, and it is what keeps a free month for a
//! student out of the revenue figures — the money tables simply do not know
//! this table exists.
//!
//! Two consequences worth stating, because both are deliberate:
//!
//! * **A grant cannot lower anything.** The plan in force is the strongest of
//!   the sources that currently apply, so handing a Business customer a Basic
//!   grant changes nothing for them. See [`plan_rank`].
//! * **A grant cannot touch a recurring payment.** When one expires, PayApp is
//!   not told, nothing is cancelled, and the account falls back to whatever it
//!   was already paying for. When a subscription lapses, a live grant still
//!   carries the account to the end of its term.
//!
//! There is no `status` column. The state of a grant is *derived* from its two
//! instants and its `revoked_at`, every time it is asked for, so there is no
//! second copy of the truth to drift out of date and no cron whose failure
//! silently leaves an expired grant working. See [`GrantState`].

use crate::db::CloudDb;
use crate::models::Plan;
use crate::{CloudError, Result};
use rusqlite::{params, OptionalExtension};
use serde::Serialize;

/// The most accounts one request may grant to.
///
/// A bulk grant is a single transaction, and a transaction that touches
/// thousands of rows holds the write lock for as long as it takes — on a
/// two-core server also running broadcasts. A hundred is more than any real
/// cohort and small enough to be instant.
pub const MAX_BULK_TARGETS: usize = 100;

/// The longest term a grant may be written for, in days.
///
/// Not a policy about generosity — a guard against a typo. "3650" where "365"
/// was meant is a decade of free Business that nobody would notice, and the
/// operator who genuinely wants that can write two grants.
pub const MAX_GRANT_DAYS: i64 = 400;

/// Where a grant is in its life, worked out rather than stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantState {
    /// Written for a start that has not arrived.
    Scheduled,
    /// In force now. The only state that grants anything.
    Active,
    /// Its term ended. Nothing had to run for this to happen.
    Expired,
    /// An operator took it back before its term ended.
    Revoked,
}

impl GrantState {
    pub fn id(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Active => "active",
            Self::Expired => "expired",
            Self::Revoked => "revoked",
        }
    }

    /// Korean, for the console.
    pub fn label(self) -> &'static str {
        match self {
            Self::Scheduled => "시작 전",
            Self::Active => "활성",
            Self::Expired => "만료",
            Self::Revoked => "회수됨",
        }
    }

    pub fn is_active(self) -> bool {
        self == Self::Active
    }
}

/// One grant, as the console shows it.
#[derive(Debug, Clone, Serialize)]
pub struct Grant {
    pub id: String,
    pub user_id: String,
    pub email: String,
    pub plan_id: String,
    pub plan_label: String,
    pub starts_at: String,
    pub expires_at: String,
    pub reason: String,
    pub granted_by_email: String,
    pub revoked_at: Option<String>,
    pub revoked_by_email: Option<String>,
    pub revoke_reason: Option<String>,
    /// Set when an extension or a plan change replaced this row. The old row is
    /// kept so the history reads as what actually happened.
    pub superseded_by: Option<String>,
    /// Which bulk run wrote it, when one did.
    pub batch_id: Option<String>,
    pub created_at: String,
    pub state: GrantState,
    /// Whole days until `expires_at`, negative once past. For a `D-29` badge.
    pub days_left: i64,
}

/// How long a grant runs for.
///
/// Two shapes, because the console offers two: a preset number of days from
/// now, and a pair of dates an operator typed. They are resolved to UTC
/// instants in one place — [`CloudDb::resolve_term`] — so no caller has to know
/// about the nine hours.
#[derive(Debug, Clone)]
pub enum Term {
    /// This many days, starting now.
    Days(i64),
    /// Seoul-local dates, `YYYY-MM-DD`. Inclusive at both ends: a grant written
    /// `to` the 1st covers all of the 1st in Seoul and ends as the 2nd begins,
    /// which is what an operator reading "만료일 11/01" expects.
    Between { from: String, to: String },
}

/// What an operator asked for.
#[derive(Debug, Clone)]
pub struct NewGrant<'a> {
    pub plan_id: &'a str,
    pub term: Term,
    pub reason: &'a str,
}

/// What to do about an account that already holds a live grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnExisting {
    /// Add the new term to the end of the one already running.
    Extend,
    /// Throw the remaining term away and start a fresh one now.
    Reset,
    /// Leave the account alone and report it.
    Skip,
}

impl OnExisting {
    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "extend" => Some(Self::Extend),
            "reset" => Some(Self::Reset),
            "skip" => Some(Self::Skip),
            _ => None,
        }
    }
}

/// What one bulk run did, per account.
#[derive(Debug, Clone, Serialize)]
pub struct GrantOutcome {
    pub user_id: String,
    pub email: String,
    /// `created`, `extended`, `reset` or `skipped`.
    pub action: String,
    pub grant_id: Option<String>,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BulkGrantResult {
    pub batch_id: String,
    pub plan_id: String,
    pub plan_label: String,
    pub outcomes: Vec<GrantOutcome>,
}

/// How many live grants there are, by plan, and how many end soon.
#[derive(Debug, Clone, Default, Serialize)]
pub struct GrantCounts {
    pub active: i64,
    pub by_plan: Vec<(String, String, i64)>,
    pub expiring_7d: i64,
    pub scheduled: i64,
}

/// Which of two plans is the stronger.
///
/// Ranked by what the plans actually allow rather than by name or by
/// `sort_order` — the first would put the policy in a string comparison, and the
/// second is a display order in which the unsubscribed plan sorts *last* (99).
///
/// The tuple is compared left to right: being able to broadcast at all, then
/// concurrent streams — which this product's own code calls "the entitlement
/// that distinguishes the paid plans" — then storage, then the per-file
/// ceiling, then price. For the three plans on sale every component agrees
/// (1 < 2 < 3), so there is nothing to disambiguate today; the later components
/// are there so a fourth plan, or one customer's raised ceiling, still orders
/// sensibly instead of tying.
pub fn plan_rank(p: &Plan) -> (bool, i64, i64, i64, i64) {
    let l = |k: &str| p.limits.get(k).copied().unwrap_or(0);
    (
        p.can_broadcast(),
        l(crate::entitlement::MAX_CONCURRENT_STREAMS),
        l(crate::entitlement::MAX_STORAGE_BYTES),
        l(crate::entitlement::MAX_UPLOAD_BYTES),
        p.monthly_price_krw,
    )
}

/// A [`Term`] as the two UTC instants the database stores.
///
/// Seoul is UTC+9 all year, so a Seoul day begins nine hours earlier in UTC and
/// that is the whole of the conversion. Done through SQLite rather than in Rust
/// because SQLite is what every comparison against these values runs in, so the
/// format cannot disagree with itself.
///
/// Validated here, before a single row is written: a window the wrong way round
/// and a term longer than [`MAX_GRANT_DAYS`] are both refused.
pub fn resolve_term(tx: &rusqlite::Connection, term: &Term) -> Result<(String, String)> {
    let (starts_at, expires_at): (String, String) = match term {
        Term::Days(n) => {
            if *n <= 0 {
                return Err(CloudError::Invalid("기간은 1일 이상이어야 합니다".into()));
            }
            if *n > MAX_GRANT_DAYS {
                return Err(CloudError::Invalid(format!(
                    "한 번에 지급할 수 있는 기간은 최대 {MAX_GRANT_DAYS}일입니다 ({n}일 요청)"
                )));
            }
            tx.query_row("SELECT datetime('now'), datetime('now', ?1)", params![format!("+{n} days")], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
        }
        Term::Between { from, to } => {
            if !is_plain_date(from) || !is_plain_date(to) {
                return Err(CloudError::Invalid("날짜는 YYYY-MM-DD 형식이어야 합니다".into()));
            }
            // A date that is well-formed but does not exist — 2026-02-31 — is
            // *normalised* by SQLite, not refused: it answers 2026-03-03. An
            // operator who typed the wrong month would get a grant running over
            // days they never chose, so the value is round-tripped and anything
            // SQLite had to move is refused.
            let round: Option<(Option<String>, Option<String>)> = tx
                .query_row("SELECT date(?1), date(?2)", params![from, to], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            match round {
                Some((Some(a), Some(b))) if a == *from && b == *to => {}
                _ => return Err(CloudError::Invalid(format!("존재하지 않는 날짜입니다 ({from} ~ {to})"))),
            }
            // `to` is inclusive, so the grant ends as the next Seoul day begins.
            tx.query_row(
                "SELECT datetime(?1, '-9 hours'), datetime(?2, '+1 day', '-9 hours')",
                params![from, to],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?
        }
    };
    if expires_at <= starts_at {
        return Err(CloudError::Invalid("만료일이 시작일보다 뒤여야 합니다".into()));
    }
    let days: f64 =
        tx.query_row("SELECT julianday(?2) - julianday(?1)", params![starts_at, expires_at], |r| r.get(0))?;
    if days > MAX_GRANT_DAYS as f64 {
        return Err(CloudError::Invalid(format!(
            "한 번에 지급할 수 있는 기간은 최대 {MAX_GRANT_DAYS}일입니다 ({days:.0}일 요청)"
        )));
    }
    Ok((starts_at, expires_at))
}

/// `YYYY-MM-DD` and nothing else.
pub fn is_plain_date(s: &str) -> bool {
    s.len() == 10
        && s.as_bytes().iter().enumerate().all(|(i, b)| match i {
            4 | 7 => *b == b'-',
            _ => b.is_ascii_digit(),
        })
}

impl CloudDb {
    /// The grant in force for this account right now, if there is one.
    ///
    /// "In force" is read from the instants on every call: not revoked, started,
    /// not yet expired. When an account somehow holds two, the stronger wins,
    /// then the one that ends later — so an extension written as a new row can
    /// never be weaker than what it replaced.
    pub fn active_grant_plan(&self, user_id: &str) -> Result<Option<Plan>> {
        let ids: Vec<String> = {
            let conn = self.raw();
            let guard = conn.lock().unwrap();
            let mut st = guard.prepare(
                "SELECT plan_id FROM admin_grants
                 WHERE user_id = ?1
                   AND revoked_at IS NULL
                   AND starts_at <= datetime('now')
                   AND expires_at > datetime('now')",
            )?;
            let rows = st.query_map([user_id], |r| r.get(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut best: Option<Plan> = None;
        for id in ids {
            let Ok(p) = self.plan(&id) else { continue };
            if best.as_ref().is_none() || best.as_ref().is_some_and(|b| plan_rank(&p) > plan_rank(b)) {
                best = Some(p);
            }
        }
        Ok(best)
    }

    /// The plan whose limits apply to this account, from every source.
    ///
    /// The paid side is `users.plan_id` exactly as it has always been read —
    /// deliberately without consulting the subscription's status, because that
    /// is how this has always behaved: a lapsed card leaves the limits in place
    /// and stops the account starting anything, and changing that here would be
    /// a silent change to every quota in the product.
    ///
    /// A live grant can only raise it.
    pub fn effective_plan(&self, user_id: &str) -> Result<Plan> {
        let paid = self.plan(&self.user(user_id)?.plan_id)?;
        match self.active_grant_plan(user_id)? {
            Some(g) if plan_rank(&g) > plan_rank(&paid) => Ok(g),
            _ => Ok(paid),
        }
    }

    /// Every grant this account has ever had, newest first.
    pub fn grants_for(&self, user_id: &str) -> Result<Vec<Grant>> {
        self.grant_rows("WHERE g.user_id = ?1", &[&user_id], MAX_GRANT_PAGE, 0)
    }

    /// One grant by id.
    pub fn grant(&self, id: &str) -> Result<Grant> {
        self.grant_rows("WHERE g.id = ?1", &[&id], 1, 0)?
            .into_iter()
            .next()
            .ok_or(CloudError::NotFound("grant"))
    }

    /// A page of grants for the console, newest first.
    ///
    /// `filter` is one of `active`, `scheduled`, `expiring_7d`, `expiring_30d`,
    /// `expired`, `revoked`; anything else lists everything.
    pub fn grants_page(&self, filter: Option<&str>, limit: i64, offset: i64) -> Result<Vec<Grant>> {
        let live = "g.revoked_at IS NULL AND g.starts_at <= datetime('now') \
                    AND g.expires_at > datetime('now')";
        let where_sql = match filter.unwrap_or("all") {
            "active" => format!("WHERE {live}"),
            "scheduled" => "WHERE g.revoked_at IS NULL AND g.starts_at > datetime('now')".into(),
            "expiring_7d" => {
                format!("WHERE {live} AND g.expires_at <= datetime('now', '+7 days')")
            }
            "expiring_30d" => {
                format!("WHERE {live} AND g.expires_at <= datetime('now', '+30 days')")
            }
            "expired" => "WHERE g.revoked_at IS NULL AND g.expires_at <= datetime('now')".into(),
            "revoked" => "WHERE g.revoked_at IS NOT NULL".into(),
            _ => String::new(),
        };
        self.grant_rows(&where_sql, &[], limit, offset)
    }

    /// Live grants by plan, and how many end within a week.
    ///
    /// For the dashboard. Deliberately its own figure: a grant is not revenue
    /// and must not be counted next to one.
    pub fn grant_counts(&self) -> Result<GrantCounts> {
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let live = "revoked_at IS NULL AND starts_at <= datetime('now') \
                    AND expires_at > datetime('now')";
        let active: i64 =
            guard.query_row(&format!("SELECT COUNT(*) FROM admin_grants WHERE {live}"), [], |r| r.get(0))?;
        let expiring_7d: i64 = guard.query_row(
            &format!(
                "SELECT COUNT(*) FROM admin_grants
                 WHERE {live} AND expires_at <= datetime('now', '+7 days')"
            ),
            [],
            |r| r.get(0),
        )?;
        let scheduled: i64 = guard.query_row(
            "SELECT COUNT(*) FROM admin_grants
             WHERE revoked_at IS NULL AND starts_at > datetime('now')",
            [],
            |r| r.get(0),
        )?;
        let mut st = guard.prepare(&format!(
            "SELECT g.plan_id, COALESCE(p.label, g.plan_id), COUNT(*)
             FROM admin_grants g LEFT JOIN plans p ON p.id = g.plan_id
             WHERE {live}
             GROUP BY g.plan_id ORDER BY COALESCE(p.sort_order, 0)"
        ))?;
        let by_plan = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(GrantCounts { active, by_plan, expiring_7d, scheduled })
    }

    /// Grant a plan to several accounts, all of them or none.
    ///
    /// One transaction. A list with one bad id in it writes nothing: an operator
    /// handing a cohort their month needs to know it either happened or did not,
    /// because "forty-one of forty-three" is a state nobody can act on.
    ///
    /// Nothing here writes `users`, `subscriptions`, `billing_subscriptions` or
    /// `billing_events`. A grant is additive to those, never a substitute.
    pub fn create_grants(
        &self,
        user_ids: &[String],
        new: &NewGrant<'_>,
        on_existing: OnExisting,
        admin_id: &str,
        admin_email: &str,
    ) -> Result<BulkGrantResult> {
        if user_ids.is_empty() {
            return Err(CloudError::Invalid("대상 회원을 선택해 주세요".into()));
        }
        if user_ids.len() > MAX_BULK_TARGETS {
            return Err(CloudError::Invalid(format!(
                "한 번에 최대 {MAX_BULK_TARGETS}명까지 지급할 수 있습니다 ({}명 요청)",
                user_ids.len()
            )));
        }
        let plan = self.plan(new.plan_id)?;
        if !plan.can_broadcast() {
            return Err(CloudError::Invalid("이 요금제로는 이용권을 지급할 수 없습니다".into()));
        }
        let reason = new.reason.trim();
        if reason.is_empty() {
            return Err(CloudError::Invalid("지급 사유를 입력해 주세요".into()));
        }

        let batch_id = crate::new_id();
        let conn = self.raw();
        let mut guard = conn.lock().unwrap();
        let tx = guard.transaction()?;

        // Resolve the whole list first, so a bad id refuses before anything is
        // written rather than halfway through.
        let mut targets: Vec<(String, String)> = Vec::with_capacity(user_ids.len());
        let mut seen = std::collections::HashSet::new();
        for uid in user_ids {
            if !seen.insert(uid.as_str()) {
                continue;
            }
            let found: Option<(String, Option<String>)> = tx
                .query_row("SELECT email, disabled_at FROM users WHERE id = ?1", [uid], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()?;
            let Some((email, disabled_at)) = found else {
                return Err(CloudError::Invalid(format!("존재하지 않는 회원이 포함되어 있습니다: {uid}")));
            };
            if disabled_at.is_some() {
                return Err(CloudError::Invalid(format!("비활성 계정에는 지급할 수 없습니다: {email}")));
            }
            targets.push((uid.clone(), email));
        }

        let (starts_at, expires_at) = resolve_term(&tx, &new.term)?;
        let days: f64 =
            tx.query_row("SELECT (julianday(?2) - julianday(?1))", params![starts_at, expires_at], |r| {
                r.get(0)
            })?;

        let mut outcomes = Vec::with_capacity(targets.len());
        for (uid, email) in targets {
            // The live grant this account already holds, if any. Strongest
            // first, then latest-ending, which is the one an extension means.
            let existing: Option<(String, String, String)> = tx
                .query_row(
                    "SELECT id, starts_at, expires_at FROM admin_grants
                     WHERE user_id = ?1 AND revoked_at IS NULL
                       AND starts_at <= datetime('now') AND expires_at > datetime('now')
                     ORDER BY expires_at DESC LIMIT 1",
                    [&uid],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;

            let (action, start, end, replaces) = match (&existing, on_existing) {
                (None, _) => ("created", starts_at.clone(), expires_at.clone(), None),
                (Some(_), OnExisting::Skip) => {
                    outcomes.push(GrantOutcome {
                        user_id: uid,
                        email,
                        action: "skipped".into(),
                        grant_id: None,
                        expires_at: None,
                    });
                    continue;
                }
                // Add this term to the end of the one running.
                //
                // The new row keeps the **original start**, not the old end.
                // Starting it where the old one stopped would leave the account
                // with a superseded row and a row that has not begun — an
                // entitlement gap from now until the old expiry, which is the
                // opposite of an extension.
                (Some((old_id, old_start, old_end)), OnExisting::Extend) => {
                    let end: String = tx.query_row(
                        "SELECT datetime(?1, ?2)",
                        params![old_end, format!("+{days} days")],
                        |r| r.get(0),
                    )?;
                    ("extended", old_start.clone(), end, Some(old_id.clone()))
                }
                // Throw the rest of the old term away and start again now.
                (Some((old_id, _, _)), OnExisting::Reset) => {
                    ("reset", starts_at.clone(), expires_at.clone(), Some(old_id.clone()))
                }
            };

            let id = crate::new_id();
            tx.execute(
                "INSERT INTO admin_grants
                   (id, user_id, plan_id, starts_at, expires_at, reason,
                    granted_by, granted_by_email, batch_id)
                 VALUES (?1, ?2, ?3, datetime(?4), datetime(?5), ?6, ?7, ?8, ?9)",
                params![id, uid, plan.id, start, end, reason, admin_id, admin_email, batch_id],
            )?;
            // The row it replaces is kept and pointed at the new one, so the
            // history shows an extension rather than an edit nobody can see.
            if let Some(old) = replaces {
                tx.execute(
                    "UPDATE admin_grants
                     SET superseded_by = ?2, revoked_at = datetime('now'),
                         revoked_by = ?3, revoke_reason = ?4, updated_at = datetime('now')
                     WHERE id = ?1",
                    params![
                        old,
                        id,
                        admin_id,
                        format!(
                            "{} (이용권 {})",
                            reason,
                            if action == "extended" { "연장" } else { "재설정" }
                        )
                    ],
                )?;
            }
            outcomes.push(GrantOutcome {
                user_id: uid,
                email,
                action: action.into(),
                grant_id: Some(id),
                expires_at: Some(end),
            });
        }
        tx.commit()?;
        Ok(BulkGrantResult { batch_id, plan_id: plan.id, plan_label: plan.label, outcomes })
    }

    /// Push one grant's end further out, keeping the row that was there.
    pub fn extend_grant(
        &self,
        id: &str,
        days: i64,
        admin_id: &str,
        admin_email: &str,
        reason: &str,
    ) -> Result<Grant> {
        if days <= 0 || days > MAX_GRANT_DAYS {
            return Err(CloudError::Invalid(format!("기간은 1일에서 {MAX_GRANT_DAYS}일 사이여야 합니다")));
        }
        let old = self.grant(id)?;
        if old.revoked_at.is_some() {
            return Err(CloudError::Invalid("회수된 이용권은 연장할 수 없습니다".into()));
        }
        let _ = admin_email;
        self.supersede(&old, &old.plan_id.clone(), days, reason, admin_id, "연장")?;
        self.active_or_latest_for(&old.user_id)
    }

    /// Move one grant to another plan, keeping its term and the row it replaces.
    pub fn change_grant_plan(
        &self,
        id: &str,
        plan_id: &str,
        admin_id: &str,
        admin_email: &str,
        reason: &str,
    ) -> Result<Grant> {
        let old = self.grant(id)?;
        if old.revoked_at.is_some() {
            return Err(CloudError::Invalid("회수된 이용권은 변경할 수 없습니다".into()));
        }
        let plan = self.plan(plan_id)?;
        if !plan.can_broadcast() {
            return Err(CloudError::Invalid("이 요금제로는 이용권을 지급할 수 없습니다".into()));
        }
        let _ = admin_email;
        self.supersede(&old, &plan.id, 0, reason, admin_id, "변경")?;
        self.active_or_latest_for(&old.user_id)
    }

    /// Write the replacement row and retire the old one, in one transaction.
    ///
    /// Keeps the original's start, so an extension reads as the same term made
    /// longer rather than as a new grant that happens to overlap.
    fn supersede(
        &self,
        old: &Grant,
        plan_id: &str,
        add_days: i64,
        reason: &str,
        admin_id: &str,
        what: &str,
    ) -> Result<()> {
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(CloudError::Invalid("사유를 입력해 주세요".into()));
        }
        let conn = self.raw();
        let mut guard = conn.lock().unwrap();
        let tx = guard.transaction()?;
        let id = crate::new_id();
        tx.execute(
            "INSERT INTO admin_grants
               (id, user_id, plan_id, starts_at, expires_at, reason,
                granted_by, granted_by_email, batch_id)
             VALUES (?1, ?2, ?3, ?4, datetime(?5, ?6), ?7, ?8,
                     (SELECT COALESCE(email,'') FROM users WHERE id = ?8), ?9)",
            params![
                id,
                old.user_id,
                plan_id,
                old.starts_at,
                old.expires_at,
                format!("+{add_days} days"),
                reason,
                admin_id,
                old.batch_id,
            ],
        )?;
        tx.execute(
            "UPDATE admin_grants
             SET superseded_by = ?2, revoked_at = datetime('now'),
                 revoked_by = ?3, revoke_reason = ?4, updated_at = datetime('now')
             WHERE id = ?1",
            params![old.id, id, admin_id, format!("{reason} (이용권 {what})")],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The grant in force for an account, or the most recent one.
    fn active_or_latest_for(&self, user_id: &str) -> Result<Grant> {
        let all = self.grants_for(user_id)?;
        all.iter()
            .find(|g| g.state.is_active())
            .or_else(|| all.first())
            .cloned()
            .ok_or(CloudError::NotFound("grant"))
    }

    /// Take a grant back.
    ///
    /// The row stays, with who took it and why. **No payment is touched**: an
    /// account that is also paying returns to the plan it pays for, and one that
    /// is not returns to unsubscribed — both of which happen on their own,
    /// because the plan in force is computed and this row stops counting.
    pub fn revoke_grant(&self, id: &str, admin_id: &str, admin_email: &str, reason: &str) -> Result<Grant> {
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(CloudError::Invalid("회수 사유를 입력해 주세요".into()));
        }
        let before = self.grant(id)?;
        if before.revoked_at.is_some() {
            return Err(CloudError::Invalid("이미 회수된 이용권입니다".into()));
        }
        let _ = admin_email;
        self.raw().lock().unwrap().execute(
            "UPDATE admin_grants
             SET revoked_at = datetime('now'), revoked_by = ?2, revoke_reason = ?3,
                 updated_at = datetime('now')
             WHERE id = ?1 AND revoked_at IS NULL",
            params![id, admin_id, reason],
        )?;
        self.grant(id)
    }

    /// Shared row reader. `where_sql` already carries its own `WHERE`.
    fn grant_rows(
        &self,
        where_sql: &str,
        binds: &[&dyn rusqlite::ToSql],
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Grant>> {
        let limit = limit.clamp(1, MAX_GRANT_PAGE);
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let sql = format!(
            "SELECT g.id, g.user_id, COALESCE(u.email, ''), g.plan_id,
                    COALESCE(p.label, g.plan_id), g.starts_at, g.expires_at, g.reason,
                    g.granted_by_email, g.revoked_at,
                    (SELECT email FROM users WHERE id = g.revoked_by), g.revoke_reason,
                    g.superseded_by, g.batch_id, g.created_at,
                    CASE
                      WHEN g.revoked_at IS NOT NULL THEN 'revoked'
                      WHEN g.starts_at > datetime('now') THEN 'scheduled'
                      WHEN g.expires_at <= datetime('now') THEN 'expired'
                      ELSE 'active'
                    END,
                    CAST(julianday(g.expires_at) - julianday('now') AS INTEGER)
             FROM admin_grants g
             LEFT JOIN users u ON u.id = g.user_id
             LEFT JOIN plans p ON p.id = g.plan_id
             {where_sql}
             ORDER BY g.created_at DESC, g.id DESC
             LIMIT {limit} OFFSET {}",
            offset.max(0)
        );
        let mut st = guard.prepare(&sql)?;
        let rows = st.query_map(binds, |r| {
            let state: String = r.get(15)?;
            Ok(Grant {
                id: r.get(0)?,
                user_id: r.get(1)?,
                email: r.get(2)?,
                plan_id: r.get(3)?,
                plan_label: r.get(4)?,
                starts_at: r.get(5)?,
                expires_at: r.get(6)?,
                reason: r.get(7)?,
                granted_by_email: r.get(8)?,
                revoked_at: r.get(9)?,
                revoked_by_email: r.get(10)?,
                revoke_reason: r.get(11)?,
                superseded_by: r.get(12)?,
                batch_id: r.get(13)?,
                created_at: r.get(14)?,
                state: match state.as_str() {
                    "revoked" => GrantState::Revoked,
                    "scheduled" => GrantState::Scheduled,
                    "expired" => GrantState::Expired,
                    _ => GrantState::Active,
                },
                days_left: r.get(16)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}

/// The most grants one listing returns.
pub const MAX_GRANT_PAGE: i64 = 200;
