//! Crash-safe session state (§32, §33).
//!
//! A small JSON file is rewritten as the broadcast progresses. If the machine
//! loses power, the next launch finds it and decides whether to resume.

use crate::config::StreamMode;
use crate::error::Result;
use crate::streaming::playlist::PlaybackMode;
use crate::streaming::state::StreamState;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The snapshot persisted to disk (§32).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionState {
    pub session_id: i64,
    pub playlist_id: i64,
    pub started_at: DateTime<Utc>,
    pub scheduled_end: Option<DateTime<Utc>>,
    pub playback_mode: PlaybackMode,
    pub stream_state: StreamState,
    pub stream_mode: StreamMode,
    /// The distinguishing fact for recovery: a deliberate stop is never resumed.
    pub user_requested_stop: bool,
    pub last_error: Option<String>,
    /// Pid of the FFmpeg child, so an orphan can be cleaned up (§33).
    pub ffmpeg_pid: Option<u32>,
    /// Seed that produced the play order, so a resumed session keeps it.
    pub order_seed: i64,
    /// Bumped on every write; a stale file is one that stopped being updated.
    pub heartbeat_at: DateTime<Utc>,
    /// Scheduler occurrence this session belongs to, if it was scheduled.
    pub schedule_id: Option<i64>,
}

impl SessionState {
    pub fn is_clean_shutdown(&self) -> bool {
        self.user_requested_stop || self.stream_state.is_terminal()
    }
}

/// Reads and writes the state file atomically.
#[derive(Debug, Clone)]
pub struct SessionStore {
    path: PathBuf,
}

impl SessionStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write via a temp file and rename, so a power cut mid-write cannot leave
    /// a half-written file where a valid one used to be.
    pub fn save(&self, s: &SessionState) -> Result<()> {
        if let Some(d) = self.path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(s)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn load(&self) -> Option<SessionState> {
        serde_json::from_str(&std::fs::read_to_string(&self.path).ok()?).ok()
    }

    pub fn clear(&self) -> Result<()> {
        if self.path.exists() {
            std::fs::remove_file(&self.path)?;
        }
        Ok(())
    }

    pub fn touch(&self, s: &mut SessionState, state: StreamState, pid: Option<u32>) -> Result<()> {
        s.stream_state = state;
        s.ffmpeg_pid = pid;
        s.heartbeat_at = Utc::now();
        self.save(s)
    }
}

/// What the app should do about a state file found at launch (§32).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryDecision {
    /// No previous session, or it ended cleanly.
    Nothing,
    /// The previous run died mid-broadcast, and we are inside its window:
    /// resume. Tell the user why.
    Resume { session: Box<SessionState>, reason: String },
    /// It died mid-broadcast but we are now outside the schedule: stay off.
    CleanUpOnly { session: Box<SessionState>, reason: String },
}

/// Decide what to do with a recovered state file.
///
/// `in_scheduled_window` is the scheduler's verdict for *now*. §32 is explicit:
/// resume only inside the schedule; outside it, do not broadcast.
pub fn decide_recovery(
    state: Option<SessionState>,
    now: DateTime<Utc>,
    in_scheduled_window: bool,
) -> RecoveryDecision {
    let Some(s) = state else { return RecoveryDecision::Nothing };

    if s.is_clean_shutdown() {
        return RecoveryDecision::Nothing;
    }
    if let Some(end) = s.scheduled_end {
        if now >= end {
            return RecoveryDecision::CleanUpOnly {
                session: Box::new(s),
                reason: "이전 방송의 예약 종료 시간이 이미 지났습니다.".into(),
            };
        }
    }
    if in_scheduled_window {
        RecoveryDecision::Resume {
            session: Box::new(s),
            reason: "이전 방송이 비정상 종료되었습니다. 예약 시간 내이므로 방송을 재개합니다.".into(),
        }
    } else {
        RecoveryDecision::CleanUpOnly {
            session: Box::new(s),
            reason: "이전 방송이 비정상 종료되었습니다. 예약 시간이 아니므로 방송을 시작하지 않습니다."
                .into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> SessionState {
        SessionState {
            session_id: 1,
            playlist_id: 7,
            started_at: "2026-03-02T20:00:00Z".parse().unwrap(),
            scheduled_end: Some("2026-03-03T08:00:00Z".parse().unwrap()),
            playback_mode: PlaybackMode::Sequential,
            stream_state: StreamState::Live,
            stream_mode: StreamMode::StreamCopy,
            user_requested_stop: false,
            last_error: None,
            ffmpeg_pid: Some(1234),
            order_seed: 99,
            heartbeat_at: "2026-03-02T22:00:00Z".parse().unwrap(),
            schedule_id: Some(3),
        }
    }

    fn t(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn state_round_trips_through_disk() {
        let d = tempfile::tempdir().unwrap();
        let st = SessionStore::new(d.path().join("sub").join("session.json"));
        assert!(st.load().is_none());

        st.save(&state()).unwrap();
        let got = st.load().unwrap();
        assert_eq!(got.session_id, 1);
        assert_eq!(got.stream_state, StreamState::Live);
        assert_eq!(got.order_seed, 99, "the play order must survive a restart");
        assert_eq!(got.ffmpeg_pid, Some(1234));

        st.clear().unwrap();
        assert!(st.load().is_none());
    }

    #[test]
    fn a_corrupt_state_file_reads_as_no_state() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("session.json");
        std::fs::write(&p, "{ truncated by a power cut").unwrap();
        assert!(SessionStore::new(&p).load().is_none(), "must not panic on a torn file");
    }

    #[test]
    fn touch_updates_state_and_heartbeat() {
        let d = tempfile::tempdir().unwrap();
        let st = SessionStore::new(d.path().join("s.json"));
        let mut s = state();
        let before = s.heartbeat_at;
        st.touch(&mut s, StreamState::Reconnecting, Some(555)).unwrap();
        let got = st.load().unwrap();
        assert_eq!(got.stream_state, StreamState::Reconnecting);
        assert_eq!(got.ffmpeg_pid, Some(555));
        assert!(got.heartbeat_at > before);
    }

    // --- recovery decisions (§32) -----------------------------------------

    #[test]
    fn no_state_file_means_nothing_to_do() {
        assert_eq!(decide_recovery(None, t("2026-03-02T21:00:00Z"), true), RecoveryDecision::Nothing);
    }

    #[test]
    fn a_clean_stop_is_never_resumed() {
        let mut s = state();
        s.user_requested_stop = true;
        s.stream_state = StreamState::Stopped;
        assert_eq!(
            decide_recovery(Some(s), t("2026-03-02T21:00:00Z"), true),
            RecoveryDecision::Nothing,
            "a broadcast the user stopped must stay stopped (§17)"
        );
    }

    #[test]
    fn a_crash_inside_the_window_resumes() {
        // The §64 scenario: the PC reboots at 22:00 during a 20:00→08:00 window.
        match decide_recovery(Some(state()), t("2026-03-02T22:05:00Z"), true) {
            RecoveryDecision::Resume { session, reason } => {
                assert_eq!(session.playlist_id, 7);
                assert_eq!(session.order_seed, 99);
                assert!(reason.contains("비정상 종료"));
            }
            d => panic!("expected Resume, got {d:?}"),
        }
    }

    #[test]
    fn a_crash_outside_the_window_cleans_up_without_broadcasting() {
        // Still before the session's own scheduled end, but the scheduler says
        // this is not a broadcast window (for example the schedule was edited).
        match decide_recovery(Some(state()), t("2026-03-03T07:00:00Z"), false) {
            RecoveryDecision::CleanUpOnly { reason, .. } => {
                assert!(reason.contains("예약 시간이 아니므로"), "{reason}");
            }
            d => panic!("expected CleanUpOnly, got {d:?}"),
        }
    }

    #[test]
    fn a_session_whose_scheduled_end_has_passed_is_not_resumed() {
        // Even if the scheduler says we are inside *some* window now, this
        // particular session's window is over.
        match decide_recovery(Some(state()), t("2026-03-03T09:00:00Z"), true) {
            RecoveryDecision::CleanUpOnly { reason, .. } => assert!(reason.contains("종료 시간")),
            d => panic!("expected CleanUpOnly, got {d:?}"),
        }
    }

    #[test]
    fn a_manual_session_with_no_scheduled_end_resumes_only_inside_a_window() {
        let mut s = state();
        s.scheduled_end = None;
        assert!(matches!(
            decide_recovery(Some(s.clone()), t("2026-03-02T22:00:00Z"), true),
            RecoveryDecision::Resume { .. }
        ));
        assert!(matches!(
            decide_recovery(Some(s), t("2026-03-02T22:00:00Z"), false),
            RecoveryDecision::CleanUpOnly { .. }
        ));
    }

    #[test]
    fn every_non_terminal_state_counts_as_an_unclean_shutdown() {
        for st in
            [StreamState::Preparing, StreamState::Connecting, StreamState::Live, StreamState::Reconnecting]
        {
            let mut s = state();
            s.stream_state = st;
            assert!(!s.is_clean_shutdown(), "{st} should look like a crash");
        }
        for st in [StreamState::Stopped, StreamState::Error, StreamState::Idle] {
            let mut s = state();
            s.stream_state = st;
            assert!(s.is_clean_shutdown(), "{st} should look clean");
        }
    }

    #[test]
    fn the_orphan_pid_is_available_for_cleanup() {
        // §33: the recovered session names the process that may still be alive.
        match decide_recovery(Some(state()), t("2026-03-02T22:00:00Z"), true) {
            RecoveryDecision::Resume { session, .. } => assert_eq!(session.ffmpeg_pid, Some(1234)),
            d => panic!("{d:?}"),
        }
    }
}
