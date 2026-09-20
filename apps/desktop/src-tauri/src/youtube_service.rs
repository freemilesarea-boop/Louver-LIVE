//! Holds the YouTube connection for the app, and owns the chat bot's life.
//!
//! Separate from `AppState`'s broadcast fields on purpose: nothing here is
//! reachable from the broadcast tick, so a slow or failing Google cannot delay
//! FFmpeg by a single millisecond.

use louver_core::database::Database;
use louver_core::error::{ErrorCode, LouverError, Result};
use louver_core::logging::{LogTarget, Logger};
use louver_core::security::SecretStore;
use louver_core::security::StreamKeyStore;
use louver_core::youtube::api::YoutubeApi;
use louver_core::youtube::api::{LiveBroadcast, MetadataVerification};
use louver_core::youtube::bot::{BotContext, ChatBot};
use louver_core::youtube::chat::{ChatMessage, ChatSettings, ChatState, ChatStatus};
use louver_core::youtube::http::UreqClient;
use louver_core::youtube::oauth::{
    ClientCredentials, ConsentPrompt, LoopbackServer, Pkce, TokenEndpoint, TokenStore,
};
use louver_core::youtube::provision::{self, BroadcastChoice, GoLive, REUSE_TOLERANCE_SECS};
use louver_core::youtube::quota::{MeteredClient, QuotaGuard, QuotaState, FREE_DAILY_UNITS};
use louver_core::youtube::steps::{ProvisionOrigin, ProvisionStep, StepRecord, StepRecorder};
use louver_core::youtube::{api::API_BASE, keys, BroadcastMetadata, ChannelInfo, HttpClient};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long the user has to finish the consent screen.
const CONSENT_TIMEOUT: Duration = Duration::from_secs(300);

pub struct YoutubeService {
    pub tokens: Arc<TokenStore>,
    /// The raw transport. Used for the OAuth token endpoint, which is
    /// `oauth2.googleapis.com` and costs no YouTube quota.
    http: Arc<UreqClient>,
    /// Every YouTube Data API call goes through this, and there is no other
    /// way to reach the API from here — so the day's allowance cannot be
    /// spent by a call site that forgot to ask.
    api_http: Arc<MeteredClient>,
    quota: Arc<QuotaGuard>,
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
    /// What happened to the metadata for the broadcast in progress, for the
    /// dashboard to show field by field (§B-10).
    apply_state: Arc<Mutex<MetadataApplyState>>,
    /// The broadcast and stream this session provisioned, so the live
    /// transition and the stop know what to act on without asking again.
    live_session: Arc<Mutex<Option<ProvisionedBroadcast>>>,
    /// Reads the saved stream key, to match it against the channel's
    /// ingestion endpoints. Never stored or logged here.
    keys: Arc<StreamKeyStore>,
    /// The last ingestion state written to the log, so the per-tick check
    /// writes a line when it changes rather than every second.
    stream_active_logged: Arc<Mutex<Option<bool>>>,
    /// Whether the broadcast in progress was started by a person or by the
    /// scheduler, so the lines written after the start carry the same origin
    /// as the ones written before it and one run reads as one run.
    session_origin: Arc<Mutex<ProvisionOrigin>>,
}

/// The broadcast a window is using, and the stream it takes video from.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProvisionedBroadcast {
    pub broadcast_id: String,
    pub stream_id: String,
    /// True when YouTube will take it live and end it by itself.
    pub auto_start_stop: bool,
    /// Set once the broadcast has actually reached `live`.
    pub went_live: bool,
}

/// The day's free-quota spending, for the UI.
///
/// There is no paid tier behind this: the project carries no billing account,
/// so running out means the features pause until tomorrow, never a charge.
#[derive(Debug, Clone, serde::Serialize)]
pub struct QuotaReport {
    pub used_percent: u8,
    pub spent: u32,
    pub cap: u32,
    pub exhausted: bool,
    pub day: String,
    /// Methods with their own daily allowance, counted in calls.
    pub buckets: Vec<BucketReport>,
}

/// One method's own daily call allowance.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BucketReport {
    pub key: String,
    pub calls: u32,
    pub daily_calls: u32,
    pub exhausted: bool,
}

/// The result of one apply, including what Google says afterwards.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MetadataOutcome {
    pub broadcast: LiveBroadcast,
    pub verification: MetadataVerification,
}

/// How far the automatic metadata apply got for the current broadcast.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApplyStage {
    /// Nothing asked for: automatic apply is off, or nothing has been saved.
    #[default]
    Off,
    /// Asked for, but no account is connected — so it cannot happen at all.
    NotConnected,
    Applying,
    /// The user was told it could not be applied and chose to broadcast
    /// anyway, so YouTube keeps whatever it already had.
    Skipped,
    /// The day's free API allowance is gone. Metadata and chat wait for the
    /// quota to reset; the broadcast does not.
    QuotaExhausted,
    /// Applied, and Google reads back what was asked for.
    Applied,
    /// The calls succeeded but at least one field did not take.
    Mismatch,
    Failed,
}

/// The YouTube half of a broadcast's state, reported separately from the
/// stream's own state (§B-1). A connected RTMPS stream says nothing about
/// whether the title changed.
#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct MetadataApplyState {
    pub stage: ApplyStage,
    pub broadcast_id: Option<String>,
    pub requested: Option<BroadcastMetadata>,
    pub verification: Option<MetadataVerification>,
    pub error: Option<LouverError>,
    /// Which preparation step failed, in the user's words — "예약 방송 생성
    /// 실패" rather than "YouTube에 연결하지 못했습니다". Set only on a
    /// failure that got as far as making a call.
    pub failed_stage: Option<String>,
    /// What to try, under the stage.
    pub failed_remedy: Option<String>,
    /// Whether this attempt was started by a person or by the scheduler, so a
    /// scheduled failure and a manual success can be told apart on the screen
    /// as well as in the log.
    pub origin: Option<String>,
    /// Every step of the attempt, in order. What makes two runs comparable.
    pub steps: Vec<StepRecord>,
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
    /// Whether this build carries a client secret to send with the token
    /// exchange. The value itself is never exposed.
    pub has_client_secret: bool,
    /// The token endpoint's own words about the last failure, if any.
    pub last_auth_diagnostic: Option<String>,
    /// Only the tail of the client id, and only in developer mode.
    pub client_id_hint: Option<String>,
    /// Where the refresh token is kept, so the user can see it is not the DB.
    pub secret_backend: String,
    pub secret_backend_is_secure: bool,
    /// Set while consent is pending.
    pub connecting_error: Option<String>,
}

impl YoutubeService {
    pub fn new(
        db: Database,
        secrets: Arc<dyn SecretStore>,
        logger: Arc<Logger>,
        keys: Arc<StreamKeyStore>,
    ) -> Self {
        // The token endpoint is fixed in a shipped build and overridable only
        // by a setting no screen writes, so the refresh path can be driven
        // against a local fake in a test.
        let mut transport = UreqClient::new();
        transport.token_endpoint =
            db.get_setting(keys::TOKEN_ENDPOINT).ok().flatten().filter(|s| !s.trim().is_empty());
        let http = Arc::new(transport);
        // Restored rather than reset: a relaunch that started the day's count
        // at zero would spend an allowance that is already gone.
        let restored: QuotaState = db
            .get_setting(keys::QUOTA_STATE)
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let persist_db = db.clone();
        let quota = Arc::new(QuotaGuard::new(
            restored,
            FREE_DAILY_UNITS,
            Box::new(move |st| {
                if let Ok(json) = serde_json::to_string(st) {
                    let _ = persist_db.set_setting(keys::QUOTA_STATE, &json);
                }
            }),
        ));
        let api_http =
            Arc::new(MeteredClient::new(Arc::clone(&http) as Arc<dyn HttpClient>, Arc::clone(&quota)));
        Self {
            tokens: Arc::new(TokenStore::new(secrets)),
            http,
            api_http,
            quota,
            db,
            logger,
            bot: Mutex::new(None),
            bot_broadcast: Mutex::new(None),
            connecting: Arc::new(Mutex::new(None)),
            applied_this_session: Arc::new(Mutex::new(false)),
            apply_state: Arc::new(Mutex::new(MetadataApplyState::default())),
            live_session: Arc::new(Mutex::new(None)),
            keys,
            stream_active_logged: Arc::new(Mutex::new(None)),
            session_origin: Arc::new(Mutex::new(ProvisionOrigin::Manual)),
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
        ClientCredentials::resolve().ok_or_else(|| {
            LouverError::with_detail(
                ErrorCode::YoutubeNotConnected,
                "이 빌드에는 Louver Live의 YouTube 클라이언트가 포함되어 있지 않습니다. 릴리스 빌드에는 자동으로 포함됩니다.",
            )
        })
    }

    /// Does this build have a client secret to send?
    ///
    /// The value is never returned, logged or stored — only whether there is
    /// one, which is what the startup line and the UI need to know.
    pub fn has_client_secret(&self) -> bool {
        self.credentials().map(|c| c.has_secret()).unwrap_or(false)
    }

    /// The last token-endpoint failure, verbatim, for reporting.
    pub fn last_auth_diagnostic(&self) -> Option<String> {
        self.db.get_setting(keys::LAST_AUTH_DIAGNOSTIC).ok().flatten().filter(|s| !s.is_empty())
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
            has_client_secret: self.has_client_secret(),
            last_auth_diagnostic: self.last_auth_diagnostic(),
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
        let api_http = Arc::clone(&self.api_http);
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
                    // `channels.list` is a Data API call like any other, and
                    // is charged like one.
                    YoutubeApi::with_base(api_http.as_ref(), api_base).my_channel(&token)
                })();

                match outcome {
                    Ok(ch) => {
                        let _ = db.set_setting(keys::LAST_AUTH_DIAGNOSTIC, "");
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
                        // The detail is the token endpoint's own words: status,
                        // error and error_description. Kept so the question of
                        // whether this client needs a secret is settled by
                        // evidence rather than by argument.
                        let detail = e.detail.clone().unwrap_or_else(|| e.message.clone());
                        let _ = db.set_setting(keys::LAST_AUTH_DIAGNOSTIC, &detail);
                        *connecting.lock().unwrap() = Some(format!("{} ({})", e.message, detail));
                        logger.warn(
                            LogTarget::App,
                            &format!("YouTube 연결 실패: {} · {}", e.code_str, detail),
                        );
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

    /// The access token, with the refresh recorded as a step of its own.
    ///
    /// The first thing a scheduled start does, and the first thing that can
    /// fail: a manual broadcast that worked an hour ago proves the API calls
    /// are fine and proves nothing about the refresh, because that run was
    /// still holding the access token consent had just minted. The note says
    /// whether Google was actually asked.
    fn token_for(&self, rec: &StepRecorder) -> Result<String> {
        let cached = self.tokens.has_cached_access_token();
        let creds = self.credentials()?;
        rec.start(
            ProvisionStep::TokenRefresh,
            &format!(
                "cached={cached} · client_id={} · client_secret={}",
                if creds.client_id.is_empty() { "missing" } else { "configured" },
                if creds.has_secret() { "configured" } else { "missing" },
            ),
        );
        match self.tokens.access_token(&creds, self.http.as_ref()) {
            Ok(t) => {
                // Length and nothing else. It is the one fact about a token
                // that is worth having in a log and cannot be used.
                rec.ok(ProvisionStep::TokenRefresh, if cached { "캐시된 토큰 사용" } else { "새 토큰 발급" });
                Ok(t)
            }
            Err(e) => Err(rec.fail(ProvisionStep::TokenRefresh, &e)),
        }
    }

    /// Push the saved metadata onto the current broadcast, then read it back.
    ///
    /// Two writes, because YouTube splits the fields: title, description and
    /// privacy live on the broadcast, while tags and category live on the
    /// video. Both merge rather than replace (§3). The read afterwards is not
    /// belt and braces — a 200 from `liveBroadcasts.update` is entirely
    /// compatible with the watch page still showing the channel's default
    /// title, so the only honest answer to "did it apply" comes from asking
    /// Google what the resource says now.
    pub fn apply_metadata(&self, meta: &BroadcastMetadata) -> Result<MetadataOutcome> {
        let meta = meta.cleaned();
        meta.validate()?;
        let token = self.token()?;
        let base = self.api_base();
        let api = YoutubeApi::with_base(self.api_http.as_ref(), base);

        let rec = self.recorder(ProvisionOrigin::Manual);
        let broadcast = api.active_broadcast(&token)?;
        self.apply_to(&api, &token, &broadcast, &meta, &rec)
    }

    /// Push the metadata onto a broadcast that is already chosen, and read it
    /// back. Split out so the provisioning path and the manual
    /// 지금 YouTube에 적용 button run exactly the same three calls.
    ///
    /// The three calls are one step in the log rather than three: they succeed
    /// or fail together, and the error each one raises already names itself.
    fn apply_to(
        &self,
        api: &YoutubeApi,
        token: &str,
        broadcast: &LiveBroadcast,
        meta: &BroadcastMetadata,
        rec: &StepRecorder,
    ) -> Result<MetadataOutcome> {
        let token = token.to_string();
        let broadcast = broadcast.clone();
        let meta = meta.clone();
        rec.start(
            ProvisionStep::MetadataApply,
            &format!("{} · {}개 태그 · {}", broadcast.id, meta.tags.len(), meta.privacy.as_api()),
        );

        let outcome = (|| -> Result<MetadataOutcome> {
            api.update_broadcast(&token, &broadcast.id, &meta, broadcast.scheduled_start_time.as_deref())?;
            self.logger.info(
                LogTarget::App,
                &format!("YOUTUBE_METADATA_UPDATED: {} ({})", broadcast.id, meta.privacy.as_api()),
            );

            api.update_video_snippet(&token, &broadcast.id, &meta)?;
            self.logger.info(LogTarget::App, &format!("YOUTUBE_TAGS_UPDATED: {}개 태그", meta.tags.len()));

            let snippet = api.video_snippet(&token, &broadcast.id)?;
            let after = api.broadcast_by_id(&token, &broadcast.id).unwrap_or_else(|_| broadcast.clone());
            let verification =
                louver_core::youtube::api::verify_metadata(&meta, &snippet, after.privacy.as_api());
            Ok(MetadataOutcome { broadcast: after, verification })
        })();

        match outcome {
            Ok(out) => {
                if out.verification.all_applied() {
                    rec.ok(ProvisionStep::MetadataApply, &format!("{} · 전 항목 반영", broadcast.id));
                    self.logger.info(LogTarget::App, &format!("YOUTUBE_METADATA_VERIFIED: {}", broadcast.id));
                } else {
                    // Not an Err: every call returned 200, and the caller
                    // decides what a partial apply is worth. Recorded as a
                    // failed step all the same, because from the user's side
                    // "the title did not change" is the failure.
                    let missing = out.verification.mismatches().join(", ");
                    rec.fail(
                        ProvisionStep::MetadataApply,
                        &LouverError::with_detail(
                            ErrorCode::YoutubeApiFailed,
                            format!("반영되지 않은 항목: {missing}"),
                        ),
                    );
                    self.logger.warn(
                        LogTarget::App,
                        &format!(
                            "YOUTUBE_METADATA_MISMATCH: {} · 반영되지 않은 항목 {missing}",
                            broadcast.id
                        ),
                    );
                }
                Ok(out)
            }
            Err(e) => Err(rec.fail(ProvisionStep::MetadataApply, &e)),
        }
    }

    /// Get a broadcast ready for a window that is starting now.
    ///
    /// This is what replaced "make a live broadcast in YouTube first". The
    /// scheduler fires while the user is asleep, so the app finds the
    /// broadcast for this window or creates it, binds it to the ingestion
    /// endpoint the saved stream key publishes to, and applies the metadata —
    /// all before FFmpeg is launched, because a broadcast that goes live first
    /// is live under whatever title it already had.
    ///
    /// `window_start` is the occurrence's start; a manual start passes the
    /// current time.
    pub fn provision_broadcast(
        &self,
        meta: &BroadcastMetadata,
        window_start: chrono::DateTime<chrono::Utc>,
        window_end: Option<chrono::DateTime<chrono::Utc>>,
        rec: &StepRecorder,
    ) -> Result<MetadataOutcome> {
        let meta = meta.cleaned();
        meta.validate()?;
        let token = self.token_for(rec)?;
        let api = YoutubeApi::with_base(self.api_http.as_ref(), self.api_base());
        let window = format!(
            "window={}…{}",
            window_start.to_rfc3339(),
            window_end.map(|e| e.to_rfc3339()).unwrap_or_else(|| "열림".into())
        );

        // 1. The broadcast for this window: reuse one prepared earlier (a
        //    retry inside the window must not leave two behind) or create it.
        //
        //    Listed with `mine=true` and nothing else. Asking Google for
        //    `mine=true&broadcastStatus=upcoming` — one filter too many — is
        //    what refused every scheduled start on a real Mac with
        //    `incompatibleParameters`, so the narrowing to this window happens
        //    below, in memory, over pages of the channel's own broadcasts.
        let choice = rec
            .run(
                ProvisionStep::BroadcastList,
                &window,
                || {
                    let mut seen: Vec<louver_core::youtube::api::LiveBroadcast> = Vec::new();
                    let mut page: Option<String> = None;
                    let mut pages = 0usize;
                    loop {
                        let (items, next) = api.broadcasts_page(&token, page.as_deref())?;
                        seen.extend(items);
                        pages += 1;
                        let choice = provision::choose_broadcast(&seen, window_start, REUSE_TOLERANCE_SECS);
                        // Stop at the first page that answers the question. A
                        // channel with years of history is not worth reading to
                        // the end, at a quota unit a page, to learn what the first
                        // page already said.
                        if matches!(choice, BroadcastChoice::Reuse(_))
                            || next.is_none()
                            || pages >= louver_core::youtube::api::MAX_BROADCAST_PAGES
                        {
                            return Ok((choice, seen.len(), pages));
                        }
                        page = next;
                    }
                },
                |(choice, seen, pages)| {
                    format!(
                        "{seen}개 확인 ({pages}페이지) · {}",
                        match choice {
                            BroadcastChoice::Reuse(b) => format!("재사용 {}", b.id),
                            BroadcastChoice::Create => "이 예약에 맞는 방송 없음".to_string(),
                        }
                    )
                },
            )?
            .0;

        let broadcast = match choice {
            BroadcastChoice::Reuse(b) => {
                rec.skipped(ProvisionStep::BroadcastInsert, &format!("기존 방송 재사용 {}", b.id));
                self.logger.info(
                    LogTarget::App,
                    &format!("YOUTUBE_BROADCAST_REUSED: 이 예약의 방송을 다시 사용합니다 ({})", b.id),
                );
                b
            }
            BroadcastChoice::Create => {
                let created = rec.run(
                    ProvisionStep::BroadcastInsert,
                    &window,
                    || {
                        api.create_broadcast(
                            &token,
                            &meta,
                            &window_start.to_rfc3339(),
                            window_end.map(|e| e.to_rfc3339()).as_deref(),
                            true,
                        )
                    },
                    |b| format!("{} · autoStart={}", b.id, b.enable_auto_start),
                )?;
                self.logger.info(
                    LogTarget::App,
                    &format!("YOUTUBE_BROADCAST_CREATED: 예약 방송을 자동으로 만들었습니다 ({})", created.id),
                );
                created
            }
        };

        // 2. Bind it to the endpoint this app is actually publishing to. The
        //    key is read, compared and dropped; it is not logged or stored —
        //    which is why the step's note counts the endpoints rather than
        //    naming them.
        let stream_id = rec
            .run(
                ProvisionStep::StreamList,
                "",
                || {
                    let stream_key = self
                        .keys
                        .get()?
                        .filter(|k| !k.trim().is_empty())
                        .ok_or_else(|| LouverError::new(ErrorCode::StreamNoStreamKey))?;
                    let streams = api.my_streams(&token)?;
                    let found = provision::stream_for_key(&streams, &stream_key).map(|s| s.id.clone());
                    drop(stream_key);
                    Ok((found?, streams.len()))
                },
                |(id, n)| format!("stream={id} · 후보 {n}개"),
            )?
            .0;

        let bound = if broadcast.bound_stream_id.as_deref() == Some(stream_id.as_str()) {
            rec.skipped(
                ProvisionStep::BroadcastBind,
                &format!("{} 는 이미 {stream_id} 에 연결됨", broadcast.id),
            );
            broadcast
        } else {
            let b = rec.run(
                ProvisionStep::BroadcastBind,
                &format!("{} ← {stream_id}", broadcast.id),
                || {
                    let b = api.bind_broadcast(&token, &broadcast.id, &stream_id)?;
                    provision::verify_bound(&b, &stream_id)?;
                    Ok(b)
                },
                |b| format!("boundStreamId={}", b.bound_stream_id.clone().unwrap_or_default()),
            )?;
            self.logger
                .info(LogTarget::App, &format!("YOUTUBE_BROADCAST_BOUND: {} ← 스트림 {}", b.id, stream_id));
            b
        };

        *self.live_session.lock().unwrap() = Some(ProvisionedBroadcast {
            broadcast_id: bound.id.clone(),
            stream_id,
            auto_start_stop: bound.enable_auto_start,
            went_live: bound.life_cycle_status == "live",
        });

        // 3. The metadata, through the same three calls the manual button uses.
        self.apply_to(&api, &token, &bound, &meta, rec)
    }

    /// Write the stream's ingestion state, once per change.
    ///
    /// `YOUTUBE_STREAM_ACTIVE_WAIT` is the line that distinguishes "FFmpeg is
    /// running but YouTube is not receiving it" from "YouTube refused the
    /// transition" — the two look identical from the dashboard and have
    /// nothing in common.
    fn note_stream_status(&self, stream_id: &str, active: bool, status: &str) {
        let mut last = self.stream_active_logged.lock().unwrap();
        if *last == Some(active) {
            return;
        }
        *last = Some(active);
        if active {
            self.logger.info(
                LogTarget::App,
                &format!(
                    "YOUTUBE_STREAM_ACTIVE: {stream_id} 가 영상을 받고 있습니다 (streamStatus={status})"
                ),
            );
        } else {
            self.logger.info(
                LogTarget::App,
                &format!(
                    "YOUTUBE_STREAM_ACTIVE_WAIT: {stream_id} 가 아직 영상을 받지 못했습니다 (streamStatus={status})"
                ),
            );
        }
    }

    /// Record a failed attempt to take the broadcast live.
    ///
    /// Not fatal: FFmpeg is already publishing, and YouTube may still take it
    /// live by itself. The next tick tries again.
    pub fn note_go_live_failure(&self, e: &LouverError) {
        self.logger
            .warn(LogTarget::App, &format!("YOUTUBE_BROADCAST_LIVE 실패: {} ({})", e.message, e.code_str));
    }

    /// The broadcast this session provisioned, if any.
    pub fn provisioned(&self) -> Option<ProvisionedBroadcast> {
        self.live_session.lock().unwrap().clone()
    }

    /// Take the broadcast live once its stream is carrying video.
    ///
    /// Called from the broadcast loop after FFmpeg has connected. Returns true
    /// once the broadcast is live, so the caller can stop asking. YouTube
    /// refuses a transition while the stream is inactive, so the stream is
    /// checked first rather than the error being retried.
    pub fn try_go_live(&self) -> Result<bool> {
        let Some(session) = self.provisioned() else { return Ok(true) };
        if session.went_live {
            return Ok(true);
        }
        // This runs on every tick of the broadcast loop, so the two waiting
        // lines are written when the answer *changes*. A log that repeats
        // "still waiting" once a second is as unreadable as one that says
        // nothing.
        let rec = self.recorder(*self.session_origin.lock().unwrap());
        let token = self.token()?;
        let api = YoutubeApi::with_base(self.api_http.as_ref(), self.api_base());
        let broadcast = api.broadcast_by_id(&token, &session.broadcast_id)?;
        let stream = api.stream_by_id(&token, &session.stream_id)?;
        self.note_stream_status(&session.stream_id, stream.is_active(), &stream.stream_status);

        let done = match provision::go_live_step(&broadcast, &stream) {
            GoLive::WaitForStream => false,
            GoLive::AlreadyLive => true,
            GoLive::AutoStart => {
                // YouTube does it; this only notices when it has happened.
                false
            }
            GoLive::Transition => {
                let after = rec.run(
                    ProvisionStep::BroadcastTransition,
                    &format!("{} → live", session.broadcast_id),
                    || api.transition_broadcast(&token, &session.broadcast_id, "live"),
                    |b| format!("lifeCycleStatus={}", b.life_cycle_status),
                )?;
                self.logger.info(
                    LogTarget::App,
                    &format!("YOUTUBE_BROADCAST_LIVE: 방송을 LIVE로 전환했습니다 ({})", after.id),
                );
                after.life_cycle_status == "live"
            }
        };
        if done || broadcast.life_cycle_status == "live" {
            if let Some(s) = self.live_session.lock().unwrap().as_mut() {
                if !s.went_live {
                    s.went_live = true;
                    self.logger.info(
                        LogTarget::App,
                        &format!("YOUTUBE_BROADCAST_LIVE: {} 가 LIVE입니다", s.broadcast_id),
                    );
                }
            }
            return Ok(true);
        }
        Ok(false)
    }

    /// End the broadcast this session provisioned.
    ///
    /// Called when the window closes or the user stops. A broadcast left
    /// `live` on the channel with nothing publishing to it is exactly the
    /// stale state the next window would then try to reuse.
    pub fn finish_broadcast(&self) {
        *self.stream_active_logged.lock().unwrap() = None;
        let Some(session) = self.live_session.lock().unwrap().take() else { return };
        if session.auto_start_stop && !session.went_live {
            return; // never started; nothing on the channel to end
        }
        let ended = (|| -> Result<()> {
            let token = self.token()?;
            let api = YoutubeApi::with_base(self.api_http.as_ref(), self.api_base());
            api.transition_broadcast(&token, &session.broadcast_id, "complete")?;
            Ok(())
        })();
        match ended {
            Ok(()) => self.logger.info(
                LogTarget::App,
                &format!("YOUTUBE_BROADCAST_COMPLETE: 방송을 종료했습니다 ({})", session.broadcast_id),
            ),
            // autoStop may already have ended it, which is a success reported
            // as an error. Worth a line, never worth failing a stop over.
            Err(e) => self.logger.warn(
                LogTarget::App,
                &format!("YOUTUBE_BROADCAST_COMPLETE 실패: {} ({})", e.message, e.code_str),
            ),
        }
    }

    /// Find whatever broadcast is on air, for the UI to show.
    pub fn current_broadcast(&self) -> Result<louver_core::youtube::LiveBroadcast> {
        let token = self.token()?;
        YoutubeApi::with_base(self.api_http.as_ref(), self.api_base()).active_broadcast(&token)
    }

    // --- metadata on start ------------------------------------------------

    pub fn apply_on_start_enabled(&self) -> bool {
        self.db.get_setting_or(keys::APPLY_ON_START, "true") == "true"
    }

    /// Is there an automatic apply still owed for the broadcast in progress?
    pub fn apply_on_start_pending(&self) -> bool {
        self.apply_on_start_enabled() && !*self.applied_this_session.lock().unwrap()
    }

    /// Forget what was done for the last broadcast, so the next Start applies
    /// again.
    ///
    /// Deliberately does *not* clear [`Self::apply_state`]. That is the record
    /// of what happened to the metadata, and it is most wanted exactly when
    /// there is no broadcast running — after a start the user was asked about
    /// and declined, or after one that stopped. Every start decides it afresh,
    /// so nothing stale survives into the next broadcast.
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

    /// Record that the user chose to broadcast without the settings.
    ///
    /// Not a failure and not a silent default: they were shown what it means
    /// and said yes, so the dashboard says "적용 안 함" rather than 적용 실패.
    pub fn note_skipped_by_user(&self) {
        let meta = self.saved_metadata();
        self.set_apply_state(MetadataApplyState {
            stage: ApplyStage::Skipped,
            requested: Some(meta),
            ..Default::default()
        });
        self.logger
            .info(LogTarget::App, "YOUTUBE_METADATA_SKIPPED: 사용자가 설정 없이 방송 시작을 선택했습니다");
    }

    /// Is the day's free API allowance gone?
    pub fn quota_exhausted(&self) -> bool {
        self.quota.is_exhausted()
    }

    /// The day's API spending, for the UI to show.
    pub fn quota_state(&self) -> QuotaReport {
        let s = self.quota.snapshot();
        QuotaReport {
            used_percent: s.used_percent(self.quota.cap()),
            spent: s.spent,
            cap: self.quota.cap(),
            exhausted: s.exhausted,
            day: s.day.clone(),
            // Reported separately because they run out separately: a spent
            // bucket leaves the shared pool untouched.
            buckets: louver_core::youtube::quota::COSTS
                .iter()
                .filter_map(|(_, _, b)| *b)
                .map(|b| BucketReport {
                    key: b.key.to_string(),
                    calls: s.bucket_calls_made(b),
                    daily_calls: b.daily_calls,
                    exhausted: s.bucket_is_exhausted(b),
                })
                .collect(),
        }
    }

    /// A fresh recorder for one preparation attempt.
    pub fn recorder(&self, origin: ProvisionOrigin) -> StepRecorder {
        StepRecorder::new(Arc::clone(&self.logger), origin)
    }

    /// What the metadata apply did for the broadcast in progress.
    pub fn apply_state(&self) -> MetadataApplyState {
        self.apply_state.lock().unwrap().clone()
    }

    fn set_apply_state(&self, st: MetadataApplyState) {
        *self.apply_state.lock().unwrap() = st;
    }

    /// Would an automatic apply actually do something on the next Start?
    ///
    /// Used by the UI so it never tells the user their settings will be
    /// applied when there is no account to apply them with (§B-2).
    pub fn apply_on_start_wanted(&self) -> bool {
        self.apply_on_start_enabled() && !self.saved_metadata().title.trim().is_empty()
    }

    /// Put the broadcast's metadata in place, before the stream starts.
    ///
    /// Called from the runtime's pre-start hook, so a manual Start and a
    /// scheduled Start do exactly the same thing in exactly the same order.
    /// An `Err` here stops the broadcast from starting, which is the point:
    /// going live on the channel's default title while the app claims the
    /// user's title was applied is the failure this replaces.
    /// What a scheduled start does when the metadata cannot be applied.
    ///
    /// A manual start asks; nobody is at the keyboard for a scheduled one, so
    /// the answer has to be decided in advance. The default is the same as the
    /// manual default — do not go on air under settings the user did not
    /// choose — and a 24/7 channel that would rather stay on air can say so.
    pub fn schedule_holds_on_failure(&self) -> bool {
        self.db.get_setting_or(keys::SCHEDULE_ON_METADATA_FAILURE, "hold") != "broadcast"
    }

    pub fn prepare_for_broadcast(&self) -> Result<()> {
        self.prepare_for_broadcast_reason(false, None)
    }

    /// `scheduled` picks which of the two policies above applies; `window` is
    /// the occurrence being started, so a created broadcast carries the right
    /// scheduled start and end.
    pub fn prepare_for_broadcast_reason(
        &self,
        scheduled: bool,
        window: Option<(chrono::DateTime<chrono::Utc>, Option<chrono::DateTime<chrono::Utc>>)>,
    ) -> Result<()> {
        let origin = if scheduled { ProvisionOrigin::Scheduled } else { ProvisionOrigin::Manual };
        match self.prepare_inner(window, origin) {
            Ok(()) => Ok(()),
            Err(e) if scheduled && !self.schedule_holds_on_failure() => {
                // Recorded, never silent: the dashboard shows 적용 실패 and the
                // log carries the code. What it does not do is take a 24/7
                // channel off air over a title.
                self.logger.warn(
                    LogTarget::App,
                    &format!(
                        "YOUTUBE_METADATA_SKIPPED: 예약 방송은 설정 없이 계속합니다 — {} ({})",
                        e.message, e.code_str
                    ),
                );
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    fn prepare_inner(
        &self,
        window: Option<(chrono::DateTime<chrono::Utc>, Option<chrono::DateTime<chrono::Utc>>)>,
        origin: ProvisionOrigin,
    ) -> Result<()> {
        if !self.apply_on_start_wanted() {
            self.set_apply_state(MetadataApplyState::default());
            return Ok(());
        }
        let meta = self.saved_metadata();

        // The day's free allowance is gone. This is not something the user can
        // fix, and it is emphatically not a reason to take a 24/7 channel off
        // air — so unlike every other refusal here it returns Ok: the stream
        // starts, and the optional half waits for the quota to reset.
        if self.quota.is_exhausted() {
            let already = self.apply_state().stage == ApplyStage::QuotaExhausted;
            self.set_apply_state(MetadataApplyState {
                stage: ApplyStage::QuotaExhausted,
                requested: Some(meta),
                ..Default::default()
            });
            if !already {
                self.logger.warn(
                    LogTarget::App,
                    "YOUTUBE_QUOTA_EXHAUSTED: 오늘 무료 사용량을 모두 썼습니다. 방송은 계속하고 제목·채팅만 쉽니다",
                );
            }
            return Ok(());
        }

        if !self.status().connected {
            // A held scheduled window retries once a minute, and the cause
            // does not change in between, so say it once.
            let already = self.apply_state().stage == ApplyStage::NotConnected;
            self.set_apply_state(MetadataApplyState {
                stage: ApplyStage::NotConnected,
                requested: Some(meta),
                ..Default::default()
            });
            if !already {
                self.logger.warn(
                    LogTarget::App,
                    "YOUTUBE_METADATA_SKIPPED: 계정이 연결되지 않아 방송 정보를 적용할 수 없습니다",
                );
            }
            return Err(LouverError::with_detail(
                ErrorCode::YoutubeNotConnected,
                "방송 설정 자동 적용을 사용하려면 YouTube 계정 연결이 필요합니다.",
            ));
        }

        self.set_apply_state(MetadataApplyState {
            stage: ApplyStage::Applying,
            requested: Some(meta.clone()),
            origin: Some(origin.as_str().to_string()),
            ..Default::default()
        });
        *self.session_origin.lock().unwrap() = origin;
        let rec = self.recorder(origin);
        self.logger.info(
            LogTarget::App,
            &format!("YOUTUBE_PREPARE_START: origin={} · 방송 준비를 시작합니다", origin.as_str()),
        );

        // Provision rather than require. The old path opened with
        // `active_broadcast`, so a scheduled window that arrived with nothing
        // on the channel — the normal case — failed with "make a live
        // broadcast in YouTube first" and never started FFmpeg at all.
        let (start, end) = window.unwrap_or((chrono::Utc::now(), None));
        match self.provision_broadcast(&meta, start, end, &rec) {
            Ok(out) => {
                *self.applied_this_session.lock().unwrap() = true;
                let all = out.verification.all_applied();
                self.set_apply_state(MetadataApplyState {
                    stage: if all { ApplyStage::Applied } else { ApplyStage::Mismatch },
                    broadcast_id: Some(out.broadcast.id.clone()),
                    requested: Some(meta),
                    verification: Some(out.verification.clone()),
                    error: None,
                    failed_stage: (!all).then(|| ProvisionStep::MetadataApply.stage_label().to_string()),
                    failed_remedy: (!all).then(|| ProvisionStep::MetadataApply.remedy().to_string()),
                    origin: Some(origin.as_str().to_string()),
                    steps: rec.trace(),
                });
                if all {
                    self.logger.info(
                        LogTarget::App,
                        &format!(
                            "YOUTUBE_METADATA_UPDATED: 방송 시작 전에 적용했습니다 ({})",
                            out.broadcast.id
                        ),
                    );
                    Ok(())
                } else {
                    Err(LouverError::with_detail(
                        ErrorCode::YoutubeApiFailed,
                        format!(
                            "YouTube가 다음 항목을 반영하지 않았습니다: {}",
                            out.verification.mismatches().join(", ")
                        ),
                    ))
                }
            }
            Err(e) => {
                // The step that actually failed, not the feature that asked
                // for it. This is what the dashboard shows instead of the one
                // sentence every YouTube failure used to share.
                let failed = rec.failed_step();
                self.set_apply_state(MetadataApplyState {
                    stage: ApplyStage::Failed,
                    requested: Some(meta),
                    error: Some(e.clone()),
                    failed_stage: failed.map(|s| s.stage_label().to_string()),
                    failed_remedy: failed.map(|s| s.remedy().to_string()),
                    origin: Some(origin.as_str().to_string()),
                    steps: rec.trace(),
                    ..Default::default()
                });
                self.logger.warn(
                    LogTarget::App,
                    &format!(
                        "YOUTUBE_PREPARE_FAIL: origin={} · {} · {} ({})",
                        origin.as_str(),
                        failed.map(|s| s.stage_label()).unwrap_or("방송 준비 실패"),
                        e.message,
                        e.code_str
                    ),
                );
                Err(e)
            }
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
            // The bot gets the metered client too: it runs on its own thread
            // and would otherwise be the one place spending unbudgeted.
            http: Arc::clone(&self.api_http) as Arc<dyn HttpClient>,
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
