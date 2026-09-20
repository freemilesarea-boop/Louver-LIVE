//! The automatic live-chat bot.
//!
//! Two rules shape all of this. The bot must never interfere with the
//! broadcast — an API failure pauses the bot and nothing else — and it must
//! never post to a chat that belongs to a previous broadcast, which is what a
//! remembered `liveChatId` would do after a restart.
//!
//! The schedule itself is pure: [`ChatScheduler`] decides *what to send and
//! when* from a clock reading, and is tested without a network or a thread.

use super::metadata::Privacy;
use crate::error::{ErrorCode, LouverError};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Never allow a production interval shorter than this.
///
/// A mistyped interval posting every ten seconds would get the channel
/// rate-limited at best and treated as spam at worst, so the floor is enforced
/// in the engine rather than trusted to the UI.
pub const MIN_INTERVAL_SECS: u64 = 300;
/// The interval a new install starts with.
pub const DEFAULT_INTERVAL_SECS: u64 = 1200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatOrder {
    Sequential,
    Random,
}

/// What the UI shows about the bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ChatState {
    /// Switched off, or no broadcast running.
    Idle,
    /// The broadcast is up but YouTube has not published a chat id yet.
    WaitingForLiveChat,
    /// A chat id is in hand and the bot is waiting for the next send time.
    Connected,
    /// A message is in flight.
    Sending,
    /// Something recoverable happened; it will try again.
    Paused,
    /// Something the user has to fix.
    Error,
}

impl ChatState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "꺼짐",
            Self::WaitingForLiveChat => "채팅 연결 대기 중",
            Self::Connected => "연결됨",
            Self::Sending => "전송 중",
            Self::Paused => "일시 중지",
            Self::Error => "오류",
        }
    }
}

/// How the bot is configured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSettings {
    pub enabled: bool,
    pub order: ChatOrder,
    pub interval_secs: u64,
    /// Post as soon as the chat connects, rather than after one interval.
    pub send_on_start: bool,
    /// Post once more when the broadcast is stopping.
    pub send_on_end: bool,
    /// Never post the same text twice running.
    pub avoid_repeats: bool,
}

impl Default for ChatSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            order: ChatOrder::Sequential,
            interval_secs: DEFAULT_INTERVAL_SECS,
            send_on_start: true,
            send_on_end: false,
            avoid_repeats: true,
        }
    }
}

impl ChatSettings {
    /// The interval actually used, with the floor applied.
    pub fn effective_interval(&self) -> Duration {
        Duration::from_secs(self.interval_secs.max(MIN_INTERVAL_SECS))
    }

    pub fn validate(&self) -> crate::error::Result<()> {
        if self.interval_secs < MIN_INTERVAL_SECS {
            return Err(LouverError::with_detail(
                ErrorCode::ConfigInvalid,
                format!("전송 간격은 최소 {}분입니다", MIN_INTERVAL_SECS / 60),
            ));
        }
        Ok(())
    }
}

/// One message in the rotation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: i64,
    pub position: i64,
    pub text: String,
    pub enabled: bool,
}

/// Decides what to send and when. No I/O, no clock of its own.
#[derive(Debug)]
pub struct ChatScheduler {
    settings: ChatSettings,
    messages: Vec<ChatMessage>,
    /// Index into `messages` for sequential order.
    cursor: usize,
    /// Text of the last message sent, for the no-repeat rule.
    last_sent: Option<String>,
    /// Seconds-since-start of the last send; None until the first.
    last_sent_at: Option<Duration>,
    sent_count: u64,
}

impl ChatScheduler {
    pub fn new(settings: ChatSettings, messages: Vec<ChatMessage>) -> Self {
        Self { settings, messages, cursor: 0, last_sent: None, last_sent_at: None, sent_count: 0 }
    }

    pub fn settings(&self) -> &ChatSettings {
        &self.settings
    }

    pub fn sent_count(&self) -> u64 {
        self.sent_count
    }

    pub fn last_sent_at(&self) -> Option<Duration> {
        self.last_sent_at
    }

    /// Messages the user has switched on.
    fn active(&self) -> Vec<&ChatMessage> {
        self.messages.iter().filter(|m| m.enabled && !m.text.trim().is_empty()).collect()
    }

    pub fn has_messages(&self) -> bool {
        !self.active().is_empty()
    }

    /// When the next message is due, given how long the chat has been
    /// connected. `None` means nothing to send.
    pub fn next_due(&self) -> Option<Duration> {
        if !self.settings.enabled || !self.has_messages() {
            return None;
        }
        Some(match self.last_sent_at {
            None if self.settings.send_on_start => Duration::ZERO,
            None => self.settings.effective_interval(),
            Some(t) => t + self.settings.effective_interval(),
        })
    }

    /// Is a message due at `elapsed`?
    pub fn is_due(&self, elapsed: Duration) -> bool {
        self.next_due().is_some_and(|due| elapsed >= due)
    }

    /// Pick the next text. `rand` is a value in 0..active.len(), supplied by
    /// the caller so the choice is testable.
    pub fn pick(&mut self, rand: usize) -> Option<String> {
        // Owned copies: the cursor moves below, and holding a borrow of
        // `self.messages` across that is what the borrow checker objects to.
        let active: Vec<String> = self.active().into_iter().map(|m| m.text.clone()).collect();
        if active.is_empty() {
            return None;
        }
        let n = active.len();

        let mut chosen = match self.settings.order {
            ChatOrder::Sequential => {
                let idx = self.cursor % n;
                self.cursor = (self.cursor + 1) % n;
                active[idx].clone()
            }
            ChatOrder::Random => {
                let mut idx = rand % n;
                // With more than one message available, never repeat the last
                // one — otherwise "random" produces visible doubles.
                if self.settings.avoid_repeats && n > 1 {
                    if let Some(last) = &self.last_sent {
                        if &active[idx] == last {
                            idx = (idx + 1) % n;
                        }
                    }
                }
                active[idx].clone()
            }
        };

        // Sequential order can still land on a repeat when the list contains
        // the same text twice; step past it.
        if self.settings.avoid_repeats && n > 1 && Some(&chosen) == self.last_sent.as_ref() {
            let idx = self.cursor % n;
            self.cursor = (self.cursor + 1) % n;
            if Some(&active[idx]) != self.last_sent.as_ref() {
                chosen = active[idx].clone();
            }
        }
        Some(chosen)
    }

    /// Record a successful send at `elapsed`.
    pub fn mark_sent(&mut self, text: String, elapsed: Duration) {
        self.last_sent = Some(text);
        self.last_sent_at = Some(elapsed);
        self.sent_count += 1;
    }

    /// Start over for a new broadcast: nothing about the previous one carries
    /// across, including how long ago the last message went out.
    pub fn reset_for_new_broadcast(&mut self) {
        self.cursor = 0;
        self.last_sent = None;
        self.last_sent_at = None;
        self.sent_count = 0;
    }

    pub fn replace(&mut self, settings: ChatSettings, messages: Vec<ChatMessage>) {
        self.settings = settings;
        self.messages = messages;
        if self.cursor >= self.messages.len() {
            self.cursor = 0;
        }
    }
}

/// What the UI is shown about the bot right now.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatStatus {
    pub state: ChatState,
    pub state_label: String,
    /// Present once a broadcast's chat has been identified.
    pub live_chat_id_known: bool,
    pub messages_sent: u64,
    pub seconds_until_next: Option<u64>,
    /// Set when the bot is paused or in error.
    pub last_error: Option<String>,
    pub last_error_code: Option<String>,
    /// The broadcast the bot is attached to, so a stale one is visible.
    pub broadcast_id: Option<String>,
    pub broadcast_title: Option<String>,
    pub broadcast_privacy: Option<Privacy>,
}

impl Default for ChatStatus {
    fn default() -> Self {
        Self {
            state: ChatState::Idle,
            state_label: ChatState::Idle.label().to_string(),
            live_chat_id_known: false,
            messages_sent: 0,
            seconds_until_next: None,
            last_error: None,
            last_error_code: None,
            broadcast_id: None,
            broadcast_title: None,
            broadcast_privacy: None,
        }
    }
}

/// Whether a failure should stop the bot or let it try again.
///
/// Nothing here can stop the broadcast; the worst case is a bot that gives up
/// and says why.
pub fn is_fatal_for_bot(code: &str) -> bool {
    matches!(code, "LL-CHAT-002" | "LL-CHAT-003" | "LL-YOUTUBE-002" | "LL-YOUTUBE-005")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msgs(texts: &[&str]) -> Vec<ChatMessage> {
        texts
            .iter()
            .enumerate()
            .map(|(i, t)| ChatMessage {
                id: i as i64 + 1,
                position: i as i64,
                text: (*t).to_string(),
                enabled: true,
            })
            .collect()
    }

    fn settings() -> ChatSettings {
        ChatSettings { enabled: true, send_on_start: false, ..Default::default() }
    }

    #[test]
    fn an_interval_below_the_floor_is_raised_to_it() {
        let s = ChatSettings { interval_secs: 10, ..settings() };
        assert_eq!(s.effective_interval(), Duration::from_secs(MIN_INTERVAL_SECS));
        assert!(s.validate().is_err(), "the UI should be told, not silently corrected");
    }

    #[test]
    fn sequential_order_walks_the_list_and_wraps() {
        let mut s = ChatScheduler::new(settings(), msgs(&["a", "b", "c"]));
        let mut seen = Vec::new();
        for i in 0..5 {
            let t = s.pick(0).unwrap();
            s.mark_sent(t.clone(), Duration::from_secs(i * 600));
            seen.push(t);
        }
        assert_eq!(seen, ["a", "b", "c", "a", "b"]);
    }

    #[test]
    fn a_disabled_message_is_skipped() {
        let mut m = msgs(&["a", "b", "c"]);
        m[1].enabled = false;
        let mut s = ChatScheduler::new(settings(), m);
        assert_eq!(s.pick(0).unwrap(), "a");
        assert_eq!(s.pick(0).unwrap(), "c");
    }

    #[test]
    fn nothing_is_due_without_messages_or_when_switched_off() {
        let s = ChatScheduler::new(settings(), vec![]);
        assert!(s.next_due().is_none());

        let off = ChatSettings { enabled: false, ..settings() };
        let s = ChatScheduler::new(off, msgs(&["a"]));
        assert!(s.next_due().is_none());
        assert!(!s.is_due(Duration::from_secs(99999)));
    }

    #[test]
    fn the_first_message_waits_one_interval_unless_asked_to_go_immediately() {
        let s = ChatScheduler::new(settings(), msgs(&["a"]));
        assert_eq!(s.next_due(), Some(Duration::from_secs(DEFAULT_INTERVAL_SECS)));
        assert!(!s.is_due(Duration::from_secs(1)));

        let eager = ChatSettings { send_on_start: true, ..settings() };
        let s = ChatScheduler::new(eager, msgs(&["a"]));
        assert_eq!(s.next_due(), Some(Duration::ZERO));
        assert!(s.is_due(Duration::ZERO));
    }

    #[test]
    fn messages_are_spaced_by_the_interval() {
        let mut s = ChatScheduler::new(ChatSettings { interval_secs: 600, ..settings() }, msgs(&["a", "b"]));
        assert!(!s.is_due(Duration::from_secs(599)));
        assert!(s.is_due(Duration::from_secs(600)));

        let t = s.pick(0).unwrap();
        s.mark_sent(t, Duration::from_secs(600));
        assert!(!s.is_due(Duration::from_secs(1100)));
        assert!(s.is_due(Duration::from_secs(1200)));
    }

    #[test]
    fn random_order_does_not_repeat_the_previous_message() {
        let mut s = ChatScheduler::new(
            ChatSettings { order: ChatOrder::Random, ..settings() },
            msgs(&["a", "b", "c"]),
        );
        let first = s.pick(1).unwrap(); // "b"
        s.mark_sent(first.clone(), Duration::from_secs(0));
        // The same draw would give "b" again; the rule must move it on.
        let second = s.pick(1).unwrap();
        assert_ne!(second, first);
    }

    #[test]
    fn a_list_containing_the_same_text_twice_still_does_not_repeat_it() {
        let mut s = ChatScheduler::new(settings(), msgs(&["a", "a", "b"]));
        let first = s.pick(0).unwrap();
        s.mark_sent(first.clone(), Duration::from_secs(0));
        let second = s.pick(0).unwrap();
        assert_ne!(second, first, "the duplicate should have been stepped over");
    }

    #[test]
    fn repeats_are_allowed_when_there_is_only_one_message() {
        let mut s = ChatScheduler::new(settings(), msgs(&["only"]));
        let a = s.pick(0).unwrap();
        s.mark_sent(a.clone(), Duration::from_secs(0));
        assert_eq!(s.pick(0).unwrap(), a);
    }

    #[test]
    fn a_new_broadcast_starts_the_rotation_over() {
        let mut s = ChatScheduler::new(settings(), msgs(&["a", "b"]));
        let t = s.pick(0).unwrap();
        s.mark_sent(t, Duration::from_secs(600));
        assert_eq!(s.sent_count(), 1);

        s.reset_for_new_broadcast();
        assert_eq!(s.sent_count(), 0);
        assert!(s.last_sent_at().is_none());
        assert_eq!(s.pick(0).unwrap(), "a", "the second broadcast starts from the top");
    }

    #[test]
    fn only_the_failures_the_user_must_fix_stop_the_bot() {
        assert!(is_fatal_for_bot("LL-CHAT-002")); // chat disabled
        assert!(is_fatal_for_bot("LL-CHAT-003")); // chat ended
        assert!(is_fatal_for_bot("LL-YOUTUBE-002")); // auth expired
        assert!(is_fatal_for_bot("LL-YOUTUBE-005")); // quota
        assert!(!is_fatal_for_bot("LL-CHAT-001")); // rate limited: back off
        assert!(!is_fatal_for_bot("LL-YOUTUBE-004")); // transient
    }
}
