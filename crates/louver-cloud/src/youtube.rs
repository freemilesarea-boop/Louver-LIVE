//! Connecting a YouTube account, and driving its live broadcasts. §1–§10.
//!
//! What this adds is the half of a broadcast that a stream key cannot do: create
//! the live broadcast, title it, set its privacy, make an ingestion endpoint of
//! its own and bind the two together. What it deliberately does **not** touch is
//! how video is sent. A YouTube-connected broadcast ends up with an ordinary
//! `stream_destinations` row — an address and a sealed key — and from there the
//! FFmpeg worker, the watchdog, the playlist and the recovery path are the same
//! code that is on air now with a pasted key.
//!
//! Nothing here is reimplemented: the API client, the error classification and
//! the OAuth URL and form builders are `louver-core`'s, already exercised by the
//! desktop app against a fake Google. This is the multi-account, server-side
//! storage and orchestration around them.

use crate::db::CloudDb;
use crate::{CloudError, Result};
use louver_core::error::ErrorCode;
use louver_core::security::SecretStore;
use louver_core::youtube::metadata::{BroadcastMetadata, Privacy as YtPrivacy};
use louver_core::youtube::oauth::{
    self, ClientCredentials, ConsentPrompt, Pkce, TokenEndpoint, TokenResponse,
};
use louver_core::youtube::quota::ApiMethod;
use louver_core::youtube::{ChannelInfo, HttpClient, YoutubeApi};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// How long an unused consent attempt stays valid. §15: single use, and it
/// expires — an authorization URL left open in a tab yesterday is not a key.
pub const STATE_TTL_MINUTES: i64 = 15;

/// A connected channel, as the UI and the API see it. No token, ever.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YoutubeAccount {
    pub id: String,
    pub user_id: String,
    pub provider: String,
    pub channel_id: String,
    pub channel_title: String,
    pub thumbnail_url: Option<String>,
    /// When the cached access token stops being usable. The token itself is
    /// sealed, not here.
    pub token_expiry: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub last_verified_at: Option<String>,
}

/// Where a broadcast's YouTube resources are, once they exist.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct YoutubeLink {
    pub account_id: Option<String>,
    pub broadcast_id: Option<String>,
    pub stream_id: Option<String>,
    /// `waiting_for_ingest` / `ready` / `live` / `complete` / `error`, as last
    /// seen from YouTube — never inferred from FFmpeg being alive.
    pub status: Option<String>,
    /// When `status` was last confirmed with YouTube, explicit-UTC. `None` means
    /// nobody has ever managed to ask — not that the answer was no.
    pub checked_at: Option<String>,
    pub watch_url: Option<String>,
    pub last_error: Option<String>,
}

/// YouTube's own view of a broadcast, kept apart from FFmpeg's. §8.
pub const WAITING: &str = "waiting_for_ingest";
pub const READY: &str = "ready";
/// YouTube is receiving video into the monitor stream's private preview and the
/// public watch page has nothing to play. Its own state, because collapsing it
/// into `ready` is what made the fault invisible: a broadcast in `testing` is
/// not waiting for anything, it is waiting for a transition that nobody sends.
pub const TESTING: &str = "testing";
pub const LIVE: &str = "live";
pub const COMPLETE: &str = "complete";
pub const ERRORED: &str = "error";

/// What still has to happen before YouTube is actually live.
///
/// A plain function of what YouTube reported, so the decision can be tested
/// without a network and read without following four call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextStep {
    /// Live, or finished. Nothing left to do.
    Settled,
    /// YouTube has not noticed the ingest yet.
    AwaitIngest,
    /// YouTube will take it live by itself once it notices. Wait.
    AwaitAutoStart,
    /// It will not start by itself, whatever we wait for.
    NeedsTransition(Stuck),
}

/// Why a broadcast cannot start itself. The two cases need different words to
/// the customer, so they are not one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stuck {
    /// The monitor stream is on, so the ingest lands in `testing` and stops
    /// there. Every broadcast this service created before the fix is like this.
    MonitorStream,
    /// `enableAutoStart` is off on this broadcast.
    AutoStartOff,
}

impl Stuck {
    /// What to tell the customer, and what they or the operator can do.
    pub fn explain(self) -> &'static str {
        match self {
            Self::MonitorStream => {
                "YouTube가 영상을 받고 있지만 '미리보기(testing)' 단계에 머물러 있어 시청 페이지에서는 아직 방송이 시작되지 않았습니다. 이 방송은 미리보기가 켜진 상태로 만들어졌기 때문에 스스로 시작하지 못합니다. YouTube 스튜디오에서 '실시간 시작'을 누르면 즉시 공개됩니다."
            }
            Self::AutoStartOff => {
                "YouTube가 영상을 받고 있지만 이 방송은 자동 시작이 꺼져 있어 스스로 시작하지 못합니다. YouTube 스튜디오에서 '실시간 시작'을 누르면 즉시 공개됩니다."
            }
        }
    }
}

/// Read YouTube's answer and say what it means. Pure.
pub fn next_step(
    life_cycle_status: &str,
    stream_active: bool,
    starts_itself: bool,
    monitor: bool,
) -> NextStep {
    match life_cycle_status {
        "live" | "complete" => NextStep::Settled,
        // Receiving video, parked in the preview. Only a transition leaves it,
        // and autostart does not send one.
        "testing" => NextStep::NeedsTransition(Stuck::MonitorStream),
        _ if !stream_active => NextStep::AwaitIngest,
        // The ingest is up and YouTube has not moved the broadcast. Whether
        // that is going to change on its own is exactly what these two say.
        _ if starts_itself => NextStep::AwaitAutoStart,
        _ if monitor => NextStep::NeedsTransition(Stuck::MonitorStream),
        _ => NextStep::NeedsTransition(Stuck::AutoStartOff),
    }
}

/// The status to store for what YouTube reported.
pub fn status_for(life_cycle_status: &str, stream_active: bool) -> &'static str {
    match life_cycle_status {
        "live" => LIVE,
        "complete" => COMPLETE,
        "testing" => TESTING,
        _ if stream_active => READY,
        _ => WAITING,
    }
}

/// One answer from YouTube, and what it means.
#[derive(Debug, Clone, Copy)]
pub struct Observed {
    /// What was stored on the broadcast row.
    pub status: &'static str,
    pub step: NextStep,
    /// True when this poll sent a transition and YouTube accepted it.
    pub transitioned: bool,
}

impl Observed {
    /// Nothing more will change by waiting, so the watch can stop asking.
    ///
    /// Only `complete`. `live` used to be in here, and it was the same mistake
    /// as the one this change is about: a broadcast that YouTube ends by itself
    /// — `enableAutoStop`, an operator in Studio, a strike — leaves 247streams
    /// saying 방송 중 for ever, because the one thing that could notice had
    /// already stopped looking. A live broadcast stays in the slow phase and is
    /// re-confirmed for as long as the encoder runs.
    pub fn settled(&self) -> bool {
        self.status == COMPLETE
    }
}

/// Is this service allowed to send `liveBroadcasts.transition` by itself?
///
/// Off unless the operator turns it on. A transition is a write against a
/// customer's channel, and one sent at the wrong moment ends a broadcast that
/// was working. With it off the fault is still detected and explained; with it
/// on a broadcast stuck in the preview is rescued without anyone opening Studio.
/// Broadcasts created after the monitor-stream fix do not need it either way.
pub fn auto_transition_enabled() -> bool {
    matches!(
        std::env::var("LOUVER_YOUTUBE_AUTO_TRANSITION")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "on" | "yes"
    )
}

/// Where the sealed credentials for one connected account live.
pub fn refresh_account(account_id: &str) -> String {
    format!("youtube:{account_id}:refresh")
}

pub fn access_account(account_id: &str) -> String {
    format!("youtube:{account_id}:access")
}

/// Where Google sends the browser back to, when nothing says otherwise.
pub const DEFAULT_REDIRECT_URI: &str = "http://localhost:8080/api/youtube/oauth/callback";

/// Everything about the Google client this server is.
///
/// A value rather than a set of `std::env::var` calls, for two reasons. The
/// environment is process-global, and this repository has already been bitten
/// once by a test's `set_var` racing another test's read — see the note in
/// `apps/desktop/src-tauri/tests/provisioning_log.rs`. And a configuration that
/// is a value can be handed to a fake Google in a test without touching the
/// process at all.
#[derive(Clone)]
pub struct Config {
    pub credentials: ClientCredentials,
    /// Must match what is registered in the Google Cloud console, exactly.
    pub redirect_uri: String,
    /// The API host. `None` is Google; a test points it at a fake.
    pub api_base: Option<String>,
}

/// `client_secret` must not be printable, even by accident.
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("client_id", &self.credentials.client_id)
            .field("client_secret", &"[REDACTED]")
            .field("redirect_uri", &self.redirect_uri)
            .finish()
    }
}

impl Config {
    /// The Google client, from the environment and nowhere else. §0.
    ///
    /// Never from the database, never from a request, never compiled in: a
    /// client secret in a row is a client secret in a backup.
    pub fn from_env() -> Result<Self> {
        let client_id = std::env::var("YOUTUBE_CLIENT_ID").unwrap_or_default();
        let client_secret = std::env::var("YOUTUBE_CLIENT_SECRET").unwrap_or_default();
        if client_id.trim().is_empty() || client_secret.trim().is_empty() {
            return Err(CloudError::Invalid(
                "YouTube 연결이 설정되지 않았습니다. 서버에 YOUTUBE_CLIENT_ID 와 YOUTUBE_CLIENT_SECRET 을 설정해 주세요."
                    .into(),
            ));
        }
        Ok(Self {
            credentials: ClientCredentials {
                client_id: client_id.trim().into(),
                client_secret: client_secret.trim().into(),
            },
            redirect_uri: redirect_uri(),
            api_base: std::env::var("YOUTUBE_API_BASE").ok().filter(|s| !s.trim().is_empty()),
        })
    }
}

/// Where Google sends the browser back to. §0: configurable, because it has to
/// match what is registered in the Google Cloud console exactly.
///
/// Read directly by the route that reports it, so an operator can check the
/// value without a connected account.
pub fn redirect_uri() -> String {
    std::env::var("YOUTUBE_OAUTH_REDIRECT_URI")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_REDIRECT_URI.to_string())
}

/// Is YouTube connecting available on this server at all?
pub fn is_configured() -> bool {
    Config::from_env().is_ok()
}

/// Everything the provider needs, and nothing it does not.
///
/// `http` and `tokens` are traits so that every path below can be driven
/// against a fake Google — no test in this repository talks to the real API.
pub struct Youtube {
    db: CloudDb,
    keys: Arc<dyn SecretStore>,
    http: Arc<dyn HttpClient>,
    tokens: Arc<dyn TokenEndpoint>,
    config: Config,
    /// May this service send `liveBroadcasts.transition` by itself? A field
    /// rather than an `std::env::var` at the point of use, so a test can set it
    /// without `set_var` — which is process-global and has already raced two of
    /// this repository's test threads once.
    auto_transition: bool,
    /// Opened against a database that refuses writes, for the audit.
    ///
    /// The only thing it changes is that caching a refreshed access token is
    /// allowed to fail: the cache is a convenience, the token works in memory
    /// regardless, and an hourly expiry would otherwise make a read-only
    /// diagnosis fail for every account. It is set by the audit entry point
    /// and by nothing else — deliberately not an environment variable, so a
    /// running server can never be talked into swallowing a write failure.
    read_only: bool,
    /// May a read-only audit call Google's token endpoint?
    ///
    /// Off by default, and the reason is not caution for its own sake. A
    /// refresh can come back with a *rotated* refresh token, and on a
    /// read-only database the rotation cannot be stored — which would leave
    /// the customer's channel connected to a token nobody has. Google rarely
    /// rotates, but "rarely" is not a risk to take on a live account without
    /// being asked. So the default audit uses the cached token the running
    /// server keeps fresh, and says so plainly when it has expired.
    read_only_may_refresh: bool,
}

impl std::fmt::Debug for Youtube {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Youtube")
    }
}

impl Clone for Youtube {
    fn clone(&self) -> Self {
        Self {
            db: self.db.clone(),
            keys: Arc::clone(&self.keys),
            http: Arc::clone(&self.http),
            tokens: Arc::clone(&self.tokens),
            config: self.config.clone(),
            auto_transition: self.auto_transition,
            read_only: self.read_only,
            read_only_may_refresh: self.read_only_may_refresh,
        }
    }
}

impl Youtube {
    pub fn new(
        db: CloudDb,
        keys: Arc<dyn SecretStore>,
        http: Arc<dyn HttpClient>,
        tokens: Arc<dyn TokenEndpoint>,
        config: Config,
    ) -> Self {
        Self {
            db,
            keys,
            http,
            tokens,
            config,
            auto_transition: auto_transition_enabled(),
            read_only: false,
            read_only_may_refresh: false,
        }
    }

    /// For a diagnosis reading a database it must not write to.
    ///
    /// Also forces the transition policy off: a read-only audit has no business
    /// writing to a customer's channel whatever the operator switch says.
    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self.auto_transition = false;
        self
    }

    /// Let a read-only audit refresh an expired access token.
    ///
    /// The operator's decision, not a default: see `read_only_may_refresh`.
    pub fn read_only_may_refresh(mut self, on: bool) -> Self {
        self.read_only_may_refresh = on;
        self
    }

    /// Override the transition policy. For tests and for a caller that has its
    /// own reason; the default comes from the environment.
    pub fn with_auto_transition(mut self, on: bool) -> Self {
        self.auto_transition = on;
        self
    }

    /// The exact URI Google has to have registered. Not a secret; the single
    /// most common thing to get wrong.
    pub fn redirect_uri(&self) -> &str {
        &self.config.redirect_uri
    }

    fn api(&self) -> YoutubeApi<'_> {
        match &self.config.api_base {
            Some(base) => YoutubeApi::with_base(self.http.as_ref(), base.clone()),
            None => YoutubeApi::new(self.http.as_ref()),
        }
    }

    /// One API call, with a fresh token if Google says the old one is stale.
    ///
    /// §4: a 401 is not a failure to report to the user, it is a token to
    /// replace. The retry happens exactly once — a second 401 on a token minted
    /// seconds earlier means the grant itself is gone, and looping would turn
    /// that into a hang instead of the "다시 연결해 주세요" it deserves.
    ///
    /// Everything that fails for any other reason is recorded against the
    /// broadcast on the way past, so the dashboard can say which step failed.
    fn call_api<T>(
        &self,
        broadcast_id: &str,
        account_id: &str,
        method: ApiMethod,
        mut call: impl FnMut(&YoutubeApi<'_>, &str) -> louver_core::error::Result<T>,
    ) -> Result<T> {
        let api = self.api();
        let token = self.access_token(account_id)?;
        match call(&api, &token) {
            Ok(v) => Ok(v),
            Err(e) if e.code == ErrorCode::YoutubeAuthExpired => {
                // The guard above is silent by design — a replaced token is not
                // news. It is news when the replacement is refused too, and
                // without this line the two attempts' transport logs sit next
                // to each other with nothing saying which is which.
                //
                // `say` and not `note`: this is a boundary marker for the log
                // stream the transport lines are already in, and `note` would
                // also write a `broadcast_events` row — a dashboard entry a
                // user would see, which a diagnosis has no business adding.
                crate::manager::say(
                    broadcast_id,
                    "youtube",
                    &format!("{} 첫 시도 401 — token 갱신 후 재시도", method.name()),
                );
                let fresh = self.refresh_access_token(account_id)?;
                call(&api, &fresh).map_err(|e| self.explain(broadcast_id, method, e))
            }
            Err(e) => Err(self.explain(broadcast_id, method, e)),
        }
    }

    // --- OAuth ------------------------------------------------------------

    /// The URL to send the browser to, and the single-use state behind it.
    pub fn consent_url(&self, user_id: &str) -> Result<String> {
        let creds = &self.config.credentials;
        let pkce = Pkce::new();
        let state = self.db.create_oauth_state(user_id, &pkce.verifier)?;
        Ok(oauth::consent_url(
            &creds.client_id,
            &self.config.redirect_uri,
            &state,
            &pkce,
            // Without this Google issues no refresh token for an account that
            // has consented before, and the connection would work for an hour
            // and then stop.
            ConsentPrompt::Consent,
        ))
    }

    /// Finish the flow: trade the code for tokens, ask whose channel it is, and
    /// store the account. Returns the account and the user it belongs to.
    pub fn complete_consent(&self, state: &str, code: &str) -> Result<YoutubeAccount> {
        let creds = &self.config.credentials;
        // Single use and time limited. An unknown, used or expired state is
        // refused before anything is exchanged.
        let claim = self.db.claim_oauth_state(state, STATE_TTL_MINUTES)?;

        let tokens = self
            .tokens
            .exchange_code(creds, code, &self.config.redirect_uri, &claim.verifier)
            .map_err(|e| CloudError::Engine(e.message))?;
        let refresh = tokens.refresh_token.clone().ok_or_else(|| {
            CloudError::Invalid(
                "Google이 refresh token을 주지 않았습니다. 다시 시도하면 동의 화면이 나타납니다.".into(),
            )
        })?;

        let channel = self.channel_for(&tokens.access_token)?;
        let thumb = self.channel_thumbnail(&tokens.access_token);
        let account =
            self.db.upsert_youtube_account(&claim.user_id, &channel.id, &channel.title, thumb.as_deref())?;

        // Sealed, both of them, under this account's own names.
        self.seal(&refresh_account(&account.id), &refresh)?;
        self.cache_access_token(&account.id, &tokens)?;
        self.db.touch_youtube_account(&account.id)?;
        self.db.youtube_account_owned(&claim.user_id, &account.id)
    }

    fn seal(&self, account: &str, value: &str) -> Result<()> {
        self.keys.set(account, value).map_err(|e| CloudError::Engine(e.message))
    }

    fn cache_access_token(&self, account_id: &str, tokens: &TokenResponse) -> Result<()> {
        if self.read_only {
            // Nothing to cache into, and nothing lost: the caller already has
            // the token it needs. Said out loud so an operator reading the
            // audit knows why the next run will refresh again.
            crate::manager::say(
                account_id,
                "youtube",
                "읽기 전용 모드이므로 access token 을 저장하지 않았습니다",
            );
            return Ok(());
        }
        self.seal(&access_account(account_id), &tokens.access_token)?;
        let seconds = tokens.expires_in.unwrap_or(3600).min(24 * 3600) as i64;
        // A minute of margin, so a call started just before expiry does not
        // arrive just after it.
        self.db.set_youtube_token_expiry(account_id, seconds - 60)
    }

    /// A usable access token for this account, refreshing when it has to. §8.
    pub fn access_token(&self, account_id: &str) -> Result<String> {
        if !self.db.youtube_token_expired(account_id)? {
            if let Ok(Some(cached)) = self.keys.get(&access_account(account_id)) {
                if !cached.trim().is_empty() {
                    return Ok(cached);
                }
            }
        }
        if self.read_only && !self.read_only_may_refresh {
            // Not a failure of the audit so much as its honesty: refreshing
            // here could rotate a refresh token this process cannot store.
            return Err(CloudError::Invalid(
                "저장된 access token 이 만료되었습니다. 읽기 전용 점검은 토큰을 갱신하지 않습니다".into(),
            ));
        }
        self.refresh_access_token(account_id)
    }

    fn refresh_access_token(&self, account_id: &str) -> Result<String> {
        let creds = &self.config.credentials;
        let refresh = self
            .keys
            .get(&refresh_account(account_id))
            .map_err(|e| CloudError::Engine(e.message))?
            .filter(|t| !t.trim().is_empty())
            .ok_or_else(|| {
                CloudError::Invalid("YouTube 계정 연결이 만료되었습니다. 다시 연결해 주세요.".into())
            })?;

        // `CloudError::from` and not `Engine(e.message)`: the message is the
        // sentence the user reads — "다시 연결해주세요" — and on its own it tells
        // an operator nothing about *why* Google refused. What they need is in
        // the detail `post_token` already built: the grant, the status, and
        // Google's own `error` and `error_description` (`invalid_grant`,
        // "Token has been expired or revoked"). The `From` impl in `lib.rs`
        // keeps it; taking `e.message` by hand dropped it here, which is the
        // one thing this line exists to fix. The detail passed through
        // `mask_secrets` on its way out of the core, and the refresh token and
        // client secret were never in it.
        let tokens = self.tokens.refresh(creds, &refresh).map_err(CloudError::from)?;
        // §2: Google does not return a refresh token on every refresh, and
        // writing the absent one would disconnect the account for good.
        if let Some(new_refresh) = tokens.refresh_token.as_deref() {
            if !new_refresh.trim().is_empty() && new_refresh != refresh {
                if self.read_only {
                    // The rotation already happened at Google's end; the only
                    // thing left is to say so, because the stored token may no
                    // longer work and only a reconnect fixes that.
                    crate::manager::say(
                        account_id,
                        "youtube",
                        "경고: Google이 refresh token 을 교체했고 읽기 전용 모드라 저장할 수 없었습니다.                          이 채널은 다시 연결해야 할 수 있습니다",
                    );
                } else {
                    self.seal(&refresh_account(account_id), new_refresh)?;
                }
            }
        }
        self.cache_access_token(account_id, &tokens)?;
        Ok(tokens.access_token)
    }

    fn channel_for(&self, token: &str) -> Result<ChannelInfo> {
        self.api().my_channel(token).map_err(|e| CloudError::Engine(e.message))
    }

    /// Best effort: a channel picture makes the connected account recognisable,
    /// and its absence is not a reason to fail a connection.
    fn channel_thumbnail(&self, token: &str) -> Option<String> {
        let base =
            self.config.api_base.clone().unwrap_or_else(|| "https://www.googleapis.com/youtube/v3".into());
        let url = format!("{base}/channels?part=snippet&mine=true");
        let (status, body) = self.http.request("GET", &url, token, None).ok()?;
        if status != 200 {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(&body).ok()?;
        v["items"][0]["snippet"]["thumbnails"]["default"]["url"].as_str().map(str::to_string)
    }

    /// Forget an account: its sealed tokens go, and so does the row. §2.
    pub fn disconnect(&self, user_id: &str, account_id: &str) -> Result<()> {
        forget_account(&self.db, &self.keys, user_id, account_id)
    }

    // --- broadcast preparation (§5, §6, §7) -------------------------------

    /// Create the YouTube broadcast, its ingestion stream, and bind them.
    ///
    /// Returns the destination the FFmpeg worker will use. The stream key is
    /// sealed on the way through and is not in the return value.
    pub fn provision(&self, user_id: &str, broadcast_id: &str, account_id: &str) -> Result<()> {
        let b = self.db.broadcast_owned(user_id, broadcast_id)?;
        self.db.youtube_account_owned(user_id, account_id)?;

        let meta = metadata_of(&b);
        let start = scheduled_start_for(&b);
        let end = scheduled_end_for(&b, &start);

        let broadcast =
            self.call_api(broadcast_id, account_id, ApiMethod::LiveBroadcastsInsert, |api, token| {
                api.create_broadcast(token, &meta, &start, end.as_deref(), true)
            })?;

        // §6: a stream of this broadcast's own, so that three concurrent
        // broadcasts cannot end up publishing to one key.
        let title = format!("247streams / {}", b.name);
        let stream =
            self.call_api(broadcast_id, account_id, ApiMethod::LiveStreamsInsert, |api, token| {
                api.create_stream(token, &title)
            })?;

        let bound =
            self.call_api(broadcast_id, account_id, ApiMethod::LiveBroadcastsBind, |api, token| {
                api.bind_broadcast(token, &broadcast.id, &stream.id)
            })?;
        if bound.bound_stream_id.as_deref() != Some(stream.id.as_str()) {
            return Err(CloudError::Engine(
                "YouTube가 방송과 스트림을 연결하지 못했습니다. 잠시 후 다시 시도해 주세요.".into(),
            ));
        }

        // An ordinary destination row, which is what keeps the sending path
        // identical to the one that is on air today.
        let destination = self.db.upsert_youtube_destination(
            user_id,
            broadcast_id,
            &stream.ingestion_address,
            account_id,
            &stream.id,
        )?;
        self.seal(&crate::credentials::destination_account(&destination.id), &stream.stream_name)?;
        self.db.attach_youtube(broadcast_id, account_id, &broadcast.id, &stream.id, WAITING)?;
        self.db.point_broadcast_at(user_id, broadcast_id, &destination.id)?;
        Ok(())
    }

    /// A new YouTube broadcast for a broadcast whose last one has ended.
    ///
    /// The point of doing this rather than provisioning from scratch: a
    /// `liveStream` is reusable and a `liveBroadcast` is not. Keeping the stream
    /// means the ingestion address and the stream key stay exactly as they were,
    /// so the `stream_destinations` row, the sealed key and every line of the
    /// sending path are untouched — a recurring schedule gets a new YouTube
    /// broadcast without the user's connection being rebuilt underneath them.
    ///
    /// Only when there is no usable stream does this fall back to full
    /// provisioning, which does replace the key.
    fn renew(&self, b: &crate::models::Broadcast, account_id: &str) -> Result<()> {
        let meta = metadata_of(b);
        let start = scheduled_start_for(b);
        let end = scheduled_end_for(b, &start);
        let fresh = self.call_api(&b.id, account_id, ApiMethod::LiveBroadcastsInsert, |api, token| {
            api.create_broadcast(token, &meta, &start, end.as_deref(), true)
        })?;

        if let Some(stream_id) = b.youtube.stream_id.clone() {
            let bound = self.call_api(&b.id, account_id, ApiMethod::LiveBroadcastsBind, |api, token| {
                api.bind_broadcast(token, &fresh.id, &stream_id)
            })?;
            if bound.bound_stream_id.as_deref() == Some(stream_id.as_str()) {
                self.db.attach_youtube(&b.id, account_id, &fresh.id, &stream_id, WAITING)?;
                return Ok(());
            }
        }
        // No stream to reuse: make one, which also rewrites the destination and
        // its key. Correct, and the more disruptive of the two paths, which is
        // why it is the fallback.
        self.provision(&b.user_id, &b.id, account_id)
    }

    /// Bring YouTube's side in line with what 247streams says. §10.
    pub fn sync_metadata(&self, user_id: &str, broadcast_id: &str) -> Result<()> {
        let b = self.db.broadcast_owned(user_id, broadcast_id)?;
        let (Some(account_id), Some(yt_id)) = (b.youtube.account_id.clone(), b.youtube.broadcast_id.clone())
        else {
            return Ok(()); // nothing connected; the caller does not have to care
        };
        let meta = metadata_of(&b);
        let start = scheduled_start_for(&b);
        let end = scheduled_end_for(&b, &start);
        self.call_api(broadcast_id, &account_id, ApiMethod::LiveBroadcastsUpdate, |api, token| {
            api.update_broadcast(token, &yt_id, &meta, Some(&start), end.as_deref())
        })
    }

    // --- start and stop (§8, §9) ------------------------------------------

    /// Everything that has to be true before FFmpeg is allowed to start.
    ///
    /// A broadcast YouTube has already completed is not started again — §14's
    /// rule against restarting for ever.
    pub fn before_start(&self, broadcast_id: &str) -> Result<()> {
        let b = self.db.broadcast(broadcast_id)?;
        let (Some(account_id), Some(yt_id)) = (b.youtube.account_id.clone(), b.youtube.broadcast_id.clone())
        else {
            return Ok(());
        };
        let broadcast =
            self.call_api(broadcast_id, &account_id, ApiMethod::LiveBroadcastsList, |api, token| {
                api.broadcast_by_id(token, &yt_id)
            })?;

        if broadcast.life_cycle_status == "complete" {
            // A completed YouTube broadcast can never go live again — and this
            // is the *ordinary* state of a broadcast that ran yesterday, or one
            // whose window ended this morning. Refusing here is what used to
            // make a daily schedule work exactly once.
            //
            // So the record is marked complete (it is), and a fresh YouTube
            // broadcast is made for this occurrence. `renew` keeps the existing
            // ingestion stream, so the destination row and its sealed key — the
            // things FFmpeg actually sends to — do not change.
            self.db.set_youtube_status(broadcast_id, COMPLETE)?;
            self.note(broadcast_id, "이전 YouTube 방송이 종료되어 새 방송을 만듭니다");
            self.renew(&b, &account_id)?;
            return Ok(());
        }
        // Recovery's case: the resources are still there but the binding is
        // not, which YouTube answers with a broadcast that has no stream.
        if broadcast.bound_stream_id.is_none() {
            if let Some(stream_id) = b.youtube.stream_id.clone() {
                self.call_api(broadcast_id, &account_id, ApiMethod::LiveBroadcastsBind, |api, token| {
                    api.bind_broadcast(token, &yt_id, &stream_id)
                })?;
            }
        }
        // A broadcast made before the monitor stream was disabled at insert
        // will park in `testing` the moment FFmpeg connects, and no amount of
        // waiting changes that. Said here, at the start, rather than discovered
        // five minutes later — and said without stopping the start, because the
        // video does reach YouTube and the customer can finish it in Studio.
        if !broadcast.starts_itself() {
            let why =
                if broadcast.enable_monitor_stream { Stuck::MonitorStream } else { Stuck::AutoStartOff };
            self.note(broadcast_id, why.explain());
        }

        self.db.set_youtube_status(broadcast_id, WAITING)?;
        Ok(())
    }

    /// Look at a broadcast on YouTube and write nothing anywhere. §3 of the
    /// P0 follow-up: the diagnosis half, which has to be usable against a live
    /// production database.
    ///
    /// Two `list` calls and no more. It does not touch `broadcasts`, does not
    /// append an event, does not transition, and does not store a token — the
    /// difference between this and [`Self::poll_status`], which does the first
    /// three because it is the running server's own view.
    ///
    /// Never returns `Err`: a failure to ask is itself one of the things the
    /// audit reports, so it comes back as a [`crate::audit::Seen`] with the
    /// reach that failed. The caller gets a verdict either way, which is what
    /// stops a broken question from reading as a healthy broadcast.
    pub fn observe(&self, broadcast_id: &str) -> crate::audit::Seen {
        use crate::audit::{Reach, Seen};

        let Ok(b) = self.db.broadcast(broadcast_id) else {
            return Seen {
                reach: Reach::Unreachable,
                error: Some("방송 행을 읽을 수 없습니다".into()),
                ..Seen::default()
            };
        };
        let (Some(account_id), Some(yt_id)) = (b.youtube.account_id.clone(), b.youtube.broadcast_id.clone())
        else {
            return Seen {
                reach: Reach::Unreachable,
                error: Some("YouTube 계정에 연결된 방송이 아닙니다".into()),
                ..Seen::default()
            };
        };

        let api = self.api();
        let token = match self.access_token(&account_id) {
            Ok(t) => t,
            // A token that cannot be obtained is an auth problem by
            // construction, whichever step of the dance refused.
            Err(e) => return Seen { reach: Reach::Refused, error: Some(e.to_string()), ..Seen::default() },
        };

        let broadcast = match api.broadcast_by_id(&token, &yt_id) {
            Ok(v) => v,
            Err(e) => return Seen { error: Some(e.message.clone()), ..reach_of(&e) },
        };

        let mut seen = Seen {
            reach: Reach::Answered,
            life_cycle_status: broadcast.life_cycle_status.clone(),
            bound_stream_id: broadcast.bound_stream_id.clone(),
            enable_auto_start: broadcast.enable_auto_start,
            enable_monitor_stream: broadcast.enable_monitor_stream,
            ..Seen::default()
        };

        // The stream is a second question, and a broadcast with no stream to
        // ask about is still worth a verdict — `classify` has one for it.
        if let Some(stream_id) = b.youtube.stream_id.as_deref() {
            match api.stream_by_id(&token, stream_id) {
                Ok(stream) => {
                    seen.stream_status = stream.stream_status.clone();
                    seen.health_status = stream.health_status.clone();
                    seen.configuration_issues = stream.configuration_issues.clone();
                }
                Err(e) => {
                    // The broadcast answered and the stream did not. Say so
                    // without throwing away the half that worked.
                    seen.error = Some(format!("스트림 조회 실패: {}", e.message));
                }
            }
        }
        seen
    }

    /// Ask YouTube what it makes of the stream, once. §8: bounded, and called
    /// on a tick rather than in a loop of its own.
    ///
    /// Reports what YouTube said and what still has to happen, and sends a
    /// transition only when YouTube's own state makes one both necessary and
    /// legal — and only when the operator has turned that on. FFmpeg being
    /// healthy is not evidence about any of it.
    pub fn poll_status(&self, broadcast_id: &str) -> Result<Option<Observed>> {
        let b = self.db.broadcast(broadcast_id)?;
        let (Some(account_id), Some(yt_id), Some(stream_id)) =
            (b.youtube.account_id.clone(), b.youtube.broadcast_id.clone(), b.youtube.stream_id.clone())
        else {
            return Ok(None);
        };
        let stream = self.call_api(broadcast_id, &account_id, ApiMethod::LiveStreamsList, |api, token| {
            api.stream_by_id(token, &stream_id)
        })?;
        let broadcast =
            self.call_api(broadcast_id, &account_id, ApiMethod::LiveBroadcastsList, |api, token| {
                api.broadcast_by_id(token, &yt_id)
            })?;

        // What YouTube is actually bound to. A broadcast whose stream is not the
        // one being sent to never goes live, and before this the only symptom
        // was silence.
        if let Some(bound) = broadcast.bound_stream_id.as_deref() {
            if bound != stream_id {
                self.note(
                    broadcast_id,
                    "YouTube 방송이 다른 스트림에 연결되어 있습니다. 송출 중인 스트림과 공개 방송이 서로 다른 대상입니다.",
                );
            }
        }

        let active = stream.is_active();
        let status = status_for(&broadcast.life_cycle_status, active);
        self.db.set_youtube_status(broadcast_id, status)?;
        let step = next_step(
            &broadcast.life_cycle_status,
            active,
            broadcast.starts_itself(),
            broadcast.enable_monitor_stream,
        );

        // An ingest YouTube calls active but cannot use looks exactly like one
        // that is fine, from here. Its own words, because "bad" and "noData"
        // need different answers.
        if active
            && matches!(stream.health_status.as_str(), "bad" | "noData")
            && !stream.configuration_issues.is_empty()
        {
            self.note(
                broadcast_id,
                &format!(
                    "YouTube가 수신 품질을 {}로 보고했습니다: {}",
                    stream.health_status,
                    stream.configuration_issues.join(", ")
                ),
            );
        }

        // Detected either way. Acted on only when the operator allows it; the
        // caller is what decides when to say it out loud, so that a poll every
        // ten seconds does not become a log entry every ten seconds.
        let mut transitioned = false;
        if matches!(step, NextStep::NeedsTransition(_)) && self.auto_transition {
            match self.call_api(
                broadcast_id,
                &account_id,
                ApiMethod::LiveBroadcastsTransition,
                |api, token| api.transition_broadcast(token, &yt_id, "live"),
            ) {
                Ok(_) => {
                    self.db.set_youtube_status(broadcast_id, LIVE)?;
                    self.note(broadcast_id, "YouTube 방송을 실시간으로 전환했습니다");
                    transitioned = true;
                }
                // Not fatal, and deliberately not a reason to stop sending: the
                // encoder is fine and the customer's video is still arriving at
                // YouTube.
                Err(e) => self.note(broadcast_id, &format!("YouTube 라이브 전환 실패: {e}")),
            }
        }

        Ok(Some(Observed { status: if transitioned { LIVE } else { status }, step, transitioned }))
    }

    /// End the YouTube side of a broadcast the user stopped. §9.
    ///
    /// Tolerant on purpose: `enableAutoStop` may already have completed it, and
    /// a failure here must not leave 247streams thinking the broadcast is still
    /// running.
    pub fn after_stop(&self, broadcast_id: &str) {
        let Ok(b) = self.db.broadcast(broadcast_id) else { return };
        let (Some(account_id), Some(yt_id)) = (b.youtube.account_id.clone(), b.youtube.broadcast_id.clone())
        else {
            return;
        };
        let live = self
            .call_api(broadcast_id, &account_id, ApiMethod::LiveBroadcastsList, |api, token| {
                api.broadcast_by_id(token, &yt_id)
            })
            .map(|x| x.is_live())
            .unwrap_or(false);
        if live {
            if let Err(e) =
                self.call_api(broadcast_id, &account_id, ApiMethod::LiveBroadcastsTransition, |api, token| {
                    api.transition_broadcast(token, &yt_id, "complete")
                })
            {
                self.note(broadcast_id, &format!("YouTube 종료 처리 실패: {e}"));
            }
        }
        let _ = self.db.set_youtube_status(broadcast_id, COMPLETE);
    }

    /// Record a failure where the user will see it, in words they can act on,
    /// and never with a token in them.
    ///
    /// §4: the HTTP status and Google's own `reason` are kept, because "권한이
    /// 없습니다" and "이 채널에 실시간 스트리밍이 켜져 있지 않습니다" are the same
    /// 403 and the same generic message, and only the reason tells them apart.
    /// The detail is built by `ApiFailure::describe`, which carries the method,
    /// the status, the reason and Google's message — and no credential of any
    /// kind, which is what makes it safe to show and to log.
    fn explain(
        &self,
        broadcast_id: &str,
        method: ApiMethod,
        e: louver_core::error::LouverError,
    ) -> CloudError {
        let message = match &e.detail {
            Some(detail) if detail != &e.message => format!("{} ({detail})", e.message),
            _ => e.message.clone(),
        };
        self.note(broadcast_id, &format!("{} 실패: {message}", method.name()));
        let _ = self.db.set_youtube_error(broadcast_id, &message);
        CloudError::Engine(message)
    }

    fn note(&self, broadcast_id: &str, line: &str) {
        crate::manager::say(broadcast_id, "youtube", line);
        let _ = self.db.append_event(broadcast_id, louver_core::database::models::EventLevel::Warn, line);
    }
}

/// Forget a connected account, without needing a configured Google client.
///
/// Deliberately a free function: disconnecting has to work on a server whose
/// `YOUTUBE_CLIENT_ID` was removed, because otherwise taking the credentials
/// away would strand every account that had been connected with them. Nothing
/// here talks to Google — revoking our own copy of the grant is all that is
/// being asked for, and the user can revoke the rest in their Google account.
pub fn forget_account(
    db: &CloudDb,
    keys: &Arc<dyn SecretStore>,
    user_id: &str,
    account_id: &str,
) -> Result<()> {
    // The owner check first, so an id belonging to someone else is simply not
    // found rather than having its tokens deleted.
    db.youtube_account_owned(user_id, account_id)?;
    let _ = keys.delete(&refresh_account(account_id));
    let _ = keys.delete(&access_account(account_id));
    db.delete_youtube_account(user_id, account_id)
}

/// 247streams' own broadcast information, in the shape the API client wants.
fn metadata_of(b: &crate::models::Broadcast) -> BroadcastMetadata {
    BroadcastMetadata {
        title: if b.title.trim().is_empty() { b.name.clone() } else { b.title.clone() },
        description: b.description.clone(),
        tags: b.tags.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect(),
        category_id: String::new(),
        privacy: match b.privacy {
            crate::models::Privacy::Public => YtPrivacy::Public,
            crate::models::Privacy::Unlisted => YtPrivacy::Unlisted,
            crate::models::Privacy::Private => YtPrivacy::Private,
        },
    }
}

/// The `scheduledEndTime` that belongs with the start we are actually sending.
///
/// A schedule stores two instants, and the stop is only ever meaningful as a
/// *length*: "every day at 21:18, for three hours". `schedule::decide` has
/// always read it that way — it stops a broadcast at
/// `occurrence + (stop_at - start_at)` — but what went to YouTube was the
/// stored `stop_at` itself, unrolled, while the start beside it was rolled
/// forward by [`scheduled_start_for`].
///
/// That asymmetry is the bug this function exists to remove. From the second
/// occurrence onwards the start was today and the end was the day the schedule
/// was first saved, so `liveBroadcasts.insert` answered
/// `400 invalidScheduledEndTime` and the broadcast did not start — for ever,
/// once a day, on a service whose customers run broadcasts for months.
///
/// `None` stays `None`: a schedule with no stop is an indefinite broadcast and
/// must not acquire an end here.
fn scheduled_end_for(b: &crate::models::Broadcast, start: &str) -> Option<String> {
    if !b.schedule.enabled {
        return None;
    }
    let parse = |s: &str| chrono::DateTime::parse_from_rfc3339(s).ok().map(|t| t.with_timezone(&chrono::Utc));
    let window = parse(b.schedule.stop_at.as_deref()?)? - parse(b.schedule.start_at.as_deref()?)?;
    // `schedule::validate` refuses a stop that is not after the start, so this
    // is a belt rather than a case: a zero or negative window would be the same
    // 400 by another route, and an end YouTube rejects is worse than no end.
    if window <= chrono::Duration::zero() {
        return None;
    }
    Some((parse(start)? + window).to_rfc3339())
}

/// A `scheduledStartTime` YouTube will accept. §5: required even for "start now".
///
/// Always in the future. A repeating schedule keeps its *original* start instant
/// for ever — that is what "every weekday at 21:00" is stored as — so using it
/// verbatim would hand YouTube a start time from last month on every occurrence
/// after the first.
fn scheduled_start_for(b: &crate::models::Broadcast) -> String {
    let soon = chrono::Utc::now() + chrono::Duration::minutes(1);
    let stored = b
        .schedule
        .start_at
        .as_deref()
        .filter(|_| b.schedule.enabled)
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&chrono::Utc));
    match stored {
        Some(t) if t > soon => t.to_rfc3339(),
        _ => soon.to_rfc3339(),
    }
}

/// Which kind of "could not ask" a core error is, for the audit.
///
/// The codes come from one `match` in the core
/// (`crates/louver-core/src/youtube/api.rs`), so this is a translation rather
/// than a second opinion: a refused credential is its own verdict, a 404 means
/// the stored broadcast is not on the channel, and everything else — a 5xx, a
/// timeout, an exhausted quota — is simply unanswered.
fn reach_of(e: &louver_core::error::LouverError) -> crate::audit::Seen {
    use crate::audit::{Reach, Seen};
    use louver_core::error::ErrorCode as C;
    match e.code {
        C::YoutubeAuthExpired | C::YoutubeNotConnected | C::YoutubeAuthRefreshFailed => {
            Seen { reach: Reach::Refused, ..Seen::default() }
        }
        // 404: the id 247streams holds is not a broadcast on this channel.
        C::YoutubeNoActiveBroadcast => Seen { reach: Reach::Answered, not_found: true, ..Seen::default() },
        _ => Seen { reach: Reach::Unreachable, ..Seen::default() },
    }
}
