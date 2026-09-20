//! The chat bot's thread.
//!
//! Deliberately its own thread with its own clock. The broadcast runtime ticks
//! once a second and must never wait on a network call to Google — so nothing
//! here is ever called from that tick. The two communicate through a mutex
//! holding a small status struct, and the broadcast could not stop this thread
//! from running even if Google stopped answering entirely.

use super::api::{HttpClient, LiveBroadcast, YoutubeApi};
use super::chat::{is_fatal_for_bot, ChatMessage, ChatScheduler, ChatSettings, ChatState, ChatStatus};
use super::oauth::{ClientCredentials, TokenEndpoint, TokenStore};
use crate::error::LouverError;
use crate::logging::{LogTarget, Logger};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How often the thread wakes to check whether anything is due.
const TICK: Duration = Duration::from_secs(2);
/// How long to wait before looking again for a chat id that has not appeared.
const CHAT_POLL: Duration = Duration::from_secs(15);
/// Backoff after a recoverable failure.
const RETRY_BACKOFF: Duration = Duration::from_secs(60);

/// Everything the bot needs from the rest of the app.
pub struct BotContext {
    pub http: Arc<dyn HttpClient>,
    pub tokens: Arc<TokenStore>,
    pub token_endpoint: Arc<dyn TokenEndpoint>,
    pub credentials: ClientCredentials,
    pub api_base: String,
    pub logger: Arc<Logger>,
}

/// A running bot. Dropping it stops the thread.
pub struct ChatBot {
    status: Arc<Mutex<ChatStatus>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ChatBot {
    /// Start the bot for one broadcast.
    ///
    /// `broadcast_id` pins it: the chat id is resolved from this broadcast and
    /// from no other. A bot started for a previous broadcast is stopped, not
    /// reused, which is what keeps a stale `liveChatId` out of the picture.
    pub fn start(
        ctx: Arc<BotContext>,
        broadcast_id: String,
        settings: ChatSettings,
        messages: Vec<ChatMessage>,
    ) -> Self {
        let status = Arc::new(Mutex::new(ChatStatus {
            state: ChatState::WaitingForLiveChat,
            state_label: ChatState::WaitingForLiveChat.label().to_string(),
            broadcast_id: Some(broadcast_id.clone()),
            ..Default::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));

        let handle = {
            let status = Arc::clone(&status);
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("louver-chat-bot".into())
                .spawn(move || run(ctx, broadcast_id, settings, messages, status, stop))
                .ok()
        };

        Self { status, stop, handle }
    }

    pub fn status(&self) -> ChatStatus {
        self.status.lock().unwrap().clone()
    }

    /// Ask the thread to finish. Returns once it has.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let mut s = self.status.lock().unwrap();
        s.state = ChatState::Idle;
        s.state_label = ChatState::Idle.label().to_string();
        s.seconds_until_next = None;
    }

    pub fn is_running(&self) -> bool {
        self.handle.as_ref().is_some_and(|h| !h.is_finished())
    }
}

impl Drop for ChatBot {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn set_state(status: &Mutex<ChatStatus>, state: ChatState) {
    let mut s = status.lock().unwrap();
    s.state = state;
    s.state_label = state.label().to_string();
}

fn record_error(status: &Mutex<ChatStatus>, err: &LouverError, state: ChatState) {
    let mut s = status.lock().unwrap();
    s.state = state;
    s.state_label = state.label().to_string();
    s.last_error = Some(err.message.clone());
    s.last_error_code = Some(err.code_str.clone());
}

fn run(
    ctx: Arc<BotContext>,
    broadcast_id: String,
    settings: ChatSettings,
    messages: Vec<ChatMessage>,
    status: Arc<Mutex<ChatStatus>>,
    stop: Arc<AtomicBool>,
) {
    let mut scheduler = ChatScheduler::new(settings, messages);
    let mut live_chat_id: Option<String> = None;
    let mut connected_at: Option<Instant> = None;
    let mut next_chat_poll = Instant::now();
    let mut retry_after: Option<Instant> = None;

    let token = |ctx: &BotContext| -> Result<String, LouverError> {
        ctx.tokens.access_token(&ctx.credentials, ctx.token_endpoint.as_ref())
    };

    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(TICK);
        if stop.load(Ordering::SeqCst) {
            break;
        }

        // Still backing off from a recoverable failure.
        if let Some(until) = retry_after {
            if Instant::now() < until {
                continue;
            }
            retry_after = None;
            set_state(&status, ChatState::Connected);
        }

        // 1. Find this broadcast's chat, if it has one yet.
        if live_chat_id.is_none() {
            if Instant::now() < next_chat_poll {
                continue;
            }
            next_chat_poll = Instant::now() + CHAT_POLL;

            let found = token(&ctx).and_then(|t| {
                YoutubeApi::with_base(ctx.http.as_ref(), ctx.api_base.clone())
                    .broadcast_by_id(&t, &broadcast_id)
            });
            match found {
                Ok(b) => {
                    update_broadcast_info(&status, &b);
                    if let Some(id) = b.active_live_chat_id.clone() {
                        live_chat_id = Some(id);
                        connected_at = Some(Instant::now());
                        // A new broadcast starts its rotation from the top.
                        scheduler.reset_for_new_broadcast();
                        status.lock().unwrap().live_chat_id_known = true;
                        set_state(&status, ChatState::Connected);
                        ctx.logger.info(LogTarget::Stream, "CHAT_CONNECTED: 실시간 채팅에 연결했습니다");
                    } else {
                        set_state(&status, ChatState::WaitingForLiveChat);
                    }
                }
                Err(e) => {
                    if is_fatal_for_bot(&e.code_str) {
                        record_error(&status, &e, ChatState::Error);
                        ctx.logger.warn(
                            LogTarget::Stream,
                            &format!("CHAT_STOPPED: {} ({})", e.message, e.code_str),
                        );
                        return;
                    }
                    record_error(&status, &e, ChatState::Paused);
                    retry_after = Some(Instant::now() + RETRY_BACKOFF);
                }
            }
            continue;
        }

        // 2. Send, when one is due.
        let Some(started) = connected_at else { continue };
        let elapsed = started.elapsed();
        {
            let mut s = status.lock().unwrap();
            s.messages_sent = scheduler.sent_count();
            s.seconds_until_next = scheduler.next_due().map(|due| due.saturating_sub(elapsed).as_secs());
        }
        if !scheduler.is_due(elapsed) {
            continue;
        }

        let Some(text) = scheduler.pick(pseudo_random()) else { continue };
        let chat_id = live_chat_id.clone().unwrap_or_default();

        set_state(&status, ChatState::Sending);
        let sent = token(&ctx).and_then(|t| {
            YoutubeApi::with_base(ctx.http.as_ref(), ctx.api_base.clone())
                .send_chat_message(&t, &chat_id, &text)
        });

        match sent {
            Ok(()) => {
                scheduler.mark_sent(text, elapsed);
                set_state(&status, ChatState::Connected);
                {
                    let mut s = status.lock().unwrap();
                    s.messages_sent = scheduler.sent_count();
                    s.last_error = None;
                    s.last_error_code = None;
                }
                // The text itself is not repeated into the log on every send;
                // §9 asks for the event, not a second copy of the message.
                ctx.logger.info(
                    LogTarget::Stream,
                    &format!("CHAT_MESSAGE_SENT: {}번째 메시지", scheduler.sent_count()),
                );
            }
            Err(e) => {
                ctx.logger
                    .warn(LogTarget::Stream, &format!("CHAT_MESSAGE_FAILED: {} ({})", e.message, e.code_str));
                if is_fatal_for_bot(&e.code_str) {
                    record_error(&status, &e, ChatState::Error);
                    ctx.logger.warn(LogTarget::Stream, &format!("CHAT_STOPPED: {}", e.code_str));
                    return;
                }
                // Recoverable: an auth blip, a rate limit, a 500. Wait and try
                // the same message again. The broadcast is untouched.
                record_error(&status, &e, ChatState::Paused);
                if e.code_str == "LL-YOUTUBE-002" {
                    ctx.tokens.invalidate_access_token();
                }
                retry_after = Some(Instant::now() + RETRY_BACKOFF);
                ctx.logger.info(
                    LogTarget::Stream,
                    &format!("CHAT_RETRY: {}초 후 다시 시도합니다", RETRY_BACKOFF.as_secs()),
                );
            }
        }
    }

    // One last message on the way out, if asked for (§6). Best effort: the
    // broadcast is already ending, so a failure here is logged and dropped
    // rather than retried.
    if scheduler.settings().send_on_end && scheduler.has_messages() {
        if let Some(chat_id) = live_chat_id {
            if let Some(text) = scheduler.pick(pseudo_random()) {
                let sent = token(&ctx).and_then(|t| {
                    YoutubeApi::with_base(ctx.http.as_ref(), ctx.api_base.clone())
                        .send_chat_message(&t, &chat_id, &text)
                });
                match sent {
                    Ok(()) => ctx.logger.info(LogTarget::Stream, "CHAT_MESSAGE_SENT: 마지막 인사 메시지"),
                    Err(e) => ctx.logger.warn(
                        LogTarget::Stream,
                        &format!("CHAT_MESSAGE_FAILED: 마지막 메시지 ({})", e.code_str),
                    ),
                }
            }
        }
    }

    set_state(&status, ChatState::Idle);
}

fn update_broadcast_info(status: &Mutex<ChatStatus>, b: &LiveBroadcast) {
    let mut s = status.lock().unwrap();
    s.broadcast_id = Some(b.id.clone());
    s.broadcast_title = Some(b.title.clone());
    s.broadcast_privacy = Some(b.privacy);
}

/// A cheap source of variation for random order. Not cryptographic, and does
/// not need to be.
fn pseudo_random() -> usize {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    RandomState::new().build_hasher().finish() as usize
}
