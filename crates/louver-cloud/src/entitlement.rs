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

/// Overrides how many broadcasts this machine will encode at once.
pub const ENCODING_BUDGET_ENV: &str = "LOUVER_MAX_ENCODING_STREAMS";

/// How many broadcasts this machine will encode in real time at once.
///
/// # Why there is a second ceiling at all
///
/// A plan's `max_concurrent_streams` counts broadcasts, and until the live
/// video source arrived every broadcast cost the same: the media was
/// normalised at upload, so sending it is `-c copy` — a file read and a socket
/// write, no encoding. Three of those on two cores is nothing.
///
/// A broadcast with a live video source cannot work that way. Its picture and
/// its sound come from two unrelated inputs, which cannot be muxed by copying
/// packets, so FFmpeg encodes H.264 in real time for as long as the broadcast
/// runs. That is a different order of cost, and the plan limit cannot see it:
/// a Business plan allows three concurrent broadcasts whether they are three
/// stream copies or three 1080p encodes.
///
/// Measured on two cores with the product's own argv and a 1080p30 source: one
/// encode held `speed=1.01x`, and two together fell to `0.957x` and `0.938x`.
/// Below 1.0x the encoder never catches up, the muxer's queue grows, and
/// YouTube reports the stream as unstable — for *both* customers, because the
/// cost is shared. Refusing the second start is the kinder failure: one
/// broadcast that works beats two that stutter.
///
/// # The number
///
/// Half the usable cores, at least one. `available_parallelism` honours CPU
/// affinity and the container's cgroup quota, so this is the machine's real
/// allowance rather than the hypervisor's core count — on a 2-vCPU box it is
/// 1, on a 4-core box 2. Half and not all, because the API, the OS and
/// FFmpeg's own I/O threads need the rest.
///
/// It is deliberately conservative and deliberately overridable. The
/// measurement above used `testsrc2`, which is synthetic high-entropy video
/// and harder to encode than a traffic camera, so an operator who has measured
/// their own cameras should raise it. `0` switches encoding broadcasts off
/// altogether, which is the honest way to say "not on this box". An
/// unparseable or negative value falls back to the derived default rather than
/// failing a boot.
pub fn encoding_budget() -> i64 {
    encoding_budget_detail().value
}

/// The budget and where it came from.
///
/// The source is carried rather than re-derived by whoever logs it: a value
/// that was set but unusable falls back to the derived default, and a line
/// saying "set by environment variable" about that number would be a lie to
/// the one person trying to work out why a start was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodingBudget {
    pub value: i64,
    /// True only when the environment variable both existed and parsed.
    pub from_env: bool,
}

pub fn encoding_budget_detail() -> EncodingBudget {
    match std::env::var(ENCODING_BUDGET_ENV) {
        Ok(raw) => match raw.trim().parse::<i64>() {
            Ok(n) if n >= 0 => EncodingBudget { value: n, from_env: true },
            _ => EncodingBudget { value: derived_encoding_budget(), from_env: false },
        },
        Err(_) => EncodingBudget { value: derived_encoding_budget(), from_env: false },
    }
}

fn derived_encoding_budget() -> i64 {
    let cores = std::thread::available_parallelism().map(|n| n.get() as i64).unwrap_or(1);
    (cores / 2).max(1)
}

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
        self.claim_stream_slot_within(user_id, broadcast_id, encoding_budget())
    }

    /// [`Self::claim_stream_slot`] with the machine's encoder ceiling given
    /// rather than read from the environment.
    ///
    /// An argument and not an `std::env::var` at the point of use, for the same
    /// reason `Youtube::with_auto_transition` is a field: `set_var` is
    /// process-global and has already raced two of this repository's test
    /// threads once. A test that needs a budget of one says so.
    pub fn claim_stream_slot_within(
        &self,
        user_id: &str,
        broadcast_id: &str,
        encode_budget: i64,
    ) -> Result<()> {
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

        let (already, encodes): (String, bool) = tx.query_row(
            // `cctv_url` is what makes a broadcast an encode rather than a
            // copy: two unrelated inputs cannot be muxed by copying packets.
            "SELECT desired_state, COALESCE(TRIM(cctv_url), '') <> '' FROM broadcasts WHERE id=?1",
            [broadcast_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
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

        // The machine's ceiling, after the plan's. Counted across every
        // account, because the CPU is not divided between them, and inside
        // this same transaction so that two starts arriving together cannot
        // both be told there is room for the last one — the same hole the plan
        // check closes, one level up.
        //
        // Only encoding broadcasts are counted and only an encoding broadcast
        // is refused. A stream copy costs almost nothing and must not be held
        // up by a camera somebody else is sending.
        if encodes {
            let allowed_encodes = encode_budget;
            let running = tx.query_row(
                "SELECT COUNT(*) FROM broadcasts
                 WHERE desired_state='running' AND COALESCE(TRIM(cctv_url), '') <> ''",
                [],
                |r| r.get::<_, i64>(0),
            )?;
            if running >= allowed_encodes {
                return Err(CloudError::EncoderBusy { running, allowed: allowed_encodes });
            }
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
