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
    ClientCredentials, ConsentPrompt, LoopbackServer, Pkce, TokenEndpoint, TokenStore,
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
    /// True once metadata has been pushed for the live session in progress, so
    /// it is applied once per broadcast rather than once per tick.
    applied_this_session: Arc<Mutex<bool>>,
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
    /// Whether saved metadata is pushed automatically when a broadcast starts.
    pub apply_on_start: bool,
    /// True when a developer has overridden the built-in OAuth client.
    pub using_custom_client: bool,
    /// Only the tail of the client id, and only in developer mode.
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
            applied_this_session: Arc::new(Mutex::new(false)),
        }
    }

    fn api_base(&self) -> String {
        self.db.get_setting_or(keys::API_BASE, API_BASE)
    }

    /// The OAuth client this app uses.
    ///
    /// Normally the one compiled into the build, so connecting is a single
    /// button and nobody has to open the Google Cloud console. A developer can
    /// override it, but only with 개발자 모드 on — the override is not part of
    /// the product's user interface.
    pub fn credentials(&self) -> Result<ClientCredentials> {
        if self.developer_mode() {
            if let Some(id) =
                self.db.get_setting(keys::CLIENT_ID).ok().flatten().filter(|s| !s.trim().is_empty())
            {
                return Ok(ClientCredentials {
                    client_id: id.trim().to_string(),
                    client_secret: self.tokens.stored_client_secret().unwrap_or_default(),
                });
            }
        }
        ClientCredentials::built_in().ok_or_else(|| {
            LouverError::with_detail(
                ErrorCode::YoutubeNotConnected,
                "이 빌드에는 Louver Live의 YouTube 클라이언트가 포함되어 있지 않습니다. 릴리스 빌드에는 자동으로 포함됩니다.",
            )
        })
    }

    fn developer_mode(&self) -> bool {
        self.db.get_setting_or(louver_core::settings_keys::DEVELOPER_MODE, "false") == "true"
    }

    pub fn has_credentials(&self) -> bool {
        self.credentials().is_ok()
    }

    /// Developer escape hatch for a custom OAuth client (§7).
    ///
    /// Refused unless 개발자 모드 is on, so it cannot be reached from the
    /// ordinary settings screen. An empty id clears the override and returns
    /// to the client the build ships with.
    pub fn set_credentials(&self, client_id: &str, client_secret: &str) -> Result<()> {
        if !self.developer_mode() {
            return Err(LouverError::with_detail(
                ErrorCode::ConfigInvalid,
                "자체 OAuth 클라이언트는 개발자 모드에서만 설정할 수 있습니다",
            ));
        }
        let id = client_id.trim();
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
            apply_on_start: self.apply_on_start_enabled(),
            using_custom_client: self.developer_mode()
                && client_id.as_deref().is_some_and(|c| !c.trim().is_empty()),
            // Only the tail, and only of the id, which is not a secret. Shown
            // in developer mode alone; the product UI has no place for it.
            client_id_hint: if self.developer_mode() {
                client_id.filter(|c| !c.trim().is_empty()).map(|c| {
                    let n = c.chars().count();
                    if n <= 12 {
                        c
                    } else {
                        format!("…{}", c.chars().skip(n - 12).collect::<String>())
                    }
                })
            } else {
                None
            },
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
    pub fn begin_connect(&self, switch_account: bool) -> Result<String> {
        let creds = self.credentials()?;
        let server = LoopbackServer::bind()?;
        // A fresh verifier and state for every attempt. Neither is reused, and
        // the verifier never leaves this process until the exchange.
        let pkce = Pkce::new();
        let url = louver_core::youtube::oauth::consent_url(
            &creds.client_id,
            &server.redirect_uri(),
            server.state(),
            &pkce,
            if switch_account { ConsentPrompt::SelectAccount } else { ConsentPrompt::Consent },
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
                    let tok = http.exchange_code(&creds, &auth.code, &auth.redirect_uri, &pkce.verifier)?;
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

    // --- metadata on start ------------------------------------------------

    pub fn apply_on_start_enabled(&self) -> bool {
        self.db.get_setting_or(keys::APPLY_ON_START, "true") == "true"
    }

    /// Is there an automatic apply still owed for the broadcast in progress?
    pub fn apply_on_start_pending(&self) -> bool {
        self.apply_on_start_enabled() && !*self.applied_this_session.lock().unwrap()
    }

    /// Forget what was done for the last broadcast. Called when the app is no
    /// longer live, so the next Start applies again.
    pub fn reset_live_session(&self) {
        *self.applied_this_session.lock().unwrap() = false;
    }

    /// The metadata the user saved in 방송 설정.
    pub fn saved_metadata(&self) -> BroadcastMetadata {
        BroadcastMetadata {
            title: self.db.get_setting_or(keys::METADATA_TITLE, ""),
            description: self.db.get_setting_or(keys::METADATA_DESCRIPTION, ""),
            tags: self
                .db
                .get_setting_or(keys::METADATA_TAGS, "")
                .lines()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .collect(),
            category_id: self.db.get_setting_or(keys::METADATA_CATEGORY, "10"),
            privacy: louver_core::youtube::Privacy::from_api(
                &self.db.get_setting_or(keys::METADATA_PRIVACY, "unlisted"),
            ),
        }
    }

    /// Push the saved metadata once for this broadcast.
    ///
    /// Marked done whether it worked or not: retrying every second would spend
    /// the day's API quota on a broadcast that is not there. The failure is
    /// logged, and 지금 YouTube에 적용 is always available.
    pub fn apply_on_start(&self) {
        *self.applied_this_session.lock().unwrap() = true;
        let meta = self.saved_metadata();
        if meta.title.trim().is_empty() {
            return; // nothing saved yet; not a failure worth reporting
        }
        match self.apply_metadata(&meta) {
            Ok(b) => self.logger.info(
                LogTarget::App,
                &format!("YOUTUBE_METADATA_UPDATED: 방송 시작과 함께 적용했습니다 ({})", b.id),
            ),
            Err(e) => self.logger.warn(
                LogTarget::App,
                &format!("방송 정보를 적용하지 못했습니다: {} ({})", e.message, e.code_str),
            ),
        }
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
