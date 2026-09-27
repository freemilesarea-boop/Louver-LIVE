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
    pub watch_url: Option<String>,
    pub last_error: Option<String>,
}

/// YouTube's own view of a broadcast, kept apart from FFmpeg's. §8.
pub const WAITING: &str = "waiting_for_ingest";
pub const READY: &str = "ready";
pub const LIVE: &str = "live";
pub const COMPLETE: &str = "complete";
pub const ERRORED: &str = "error";

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
        Self { db, keys, http, tokens, config }
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

        let tokens = self.tokens.refresh(creds, &refresh).map_err(|e| CloudError::Engine(e.message))?;
        // §2: Google does not return a refresh token on every refresh, and
        // writing the absent one would disconnect the account for good.
        if let Some(new_refresh) = tokens.refresh_token.as_deref() {
            if !new_refresh.trim().is_empty() && new_refresh != refresh {
                self.seal(&refresh_account(account_id), new_refresh)?;
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
        let end = b.schedule.stop_at.clone().filter(|_| b.schedule.enabled);

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

    /// Bring YouTube's side in line with what 247streams says. §10.
    pub fn sync_metadata(&self, user_id: &str, broadcast_id: &str) -> Result<()> {
        let b = self.db.broadcast_owned(user_id, broadcast_id)?;
        let (Some(account_id), Some(yt_id)) = (b.youtube.account_id.clone(), b.youtube.broadcast_id.clone())
        else {
            return Ok(()); // nothing connected; the caller does not have to care
        };
        let meta = metadata_of(&b);
        let start = scheduled_start_for(&b);
        let end = b.schedule.stop_at.clone().filter(|_| b.schedule.enabled);
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
            self.db.set_youtube_status(broadcast_id, COMPLETE)?;
            return Err(CloudError::Invalid(
                "이 YouTube 방송은 이미 종료되었습니다. 새 방송을 만들어 주세요.".into(),
            ));
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
        self.db.set_youtube_status(broadcast_id, WAITING)?;
        Ok(())
    }

    /// Ask YouTube what it makes of the stream, once. §8: bounded, and called
    /// on a tick rather than in a loop of its own.
    pub fn poll_status(&self, broadcast_id: &str) -> Result<Option<String>> {
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

        let status = match broadcast.life_cycle_status.as_str() {
            "live" => LIVE,
            "complete" => COMPLETE,
            _ if stream.is_active() => READY,
            _ => WAITING,
        };
        self.db.set_youtube_status(broadcast_id, status)?;

        // A broadcast that will not start itself has to be told to. Only once
        // the stream is actually active, because YouTube refuses otherwise and
        // the refusal reads like a permissions problem.
        if status == READY && !broadcast.enable_auto_start {
            match self.call_api(
                broadcast_id,
                &account_id,
                ApiMethod::LiveBroadcastsTransition,
                |api, token| api.transition_broadcast(token, &yt_id, "live"),
            ) {
                Ok(_) => {
                    self.db.set_youtube_status(broadcast_id, LIVE)?;
                    return Ok(Some(LIVE.to_string()));
                }
                Err(e) => self.note(broadcast_id, &format!("YouTube 라이브 전환 실패: {e}")),
            }
        }
        Ok(Some(status.to_string()))
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

/// A `scheduledStartTime` YouTube will accept. §5: required even for "start now".
fn scheduled_start_for(b: &crate::models::Broadcast) -> String {
    b.schedule
        .start_at
        .clone()
        .filter(|_| b.schedule.enabled)
        .unwrap_or_else(|| (chrono::Utc::now() + chrono::Duration::minutes(1)).to_rfc3339())
}
