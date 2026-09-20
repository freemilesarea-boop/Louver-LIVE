//! Holds the YouTube connection for the app, and owns the chat bot's life.
//!
//! Separate from `AppState`'s broadcast fields on purpose: nothing here is
//! reachable from the broadcast tick, so a slow or failing Google cannot delay
//! FFmpeg by a single millisecond.

use louver_core::database::Database;
use louver_core::error::{ErrorCode, LouverError, Result};
use louver_core::logging::{LogTarget, Logger};
use louver_core::security::SecretStore;
use louver_core::youtube::api::YoutubeApi;
use louver_core::youtube::bot::{BotContext, ChatBot};
use louver_core::youtube::chat::{ChatMessage, ChatSettings, ChatState, ChatStatus};
use louver_core::youtube::http::UreqClient;
use louver_core::youtube::oauth::{
    ClientCredentials, LoopbackServer, TokenEndpoint, TokenStore, BUILT_IN_CLIENT_ID, BUILT_IN_CLIENT_SECRET,
};
use louver_core::youtube::{api::API_BASE, keys, BroadcastMetadata, ChannelInfo, HttpClient};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long the user has to finish the consent screen.
const CONSENT_TIMEOUT: Duration = Duration::from_secs(300);

pub struct YoutubeService {
    pub tokens: Arc<TokenStore>,
    http: Arc<UreqClient>,
    db: Database,
    logger: Arc<Logger>,
    bot: Mutex<Option<ChatBot>>,
    /// The broadcast the bot is attached to, so a restart is noticed.
    bot_broadcast: Mutex<Option<String>>,
    /// Set while a consent flow is in progress, for the UI to show.
    connecting: Arc<Mutex<Option<String>>>,
}

impl std::fmt::Debug for YoutubeService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YoutubeService").finish_non_exhaustive()
    }
}

/// What Settings shows about the connection.
#[derive(Debug, Clone, serde::Serialize)]
pub struct YoutubeStatus {
    pub connected: bool,
    pub channel_id: Option<String>,
    pub channel_title: Option<String>,
    /// False when no OAuth client has been configured yet.
    pub has_credentials: bool,
    pub client_id_hint: Option<String>,
    /// Where the refresh token is kept, so the user can see it is not the DB.
    pub secret_backend: String,
    pub secret_backend_is_secure: bool,
    /// Set while consent is pending.
    pub connecting_error: Option<String>,
}

impl YoutubeService {
    pub fn new(db: Database, secrets: Arc<dyn SecretStore>, logger: Arc<Logger>) -> Self {
        Self {
            tokens: Arc::new(TokenStore::new(secrets)),
            http: Arc::new(UreqClient::new()),
            db,
            logger,
            bot: Mutex::new(None),
            bot_broadcast: Mutex::new(None),
            connecting: Arc::new(Mutex::new(None)),
        }
    }

    fn api_base(&self) -> String {
        self.db.get_setting_or(keys::API_BASE, API_BASE)
    }

    /// The OAuth client for this installation.
    ///
    /// A build can bake one in; otherwise the user pastes their own from the
    /// Google Cloud console. The id lives in settings, the secret in the
    /// keychain with the refresh token.
    pub fn credentials(&self) -> Result<ClientCredentials> {
        let id = self
            .db
            .get_setting(keys::CLIENT_ID)
            .ok()
            .flatten()
            .filter(|s| !s.is_empty())
            .or_else(|| BUILT_IN_CLIENT_ID.map(str::to_string))
            .ok_or_else(|| {
                LouverError::with_detail(
                    ErrorCode::YoutubeNotConnected,
                    "YouTube API 클라이언트가 설정되지 않았습니다",
                )
            })?;
        let secret = self
            .tokens
            .stored_client_secret()
            .or_else(|| BUILT_IN_CLIENT_SECRET.map(str::to_string))
            .unwrap_or_default();
        Ok(ClientCredentials { client_id: id, client_secret: secret })
    }

    pub fn has_credentials(&self) -> bool {
        self.credentials().is_ok()
    }

    pub fn set_credentials(&self, client_id: &str, client_secret: &str) -> Result<()> {
        let id = client_id.trim();
        if id.is_empty() {
            return Err(LouverError::with_detail(ErrorCode::ConfigInvalid, "클라이언트 ID를 입력해주세요"));
        }
        self.db.set_setting(keys::CLIENT_ID, id)?;
        if !client_secret.trim().is_empty() {
            self.tokens.save_client_secret(client_secret.trim())?;
        }
        Ok(())
    }

    pub fn status(&self) -> YoutubeStatus {
        let connected = self.tokens.is_connected();
        let client_id = self.db.get_setting(keys::CLIENT_ID).ok().flatten();
        YoutubeStatus {
            connected,
            channel_id: self.db.get_setting(keys::CHANNEL_ID).ok().flatten(),
            channel_title: self.db.get_setting(keys::CHANNEL_TITLE).ok().flatten(),
            has_credentials: self.has_credentials(),
            // Only the tail, and only of the id, which is not a secret.
            client_id_hint: client_id.map(|c| {
                let n = c.chars().count();
                if n <= 12 {
                    c
                } else {
                    format!("…{}", c.chars().skip(n - 12).collect::<String>())
                }
            }),
            secret_backend: self.secret_backend(),
            secret_backend_is_secure: self.secret_is_secure(),
            connecting_error: self.connecting.lock().unwrap().clone(),
        }
    }

    fn secret_backend(&self) -> String {
        // Reported through the same store the stream key uses.
        self.tokens.backend_name().to_string()
    }

    fn secret_is_secure(&self) -> bool {
        self.tokens.is_secure()
    }

    /// Begin consent. Returns the URL the UI should open in a browser.
    ///
    /// The wait happens on its own thread so the window never freezes; the UI
    /// polls `status()` to find out how it went.
    pub fn begin_connect(&self) -> Result<String> {
        let creds = self.credentials()?;
        let server = LoopbackServer::bind()?;
        let url = louver_core::youtube::oauth::consent_url(
            &creds.client_id,
            &server.redirect_uri(),
            server.state(),
        );

        *self.connecting.lock().unwrap() = None;
        let tokens = Arc::clone(&self.tokens);
        let http = Arc::clone(&self.http);
        let db = self.db.clone();
        let logger = Arc::clone(&self.logger);
        let connecting = Arc::clone(&self.connecting);
        let api_base = self.api_base();

        std::thread::Builder::new()
            .name("louver-youtube-oauth".into())
            .spawn(move || {
                let outcome = (|| -> Result<ChannelInfo> {
                    let auth = server.wait_for_code(CONSENT_TIMEOUT)?;
                    let tok = http.exchange_code(&creds, &auth.code, &auth.redirect_uri)?;
                    let refresh = tok.refresh_token.filter(|t| !t.is_empty()).ok_or_else(|| {
                        LouverError::with_detail(
                            ErrorCode::YoutubeAuthExpired,
                            "Google이 refresh token을 주지 않았습니다. 계정 권한을 해제한 뒤 다시 연결해주세요.",
                        )
                    })?;
                    tokens.save_refresh_token(&refresh)?;
                    let token = tokens.access_token(&creds, http.as_ref())?;
                    YoutubeApi::with_base(http.as_ref(), api_base).my_channel(&token)
                })();

                match outcome {
                    Ok(ch) => {
                        let _ = db.set_setting(keys::CHANNEL_ID, &ch.id);
                        let _ = db.set_setting(keys::CHANNEL_TITLE, &ch.title);
                        // The channel name is not a secret; the token is, and
                        // never reaches this line.
                        logger.info(
                            LogTarget::App,
                            &format!("YOUTUBE_AUTH_CONNECTED: {} 채널에 연결했습니다", ch.title),
                        );
                    }
                    Err(e) => {
                        *connecting.lock().unwrap() = Some(e.message.clone());
                        logger.warn(LogTarget::App, &format!("YouTube 연결 실패: {}", e.code_str));
                    }
                }
            })
            .map_err(|e| {
                LouverError::with_detail(ErrorCode::YoutubeApiFailed, format!("연결을 시작하지 못했습니다: {e}"))
            })?;

        Ok(url)
    }

    pub fn disconnect(&self) -> Result<()> {
        self.stop_bot();
        self.tokens.disconnect()?;
        let _ = self.db.set_setting(keys::CHANNEL_ID, "");
        let _ = self.db.set_setting(keys::CHANNEL_TITLE, "");
        self.logger.info(LogTarget::App, "YouTube 계정 연결을 해제했습니다");
        Ok(())
    }

    fn token(&self) -> Result<String> {
        let creds = self.credentials()?;
        self.tokens.access_token(&creds, self.http.as_ref())
    }

    /// Push the saved metadata onto the current broadcast.
    ///
    /// Two calls, because YouTube splits the fields: title, description and
    /// privacy live on the broadcast, while tags and category live on the
    /// video. The video call merges rather than replaces (§3).
    pub fn apply_metadata(&self, meta: &BroadcastMetadata) -> Result<louver_core::youtube::LiveBroadcast> {
        let meta = meta.cleaned();
        meta.validate()?;
        let token = self.token()?;
        let base = self.api_base();
        let api = YoutubeApi::with_base(self.http.as_ref(), base);

        let broadcast = api.active_broadcast(&token)?;
        api.update_broadcast(&token, &broadcast.id, &meta, None)?;
        self.logger.info(
            LogTarget::App,
            &format!("YOUTUBE_METADATA_UPDATED: {} ({})", broadcast.id, meta.privacy.as_api()),
        );

        api.update_video_snippet(&token, &broadcast.id, &meta)?;
        self.logger.info(LogTarget::App, &format!("YOUTUBE_TAGS_UPDATED: {}개 태그", meta.tags.len()));
        Ok(broadcast)
    }

    /// Find whatever broadcast is on air, for the UI to show.
    pub fn current_broadcast(&self) -> Result<louver_core::youtube::LiveBroadcast> {
        let token = self.token()?;
        YoutubeApi::with_base(self.http.as_ref(), self.api_base()).active_broadcast(&token)
    }

    // --- chat bot ---------------------------------------------------------

    pub fn chat_settings(&self) -> ChatSettings {
        let flag = |k: &str, d: bool| self.db.get_setting_or(k, if d { "true" } else { "false" }) == "true";
        ChatSettings {
            enabled: flag(keys::CHAT_ENABLED, false),
            order: match self.db.get_setting_or(keys::CHAT_ORDER, "sequential").as_str() {
                "random" => louver_core::youtube::ChatOrder::Random,
                _ => louver_core::youtube::ChatOrder::Sequential,
            },
            interval_secs: self
                .db
                .get_setting_or(
                    keys::CHAT_INTERVAL,
                    &louver_core::youtube::chat::DEFAULT_INTERVAL_SECS.to_string(),
                )
                .parse()
                .unwrap_or(louver_core::youtube::chat::DEFAULT_INTERVAL_SECS),
            send_on_start: flag(keys::CHAT_ON_START, true),
            send_on_end: flag(keys::CHAT_ON_END, false),
            avoid_repeats: flag(keys::CHAT_AVOID_REPEATS, true),
        }
    }

    pub fn save_chat_settings(&self, s: &ChatSettings) -> Result<()> {
        s.validate()?;
        let b = |v: bool| if v { "true" } else { "false" };
        self.db.set_setting(keys::CHAT_ENABLED, b(s.enabled))?;
        self.db.set_setting(
            keys::CHAT_ORDER,
            match s.order {
                louver_core::youtube::ChatOrder::Random => "random",
                _ => "sequential",
            },
        )?;
        self.db.set_setting(keys::CHAT_INTERVAL, &s.interval_secs.to_string())?;
        self.db.set_setting(keys::CHAT_ON_START, b(s.send_on_start))?;
        self.db.set_setting(keys::CHAT_ON_END, b(s.send_on_end))?;
        self.db.set_setting(keys::CHAT_AVOID_REPEATS, b(s.avoid_repeats))?;
        Ok(())
    }

    pub fn chat_status(&self) -> ChatStatus {
        self.bot.lock().unwrap().as_ref().map(|b| b.status()).unwrap_or_default()
    }

    /// Start the bot for a broadcast, replacing any bot from a previous one.
    ///
    /// Called when the app goes live. A bot attached to a different broadcast
    /// is stopped rather than reused — its `liveChatId` belongs to a stream
    /// that has ended.
    pub fn start_bot_for(&self, broadcast_id: &str, messages: Vec<ChatMessage>) {
        {
            let current = self.bot_broadcast.lock().unwrap().clone();
            if current.as_deref() == Some(broadcast_id)
                && self.bot.lock().unwrap().as_ref().is_some_and(|b| b.is_running())
            {
                return; // already attached to this one
            }
        }
        self.stop_bot();

        let ctx = Arc::new(BotContext {
            http: Arc::clone(&self.http) as Arc<dyn HttpClient>,
            tokens: Arc::clone(&self.tokens),
            token_endpoint: Arc::clone(&self.http) as Arc<dyn TokenEndpoint>,
            credentials: match self.credentials() {
                Ok(c) => c,
                Err(_) => return,
            },
            api_base: self.api_base(),
            logger: Arc::clone(&self.logger),
        });

        let bot = ChatBot::start(ctx, broadcast_id.to_string(), self.chat_settings(), messages);
        *self.bot.lock().unwrap() = Some(bot);
        *self.bot_broadcast.lock().unwrap() = Some(broadcast_id.to_string());
    }

    /// Stop the bot without waiting for it.
    ///
    /// Called from the broadcast loop, which must not block: the bot may be
    /// mid-request, and with "마지막 메시지" switched on it sends one more on
    /// the way out. Both happen on a thread of their own.
    pub fn stop_bot(&self) {
        let taken = self.bot.lock().unwrap().take();
        *self.bot_broadcast.lock().unwrap() = None;
        if let Some(mut bot) = taken {
            let logger = Arc::clone(&self.logger);
            std::thread::spawn(move || {
                bot.stop();
                logger.info(LogTarget::Stream, "CHAT_STOPPED: 방송이 끝나 채팅 봇을 멈췄습니다");
            });
        }
    }

    pub fn bot_is_running(&self) -> bool {
        self.bot.lock().unwrap().as_ref().is_some_and(|b| b.is_running())
    }

    /// Which broadcast the bot is attached to, if any.
    pub fn bot_broadcast_id(&self) -> Option<String> {
        self.bot_broadcast.lock().unwrap().clone()
    }

    pub fn chat_state(&self) -> ChatState {
        self.chat_status().state
    }
}
