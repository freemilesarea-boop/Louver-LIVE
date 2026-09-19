//! Broadcast state machine (§16).
//!
//! Transitions are validated rather than implied by string assignment, so an
//! illegal move (for example RECONNECTING after an explicit stop) is a typed
//! error instead of a silently wrong UI.

use crate::error::{ErrorCode, LouverError, Result};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "UPPERCASE")]
pub enum StreamState {
    #[default]
    Idle,
    Preparing,
    Connecting,
    Live,
    Reconnecting,
    Stopping,
    Stopped,
    Error,
}

impl StreamState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "IDLE",
            Self::Preparing => "PREPARING",
            Self::Connecting => "CONNECTING",
            Self::Live => "LIVE",
            Self::Reconnecting => "RECONNECTING",
            Self::Stopping => "STOPPING",
            Self::Stopped => "STOPPED",
            Self::Error => "ERROR",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        Some(match s {
            "IDLE" => Self::Idle,
            "PREPARING" => Self::Preparing,
            "CONNECTING" => Self::Connecting,
            "LIVE" => Self::Live,
            "RECONNECTING" => Self::Reconnecting,
            "STOPPING" => Self::Stopping,
            "STOPPED" => Self::Stopped,
            "ERROR" => Self::Error,
            _ => return None,
        })
    }

    /// True while an FFmpeg process should exist or is being (re)created.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Preparing | Self::Connecting | Self::Live | Self::Reconnecting)
    }

    /// True when the session has finished for good.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Idle | Self::Stopped | Self::Error)
    }

    fn allowed_next(self) -> &'static [StreamState] {
        use StreamState::*;
        match self {
            Idle => &[Preparing],
            Preparing => &[Connecting, Stopping, Error],
            Connecting => &[Live, Reconnecting, Stopping, Error],
            Live => &[Reconnecting, Stopping, Error],
            Reconnecting => &[Connecting, Live, Stopping, Error],
            Stopping => &[Stopped, Error],
            Stopped => &[Preparing, Idle],
            Error => &[Preparing, Idle],
        }
    }

    pub fn can_transition_to(self, next: StreamState) -> bool {
        self == next || self.allowed_next().contains(&next)
    }
}

impl fmt::Display for StreamState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Owns the current state and rejects illegal transitions.
#[derive(Debug, Clone)]
pub struct StateMachine {
    state: StreamState,
    history: Vec<StreamState>,
    /// Set the moment the user asks to stop; suppresses every auto-restart (§17).
    user_requested_stop: bool,
}

impl Default for StateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl StateMachine {
    pub fn new() -> Self {
        Self { state: StreamState::Idle, history: vec![StreamState::Idle], user_requested_stop: false }
    }

    pub fn state(&self) -> StreamState {
        self.state
    }

    pub fn history(&self) -> &[StreamState] {
        &self.history
    }

    pub fn user_requested_stop(&self) -> bool {
        self.user_requested_stop
    }

    /// Mark the stop as deliberate. Must be called before moving to `Stopping`
    /// for the supervisor to skip reconnection.
    pub fn request_user_stop(&mut self) {
        self.user_requested_stop = true;
    }

    pub fn transition(&mut self, next: StreamState) -> Result<StreamState> {
        if !self.state.can_transition_to(next) {
            return Err(LouverError::with_detail(
                ErrorCode::StreamInvalidTransition,
                format!("{} -> {}", self.state, next),
            ));
        }
        if self.state != next {
            self.state = next;
            self.history.push(next);
        }
        Ok(next)
    }

    /// Begin a fresh session, clearing the stop latch.
    pub fn begin(&mut self) -> Result<()> {
        if self.state.is_active() {
            return Err(LouverError::new(ErrorCode::StreamAlreadyRunning));
        }
        self.user_requested_stop = false;
        self.transition(StreamState::Preparing)?;
        Ok(())
    }

    /// FFmpeg exited. Returns the state to move to: `Reconnecting` for an
    /// unexpected exit, `Stopped` when the user asked for it.
    pub fn on_process_exit(&mut self, exit_ok: bool) -> Result<StreamState> {
        if self.user_requested_stop {
            // A stop that already completed stays completed; FFmpeg exit events
            // can arrive after `stop()` has finished tearing the session down.
            if self.state == StreamState::Stopped {
                return Ok(StreamState::Stopped);
            }
            if self.state != StreamState::Stopping {
                self.transition(StreamState::Stopping)?;
            }
            return self.transition(StreamState::Stopped);
        }
        if exit_ok && self.state == StreamState::Stopping {
            return self.transition(StreamState::Stopped);
        }
        self.transition(StreamState::Reconnecting)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_start_path() {
        let mut m = StateMachine::new();
        m.begin().unwrap();
        assert_eq!(m.state(), StreamState::Preparing);
        m.transition(StreamState::Connecting).unwrap();
        m.transition(StreamState::Live).unwrap();
        assert_eq!(m.state(), StreamState::Live);
        assert_eq!(
            m.history(),
            &[StreamState::Idle, StreamState::Preparing, StreamState::Connecting, StreamState::Live]
        );
    }

    #[test]
    fn normal_stop_path() {
        let mut m = StateMachine::new();
        m.begin().unwrap();
        m.transition(StreamState::Connecting).unwrap();
        m.transition(StreamState::Live).unwrap();
        m.request_user_stop();
        m.transition(StreamState::Stopping).unwrap();
        m.transition(StreamState::Stopped).unwrap();
        assert!(m.state().is_terminal());
    }

    #[test]
    fn unexpected_exit_moves_to_reconnecting() {
        let mut m = StateMachine::new();
        m.begin().unwrap();
        m.transition(StreamState::Connecting).unwrap();
        m.transition(StreamState::Live).unwrap();
        assert_eq!(m.on_process_exit(false).unwrap(), StreamState::Reconnecting);
    }

    #[test]
    fn reconnect_returns_to_live() {
        let mut m = StateMachine::new();
        m.begin().unwrap();
        m.transition(StreamState::Connecting).unwrap();
        m.transition(StreamState::Live).unwrap();
        m.on_process_exit(false).unwrap();
        m.transition(StreamState::Connecting).unwrap();
        m.transition(StreamState::Live).unwrap();
        assert_eq!(m.state(), StreamState::Live);
    }

    #[test]
    fn a_late_exit_event_after_a_completed_stop_is_not_an_error() {
        let mut m = StateMachine::new();
        m.begin().unwrap();
        m.transition(StreamState::Connecting).unwrap();
        m.transition(StreamState::Live).unwrap();
        m.request_user_stop();
        m.transition(StreamState::Stopping).unwrap();
        m.transition(StreamState::Stopped).unwrap();
        assert_eq!(m.on_process_exit(false).unwrap(), StreamState::Stopped);
        assert_eq!(m.state(), StreamState::Stopped);
    }

    #[test]
    fn explicit_stop_never_reconnects_even_on_a_bad_exit_code() {
        let mut m = StateMachine::new();
        m.begin().unwrap();
        m.transition(StreamState::Connecting).unwrap();
        m.transition(StreamState::Live).unwrap();
        m.request_user_stop();
        // FFmpeg is killed, so it exits non-zero — that must not trigger a restart.
        assert_eq!(m.on_process_exit(false).unwrap(), StreamState::Stopped);
        assert!(m.user_requested_stop());
    }

    #[test]
    fn illegal_transitions_are_rejected_with_a_code() {
        let mut m = StateMachine::new();
        let e = m.transition(StreamState::Live).unwrap_err();
        assert_eq!(e.code, ErrorCode::StreamInvalidTransition);
        assert!(e.detail.unwrap().contains("IDLE -> LIVE"));
        assert_eq!(m.state(), StreamState::Idle, "failed transition must not mutate state");
    }

    #[test]
    fn cannot_begin_while_already_active() {
        let mut m = StateMachine::new();
        m.begin().unwrap();
        m.transition(StreamState::Live).ok();
        assert_eq!(m.begin().unwrap_err().code, ErrorCode::StreamAlreadyRunning);
    }

    #[test]
    fn restarting_after_stop_clears_the_stop_latch() {
        let mut m = StateMachine::new();
        m.begin().unwrap();
        m.request_user_stop();
        m.transition(StreamState::Stopping).unwrap();
        m.transition(StreamState::Stopped).unwrap();
        m.begin().unwrap();
        assert!(!m.user_requested_stop(), "a new session must be restartable");
        assert_eq!(m.state(), StreamState::Preparing);
    }

    #[test]
    fn error_state_is_recoverable() {
        let mut m = StateMachine::new();
        m.begin().unwrap();
        m.transition(StreamState::Error).unwrap();
        assert!(m.state().is_terminal());
        m.begin().unwrap();
        assert_eq!(m.state(), StreamState::Preparing);
    }

    #[test]
    fn state_strings_round_trip() {
        for s in [
            StreamState::Idle, StreamState::Preparing, StreamState::Connecting,
            StreamState::Live, StreamState::Reconnecting, StreamState::Stopping,
            StreamState::Stopped, StreamState::Error,
        ] {
            assert_eq!(StreamState::from_str_opt(s.as_str()), Some(s));
        }
        assert_eq!(StreamState::from_str_opt("BOGUS"), None);
    }

    #[test]
    fn active_and_terminal_partition_the_states() {
        use StreamState::*;
        for s in [Idle, Preparing, Connecting, Live, Reconnecting, Stopping, Stopped, Error] {
            assert!(!(s.is_active() && s.is_terminal()), "{s} cannot be both");
        }
        assert!(Live.is_active());
        assert!(Stopped.is_terminal());
        // Stopping is neither: the process is winding down.
        assert!(!Stopping.is_active() && !Stopping.is_terminal());
    }
}
