//! What this worker refuses to do, so that it cannot become the reason a
//! machine runs out of CPU.
//!
//! Measured on the dev box (4 cores), with the production argv:
//!
//! | path | CPU | RSS |
//! |---|---|---|
//! | existing playlist broadcast (stream copy) | 2–8% | 56 MB |
//! | this worker, 1080p30 (encode is forced) | 95–148% | 388 MB |
//!
//! A live video source cannot be stream-copied — its picture and the playlist's
//! sound are unrelated streams — so every one of these broadcasts is a real
//! x264 encode. One costs about a core. That is why the ceiling below is small
//! and why it is enforced before a child is spawned rather than measured after.
//!
//! ## Two ceilings, not one
//!
//! The machine ceiling protects the host. It does **not** protect the other
//! users on it: with only `max_concurrent` in place, one beta account could
//! take every slot and every other account's request would be refused — a
//! denial of service that needs no attacker, only an enthusiastic customer. So
//! there is a second, smaller ceiling per user, checked first, and a request
//! has to pass both.

use crate::error::{LiveSourceError, Result};

/// Cores to leave for everything that is not this worker.
pub const RESERVED_CORES: usize = 1;

/// Measured cost of one 1080p30 encode, rounded up.
pub const CORES_PER_WORKER: f32 = 1.5;

/// Measured resident size of one encode, rounded up.
pub const MB_PER_WORKER: u64 = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Hard ceiling on concurrent encodes across every user, whatever the
    /// machine says. This one is about the host.
    pub max_concurrent: usize,
    /// Ceiling on concurrent encodes for one user. This one is about the other
    /// users: it stops a single account from taking every slot.
    pub max_per_user: usize,
}

impl Limits {
    /// A ceiling derived from the machine, never above `absolute_max`.
    ///
    /// `cores - RESERVED_CORES` divided by the measured per-worker cost. On a
    /// 4-core box that is 2, which matches the measurement: two encodes is
    /// ~3 cores and leaves one.
    pub fn for_machine(cores: usize, absolute_max: usize) -> Self {
        let usable = cores.saturating_sub(RESERVED_CORES) as f32;
        let by_cpu = (usable / CORES_PER_WORKER).floor() as usize;
        let max_concurrent = by_cpu.clamp(1, absolute_max.max(1));
        // Half the machine, rounded up, and never the whole of it unless the
        // machine only has room for one anyway. On a ceiling of 2 that is 1, so
        // a second user always has somewhere to go.
        Self { max_concurrent, max_per_user: default_per_user(max_concurrent) }
    }

    /// Override the per-user ceiling, never above the machine ceiling.
    ///
    /// A per-user ceiling larger than the machine's would be a number that
    /// never fires, which is worse than no number at all because it reads as
    /// protection.
    pub fn with_per_user(mut self, max_per_user: usize) -> Self {
        self.max_per_user = max_per_user.clamp(1, self.max_concurrent.max(1));
        self
    }

    /// Refuse a new worker when the ceiling is already reached.
    pub fn admit(&self, running: usize) -> Result<()> {
        if running >= self.max_concurrent {
            return Err(LiveSourceError::limit(format!(
                "동시에 보낼 수 있는 YouTube Live 방송은 {}개입니다. 현재 {}개가 실행 중입니다.",
                self.max_concurrent, running
            )));
        }
        Ok(())
    }

    /// Refuse a new worker when this user already has their share.
    ///
    /// Checked **before** [`Self::admit`], so a user at their own ceiling is
    /// told it is their own rather than the machine's: the two have different
    /// remedies — wait for your own broadcast to end, versus wait for someone
    /// else's — and a message that confuses them sends the user to support.
    pub fn admit_user(&self, running_for_user: usize) -> Result<()> {
        if running_for_user >= self.max_per_user {
            return Err(LiveSourceError::limit(format!(
                "한 계정이 동시에 보낼 수 있는 YouTube Live 방송은 {}개입니다. 현재 {}개가 실행 중입니다.",
                self.max_per_user, running_for_user
            )));
        }
        Ok(())
    }

    /// What the ceiling is expected to cost, for the operator's log.
    pub fn budget(&self) -> (f32, u64) {
        (self.max_concurrent as f32 * CORES_PER_WORKER, self.max_concurrent as u64 * MB_PER_WORKER)
    }
}

impl Default for Limits {
    /// Deliberately one. A default that is wrong is wrong in the direction of
    /// not starving anything.
    fn default() -> Self {
        Self { max_concurrent: 1, max_per_user: 1 }
    }
}

/// Half the machine ceiling, rounded up, so one user cannot hold all of it
/// unless there is only room for one encode in the first place.
fn default_per_user(max_concurrent: usize) -> usize {
    max_concurrent.div_ceil(2).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_four_core_box_takes_two_encodes() {
        // (4 - 1) / 1.5 = 2, which is what the measurement supports.
        assert_eq!(Limits::for_machine(4, 8).max_concurrent, 2);
    }

    #[test]
    fn a_small_box_still_takes_one_rather_than_zero() {
        // Refusing everything on a 1- or 2-core box would make the feature
        // untestable there; one encode is the honest minimum.
        for cores in [1, 2] {
            assert_eq!(Limits::for_machine(cores, 8).max_concurrent, 1, "{cores} cores");
        }
    }

    #[test]
    fn the_absolute_ceiling_wins_over_a_big_machine() {
        assert_eq!(Limits::for_machine(64, 4).max_concurrent, 4);
        assert_eq!(Limits::for_machine(64, 1).max_concurrent, 1);
    }

    #[test]
    fn the_default_is_one() {
        assert_eq!(Limits::default().max_concurrent, 1);
        assert_eq!(Limits::default().max_per_user, 1);
    }

    #[test]
    fn one_user_cannot_hold_the_whole_machine() {
        // The ceiling that protects the host is not the ceiling that protects
        // the other customers on it.
        assert_eq!(Limits::for_machine(64, 4).max_per_user, 2);
        assert_eq!(Limits::for_machine(64, 6).max_per_user, 3);
        // Except where there is only room for one encode at all: refusing the
        // only slot to everybody would make the feature unusable.
        assert_eq!(Limits::for_machine(2, 8).max_per_user, 1);
    }

    #[test]
    fn a_per_user_ceiling_above_the_machine_is_clamped_down() {
        // A number that can never fire reads as protection and is not.
        let l = Limits::for_machine(64, 4).with_per_user(99);
        assert_eq!(l.max_per_user, 4);
        assert_eq!(Limits::for_machine(64, 4).with_per_user(0).max_per_user, 1);
        assert_eq!(Limits::for_machine(64, 4).with_per_user(1).max_per_user, 1);
    }

    #[test]
    fn a_user_at_their_own_ceiling_is_told_it_is_theirs() {
        let l = Limits { max_concurrent: 4, max_per_user: 2 };
        assert!(l.admit_user(0).is_ok());
        assert!(l.admit_user(1).is_ok());
        let e = l.admit_user(2).unwrap_err();
        assert_eq!(e.kind, crate::error::ErrorKind::Limit);
        // The two refusals have different remedies, so they say different
        // things: wait for your own broadcast, or wait for someone else's.
        assert!(e.message.contains("한 계정"), "{}", e.message);
        // The machine ceiling is four, so that is where its own refusal starts
        // — and it does not claim to be about one account.
        assert!(l.admit(3).is_ok());
        assert!(!l.admit(4).unwrap_err().message.contains("한 계정"));
    }

    #[test]
    fn admission_is_refused_at_the_ceiling_and_says_the_numbers() {
        let l = Limits { max_concurrent: 2, max_per_user: 2 };
        assert!(l.admit(0).is_ok());
        assert!(l.admit(1).is_ok());
        let e = l.admit(2).unwrap_err();
        assert_eq!(e.kind, crate::error::ErrorKind::Limit);
        assert!(e.message.contains('2'), "{}", e.message);
        assert!(l.admit(99).is_err());
    }

    #[test]
    fn the_budget_is_reported_from_the_measured_cost() {
        let (cores, mb) = Limits { max_concurrent: 2, max_per_user: 1 }.budget();
        assert_eq!(cores, 3.0);
        assert_eq!(mb, 800);
    }
}
