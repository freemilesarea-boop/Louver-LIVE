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

use crate::error::{LiveSourceError, Result};

/// Cores to leave for everything that is not this worker.
pub const RESERVED_CORES: usize = 1;

/// Measured cost of one 1080p30 encode, rounded up.
pub const CORES_PER_WORKER: f32 = 1.5;

/// Measured resident size of one encode, rounded up.
pub const MB_PER_WORKER: u64 = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Hard ceiling on concurrent encodes, whatever the machine says.
    pub max_concurrent: usize,
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
        Self { max_concurrent: by_cpu.clamp(1, absolute_max.max(1)) }
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

    /// What the ceiling is expected to cost, for the operator's log.
    pub fn budget(&self) -> (f32, u64) {
        (self.max_concurrent as f32 * CORES_PER_WORKER, self.max_concurrent as u64 * MB_PER_WORKER)
    }
}

impl Default for Limits {
    /// Deliberately one. A default that is wrong is wrong in the direction of
    /// not starving anything.
    fn default() -> Self {
        Self { max_concurrent: 1 }
    }
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
    }

    #[test]
    fn admission_is_refused_at_the_ceiling_and_says_the_numbers() {
        let l = Limits { max_concurrent: 2 };
        assert!(l.admit(0).is_ok());
        assert!(l.admit(1).is_ok());
        let e = l.admit(2).unwrap_err();
        assert_eq!(e.kind, crate::error::ErrorKind::Limit);
        assert!(e.message.contains('2'), "{}", e.message);
        assert!(l.admit(99).is_err());
    }

    #[test]
    fn the_budget_is_reported_from_the_measured_cost() {
        let (cores, mb) = Limits { max_concurrent: 2 }.budget();
        assert_eq!(cores, 3.0);
        assert_eq!(mb, 800);
    }
}
