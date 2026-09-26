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
    pub fn limit(&self, user_id: &str, key: &str) -> Result<i64> {
        let u = self.user(user_id)?;
        Ok(self.plan(&u.plan_id)?.limits.get(key).copied().unwrap_or(0))
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
        let allowed = self.limit(user_id, MAX_CONCURRENT_STREAMS)?;
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
            return Err(CloudError::LimitReached { limit: MAX_CONCURRENT_STREAMS, used, allowed });
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

    /// Refuse an upload that would take the user over their storage ceiling.
    pub fn check_upload_allowed(&self, user_id: &str, incoming_bytes: i64) -> Result<()> {
        let per_file = self.limit(user_id, MAX_UPLOAD_BYTES)?;
        if incoming_bytes > per_file {
            return Err(CloudError::LimitReached {
                limit: MAX_UPLOAD_BYTES,
                used: incoming_bytes,
                allowed: per_file,
            });
        }
        let ceiling = self.limit(user_id, MAX_STORAGE_BYTES)?;
        let used = self.storage_used(user_id)?;
        if used + incoming_bytes > ceiling {
            return Err(CloudError::LimitReached { limit: MAX_STORAGE_BYTES, used, allowed: ceiling });
        }
        Ok(())
    }

    pub fn check_can_create_broadcast(&self, user_id: &str) -> Result<()> {
        let allowed = self.limit(user_id, MAX_BROADCASTS)?;
        let used: i64 = self.raw().lock().unwrap().query_row(
            "SELECT COUNT(*) FROM broadcasts WHERE user_id=?1",
            [user_id],
            |r| r.get(0),
        )?;
        if used >= allowed {
            return Err(CloudError::LimitReached { limit: MAX_BROADCASTS, used, allowed });
        }
        Ok(())
    }
}
