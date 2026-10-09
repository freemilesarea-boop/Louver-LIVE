//! Noticing that the picture has stopped while the sound carries on.
//!
//! ## The fault this exists for, as measured
//!
//! When a live video source goes away, FFmpeg does **not** exit. Measured on
//! the dev box against a real HLS source, in both of the two ways a source can
//! fail:
//!
//! | failure | picture stopped | sound continued | process |
//! |---|---|---|---|
//! | source stopped producing segments (URL still answers 200 — an ended broadcast) | 17.4s | 60.0s | alive at +60s |
//! | endpoint died entirely (TCP refused) | 13.4s | 48.4s | alive at +45s |
//!
//! The playlist input is looped (`-stream_loop -1`), so the audio never ends,
//! so the process never ends, so the supervisor's restart never fires. A viewer
//! sees a frozen frame with music over it, indefinitely. `-rw_timeout` does not
//! help: it governs socket reads, and a demuxer that keeps failing to reload a
//! playlist is not blocked on one. `-reconnect` does not help either: it does
//! not apply to HLS playlist reloads.
//!
//! ## The signal
//!
//! From the same measurement, during the stall:
//!
//! ```text
//! frame=401  out_time=00:08:53.354667  dup_frames=0  drop_frames=0  speed=1x
//! frame=401  out_time=00:09:46.602667  dup_frames=0  drop_frames=0  speed=1x
//! ```
//!
//! `frame` is **frozen** and `out_time` **advances**. That pair is the
//! discriminator, and it is better than `frame` alone: if both freeze, the
//! process itself is wedged (or paused), which is a different fault needing a
//! different message. `dup_frames` stays at 0, which is worth knowing — it
//! means FFmpeg is not duplicating the last frame to hold the output frame
//! rate, so a frame counter really does stop.
//!
//! Nothing here reads a pid, scans a process table or sends a signal. The
//! watchdog returns a verdict; the worker that owns the child acts on it.

use std::time::{Duration, Instant};

/// How long the picture may be frozen before this is a fault.
///
/// Long enough to ride out a slow segment fetch — a 6-second HLS target
/// duration plus a retry is common — and short enough that a viewer sees a
/// reconnect rather than a frozen minute.
pub const DEFAULT_STALL_AFTER: Duration = Duration::from_secs(12);

/// How long after start a frozen picture is not yet a fault.
///
/// FFmpeg emits progress before the first frame leaves the encoder, so without
/// this every start would look like a stall for its first second.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Still starting up, or the picture is moving.
    Healthy,
    /// The picture is frozen and the output clock is still running.
    VideoStalled { frames: u64, frozen_secs: u64 },
    /// Neither the picture nor the clock is moving: FFmpeg itself is stuck.
    ProcessStalled { frozen_secs: u64 },
}

impl Verdict {
    pub fn is_fault(self) -> bool {
        !matches!(self, Verdict::Healthy)
    }
    /// A short token for the log line and the state file.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::VideoStalled { .. } => "video_stalled",
            Self::ProcessStalled { .. } => "process_stalled",
        }
    }
}

/// Reads FFmpeg's `-progress` stream and says whether the picture is moving.
#[derive(Debug)]
pub struct FrameWatchdog {
    stall_after: Duration,
    grace: Duration,
    started: Instant,
    frame: u64,
    out_time_ms: u64,
    /// When `frame` last increased (or start, if it never has).
    frame_moved: Instant,
    /// When `out_time_ms` last increased.
    clock_moved: Instant,
}

impl FrameWatchdog {
    pub fn new(now: Instant, stall_after: Duration, grace: Duration) -> Self {
        Self {
            stall_after,
            grace,
            started: now,
            frame: 0,
            out_time_ms: 0,
            frame_moved: now,
            clock_moved: now,
        }
    }

    pub fn with_defaults(now: Instant) -> Self {
        Self::new(now, DEFAULT_STALL_AFTER, DEFAULT_GRACE)
    }

    pub fn frame(&self) -> u64 {
        self.frame
    }

    /// Feed one line of `-progress` output.
    ///
    /// Anything that is not `frame=` or `out_time_ms=` is ignored, so the rest
    /// of the progress block (and any log line that lands on the same pipe)
    /// costs nothing. Returns true when the line moved something.
    pub fn observe(&mut self, line: &str, now: Instant) -> bool {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("frame=") {
            if let Ok(n) = v.trim().parse::<u64>() {
                // `>` and not `!=`: FFmpeg restarts the counter at 0 on a new
                // output, and a reset must not read as progress.
                if n > self.frame {
                    self.frame = n;
                    self.frame_moved = now;
                    return true;
                }
                return false;
            }
        }
        if let Some(v) = line.strip_prefix("out_time_ms=") {
            if let Ok(n) = v.trim().parse::<u64>() {
                if n > self.out_time_ms {
                    self.out_time_ms = n;
                    self.clock_moved = now;
                    return true;
                }
            }
        }
        false
    }

    /// Call this on a timer, not per line: the verdict is about elapsed time.
    pub fn verdict(&self, now: Instant) -> Verdict {
        if now.duration_since(self.started) < self.grace {
            return Verdict::Healthy;
        }
        let frame_frozen = now.duration_since(self.frame_moved);
        if frame_frozen < self.stall_after {
            return Verdict::Healthy;
        }
        let clock_frozen = now.duration_since(self.clock_moved);
        if clock_frozen >= self.stall_after {
            // Nothing is moving at all.
            return Verdict::ProcessStalled { frozen_secs: clock_frozen.as_secs() };
        }
        Verdict::VideoStalled { frames: self.frame, frozen_secs: frame_frozen.as_secs() }
    }

    /// After a restart, the next process starts its counters again.
    pub fn reset(&mut self, now: Instant) {
        *self = Self::new(now, self.stall_after, self.grace);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    /// The exact progress block FFmpeg writes, so the parser is tested against
    /// the real shape and not a convenient one.
    fn block(frame: u64, out_ms: u64) -> Vec<String> {
        vec![
            format!("frame={frame}"),
            "fps=30.00".into(),
            "stream_0_0_q=28.0".into(),
            "bitrate=6000.0kbits/s".into(),
            "total_size=1234567".into(),
            format!("out_time_ms={out_ms}"),
            format!("out_time={}", out_ms / 1_000_000),
            "dup_frames=0".into(),
            "drop_frames=0".into(),
            "speed=   1x".into(),
            "progress=continue".into(),
        ]
    }

    fn feed(w: &mut FrameWatchdog, frame: u64, out_ms: u64, now: Instant) {
        for line in block(frame, out_ms) {
            w.observe(&line, now);
        }
    }

    #[test]
    fn a_moving_picture_is_healthy() {
        let t0 = Instant::now();
        let mut w = FrameWatchdog::with_defaults(t0);
        for s in 1..=120 {
            feed(&mut w, s * 30, s * 1_000_000, at(t0, s));
            assert_eq!(w.verdict(at(t0, s)), Verdict::Healthy, "t+{s}s");
        }
        assert_eq!(w.frame(), 3600);
    }

    #[test]
    fn the_startup_grace_keeps_a_slow_first_frame_from_reading_as_a_stall() {
        let t0 = Instant::now();
        let w = FrameWatchdog::with_defaults(t0);
        // Nothing fed at all, which is what the first moments look like.
        assert_eq!(w.verdict(at(t0, 19)), Verdict::Healthy);
        assert!(w.verdict(at(t0, 40)).is_fault(), "after the grace it is a fault");
    }

    #[test]
    fn a_frozen_picture_with_a_running_clock_is_the_measured_fault() {
        // The real case: source stops, frame sticks at 401, out_time keeps going.
        let t0 = Instant::now();
        let mut w = FrameWatchdog::with_defaults(t0);
        feed(&mut w, 401, 30_000_000, at(t0, 30));
        // The clock keeps advancing; the frame count does not.
        for s in 31..=41 {
            feed(&mut w, 401, 30_000_000 + (s - 30) * 1_000_000, at(t0, s));
            assert_eq!(w.verdict(at(t0, s)), Verdict::Healthy, "t+{s}s is still inside stall_after");
        }
        feed(&mut w, 401, 42_000_000, at(t0, 42));
        match w.verdict(at(t0, 42)) {
            Verdict::VideoStalled { frames, frozen_secs } => {
                assert_eq!(frames, 401);
                assert_eq!(frozen_secs, 12);
            }
            other => panic!("expected VideoStalled, got {other:?}"),
        }
    }

    #[test]
    fn a_frozen_clock_as_well_is_reported_as_a_different_fault() {
        // Both stopped: FFmpeg is wedged, not the source. The operator needs to
        // be able to tell these apart, so they are separate verdicts.
        let t0 = Instant::now();
        let mut w = FrameWatchdog::with_defaults(t0);
        feed(&mut w, 900, 30_000_000, at(t0, 30));
        match w.verdict(at(t0, 45)) {
            Verdict::ProcessStalled { frozen_secs } => assert_eq!(frozen_secs, 15),
            other => panic!("expected ProcessStalled, got {other:?}"),
        }
    }

    #[test]
    fn a_counter_that_goes_backwards_is_not_progress() {
        // A new output restarts `frame` at 0. Treating that as movement would
        // hide a stall for another full stall_after.
        let t0 = Instant::now();
        let mut w = FrameWatchdog::with_defaults(t0);
        feed(&mut w, 500, 20_000_000, at(t0, 25));
        assert!(!w.observe("frame=1", at(t0, 26)), "a lower frame count is not progress");
        assert_eq!(w.frame(), 500);
        // And the freeze clock still runs from t+25.
        assert!(w.verdict(at(t0, 38)).is_fault());
    }

    #[test]
    fn rubbish_on_the_pipe_is_ignored_rather_than_parsed() {
        let t0 = Instant::now();
        let mut w = FrameWatchdog::with_defaults(t0);
        for line in [
            "",
            "   ",
            "frame=",
            "frame=abc",
            "frame=-1",
            "out_time_ms=notanumber",
            "[hls @ 0x55] Failed to reload playlist 0",
            "progress=continue",
        ] {
            assert!(!w.observe(line, at(t0, 1)), "{line:?} must not count as progress");
        }
        assert_eq!(w.frame(), 0);
    }

    #[test]
    fn a_reset_starts_the_next_process_clean() {
        let t0 = Instant::now();
        let mut w = FrameWatchdog::with_defaults(t0);
        feed(&mut w, 401, 30_000_000, at(t0, 30));
        assert!(w.verdict(at(t0, 60)).is_fault());
        w.reset(at(t0, 60));
        assert_eq!(w.frame(), 0);
        assert_eq!(w.verdict(at(t0, 61)), Verdict::Healthy, "the grace applies again");
    }

    #[test]
    fn the_verdict_tokens_are_stable_for_logs() {
        assert_eq!(Verdict::Healthy.as_str(), "healthy");
        assert_eq!(Verdict::VideoStalled { frames: 1, frozen_secs: 2 }.as_str(), "video_stalled");
        assert_eq!(Verdict::ProcessStalled { frozen_secs: 2 }.as_str(), "process_stalled");
        assert!(!Verdict::Healthy.is_fault());
    }
}
