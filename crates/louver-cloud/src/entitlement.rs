//! Plan limits, enforced where they cannot be talked out of.
//!
//! §3 is explicit that a limit checked only in the browser is not a limit. Every
//! function here runs against the database, and the one that matters —
//! [`claim_stream_slot`] — does its counting and its writing inside one
//! `BEGIN IMMEDIATE` transaction, so two requests arriving together cannot both
//! be told there is room for the last slot.
//!
//! Nothing here branches on a plan's name. Limits are looked up by key, which is
//! what lets a fourth plan, or one customer's raised ceiling, be a row.
//!
//! There are two questions, not one. *Is there a subscription* is asked by
//! [`CloudDb::require_active_subscription`]; *how much does it allow* is asked of
//! the plan's limits. Keeping them apart is what lets an account with no plan be
//! told "요금제가 필요합니다" instead of "0개 중 0개를 사용했습니다".

use crate::db::CloudDb;
use crate::models::{DesiredState, RuntimeState};
use crate::{CloudError, Result};
use rusqlite::{params, TransactionBehavior};

/// Limit keys. Spelt once so a typo is a compile error, not a missing limit.
pub const MAX_CONCURRENT_STREAMS: &str = "max_concurrent_streams";
pub const MAX_BROADCASTS: &str = "max_broadcasts";
pub const MAX_STORAGE_BYTES: &str = "max_storage_bytes";
pub const MAX_UPLOAD_BYTES: &str = "max_upload_bytes";
pub const SCHEDULING_ENABLED: &str = "scheduling_enabled";

impl CloudDb {
    /// One limit for one user.
    ///
    /// A limit absent from the plan is zero, not unlimited. Forgetting to write
    /// a limit must fail closed.
    ///
    /// The plan is whichever currently applies — what the account pays for, or
    /// a stronger one an operator granted. With no grant in force this is
    /// `users.plan_id`, exactly as it has always been, so no existing account's
    /// limits move. See [`CloudDb::effective_plan`].
    pub fn limit(&self, user_id: &str, key: &str) -> Result<i64> {
        Ok(self.effective_plan(user_id)?.limits.get(key).copied().unwrap_or(0))
    }

    pub fn flag(&self, user_id: &str, key: &str) -> Result<bool> {
        Ok(self.limit(user_id, key)? != 0)
    }

    /// How many of this user's broadcasts are holding a slot right now.
    pub fn active_stream_count(&self, user_id: &str) -> Result<i64> {
        let states: Vec<&str> = [
            RuntimeState::Preparing,
            RuntimeState::Starting,
            RuntimeState::Running,
            RuntimeState::Reconnecting,
        ]
        .iter()
        .map(|s| s.id())
        .collect();
        let list = states.join("','");
        let sql = format!(
            "SELECT COUNT(*) FROM broadcasts
             WHERE user_id=?1 AND (desired_state='running' OR runtime_state IN ('{list}'))"
        );
        Ok(self.raw().lock().unwrap().query_row(&sql, [user_id], |r| r.get(0))?)
    }

    /// Take a concurrency slot for `broadcast_id`, or refuse.
    ///
    /// The count and the write are one transaction, opened `IMMEDIATE` so SQLite
    /// takes the write lock before reading rather than at first write. Without
    /// that, two `start` requests could both count two running streams on a
    /// three-stream plan, both decide there is room, and both start — the exact
    /// hole §3 asks to be closed.
    ///
    /// Idempotent: a broadcast already marked running keeps its slot instead of
    /// being counted twice, so a repeated click cannot consume two.
    pub fn claim_stream_slot(&self, user_id: &str, broadcast_id: &str) -> Result<()> {
        // No subscription, no slot. Asked first so the answer is "요금제가
        // 필요합니다" rather than a concurrency ceiling of zero, which reads like
        // a bug to whoever hits it.
        let sub = self.require_active_subscription(user_id)?;
        let allowed = sub.plan.as_ref().map(|p| p.max_concurrent_streams()).unwrap_or(0);
        let plan_label = sub.plan_label.clone();
        let conn = self.raw();
        let mut guard = conn.lock().unwrap();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let owner: Option<String> =
            tx.query_row("SELECT user_id FROM broadcasts WHERE id=?1", [broadcast_id], |r| r.get(0)).ok();
        match owner.as_deref() {
            None => return Err(CloudError::NotFound("broadcast")),
            Some(o) if o != user_id => return Err(CloudError::Forbidden),
            _ => {}
        }

        let already: String =
            tx.query_row("SELECT desired_state FROM broadcasts WHERE id=?1", [broadcast_id], |r| r.get(0))?;
        if already == DesiredState::Running.id() {
            return Ok(()); // already holds the slot
        }

        let used: i64 = tx.query_row(
            "SELECT COUNT(*) FROM broadcasts WHERE user_id=?1 AND desired_state='running'",
            [user_id],
            |r| r.get(0),
        )?;
        if used >= allowed {
            return Err(CloudError::ConcurrencyReached { plan_label, used, allowed });
        }

        tx.execute(
            "UPDATE broadcasts
             SET desired_state='running', runtime_state=?2, started_at=datetime('now'),
                 stopped_at=NULL, last_error=NULL
             WHERE id=?1",
            params![broadcast_id, RuntimeState::Preparing.id()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Give the slot back. Marks the stop as deliberate so no watchdog resumes it.
    pub fn release_stream_slot(&self, user_id: &str, broadcast_id: &str) -> Result<()> {
        let conn = self.raw();
        let guard = conn.lock().unwrap();
        let n = guard.execute(
            "UPDATE broadcasts
             SET desired_state='stopped', runtime_state=?3, stopped_at=datetime('now')
             WHERE id=?1 AND user_id=?2",
            params![broadcast_id, user_id, RuntimeState::Stopping.id()],
        )?;
        if n == 0 {
            return Err(CloudError::NotFound("broadcast"));
        }
        Ok(())
    }

    /// Bytes this user's stored originals and prepared copies occupy.
    pub fn storage_used(&self, user_id: &str) -> Result<i64> {
        Ok(self.raw().lock().unwrap().query_row(
            "SELECT COALESCE(SUM(size_bytes),0) FROM media WHERE user_id=?1",
            [user_id],
            |r| r.get(0),
        )?)
    }

    /// The label of the plan this user's limits come from, for messages.
    pub fn plan_label_of(&self, user_id: &str) -> Result<String> {
        let u = self.user(user_id)?;
        Ok(self.plan(&u.plan_id)?.label)
    }

    /// Refuse an upload that breaks either of the plan's two storage limits.
    ///
    /// Both are **per account**: `used` is this user's own media and nobody
    /// else's. The physical disk is a third, separate check
    /// (`Ingest::check_disk_has_room`) that runs after this one, so a plan with
    /// room left can still be refused when the server has none.
    ///
    /// Both boundaries are inclusive: a file exactly at the per-file ceiling,
    /// or one that fills the account exactly, is allowed.
    pub fn check_upload_allowed(&self, user_id: &str, incoming_bytes: i64) -> Result<()> {
        let per_file = self.limit(user_id, MAX_UPLOAD_BYTES)?;
        if incoming_bytes > per_file {
            return Err(CloudError::LimitReached {
                limit: MAX_UPLOAD_BYTES,
                plan_label: self.plan_label_of(user_id)?,
                used: incoming_bytes,
                allowed: per_file,
            });
        }
        let ceiling = self.limit(user_id, MAX_STORAGE_BYTES)?;
        let used = self.storage_used(user_id)?;
        if used.saturating_add(incoming_bytes) > ceiling {
            return Err(CloudError::LimitReached {
                limit: MAX_STORAGE_BYTES,
                plan_label: self.plan_label_of(user_id)?,
                used,
                allowed: ceiling,
            });
        }
        Ok(())
    }

    pub fn check_can_create_broadcast(&self, user_id: &str) -> Result<()> {
        // §8: making a broadcast is where a user puts in the work — choosing
        // videos, setting a title, arranging a playlist. Letting them do all of
        // that and only refusing at START would waste it. `max_broadcasts` is 0
        // on the unsubscribed plan, so this would refuse anyway; the gate is here
        // so the refusal says why.
        self.require_active_subscription(user_id)?;
        let allowed = self.limit(user_id, MAX_BROADCASTS)?;
        let used: i64 = self.raw().lock().unwrap().query_row(
            "SELECT COUNT(*) FROM broadcasts WHERE user_id=?1",
            [user_id],
            |r| r.get(0),
        )?;
        if used >= allowed {
            return Err(CloudError::LimitReached {
                limit: MAX_BROADCASTS,
                plan_label: self.plan_label_of(user_id)?,
                used,
                allowed,
            });
        }
        Ok(())
    }
}
