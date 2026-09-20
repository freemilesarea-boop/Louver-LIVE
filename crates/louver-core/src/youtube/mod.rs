//! YouTube account, broadcast metadata and live chat (V2).
//!
//! Strictly separate from the broadcast engine. Video reaches YouTube over
//! RTMPS with a stream key exactly as before; nothing in this module can start,
//! stop or disturb FFmpeg. What it adds is the part of a broadcast that
//! otherwise means opening YouTube Studio: the title, description, tags,
//! category, privacy — and an automatic chat message every so often.

pub mod api;
pub mod bot;
pub mod chat;
pub mod http;
pub mod metadata;
pub mod oauth;
pub mod provision;
pub mod quota;

pub use api::{ChannelInfo, HttpClient, LiveBroadcast, LiveStream, YoutubeApi};
pub use chat::{ChatMessage, ChatOrder, ChatSettings, ChatState, ChatStatus, MIN_INTERVAL_SECS};
pub use metadata::{BroadcastMetadata, Privacy, CATEGORIES};
pub use oauth::{ClientCredentials, TokenStore};
pub use quota::{ApiMethod, MeteredClient, QuotaGuard, QuotaState, FREE_DAILY_UNITS};

/// Settings keys this module owns.
pub mod keys {
    pub const CLIENT_ID: &str = "youtube_client_id";
    pub const CHANNEL_ID: &str = "youtube_channel_id";
    pub const CHANNEL_TITLE: &str = "youtube_channel_title";
    pub const METADATA_TITLE: &str = "youtube_meta_title";
    pub const METADATA_DESCRIPTION: &str = "youtube_meta_description";
    pub const METADATA_TAGS: &str = "youtube_meta_tags";
    pub const METADATA_CATEGORY: &str = "youtube_meta_category";
    pub const METADATA_PRIVACY: &str = "youtube_meta_privacy";
    pub const APPLY_ON_START: &str = "youtube_apply_on_start";
    /// What a *scheduled* start does when the metadata cannot be applied:
    /// `hold` (default — do not broadcast under YouTube's own settings) or
    /// `broadcast` (keep the channel on air and record the failure).
    pub const SCHEDULE_ON_METADATA_FAILURE: &str = "youtube_schedule_on_metadata_failure";
    pub const CHAT_ENABLED: &str = "youtube_chat_enabled";
    pub const CHAT_ORDER: &str = "youtube_chat_order";
    pub const CHAT_INTERVAL: &str = "youtube_chat_interval";
    pub const CHAT_ON_START: &str = "youtube_chat_on_start";
    pub const CHAT_ON_END: &str = "youtube_chat_on_end";
    pub const CHAT_AVOID_REPEATS: &str = "youtube_chat_avoid_repeats";
    /// Overridden by the test harness to point at a local fake.
    pub const API_BASE: &str = "youtube_api_base";
    /// Send `client_secret` in the token exchange after all.
    ///
    /// Off by default: the exchange is attempted with PKCE alone. Turned on
    /// only with evidence that Google refused the secret-less exchange for
    /// this client, which the app records verbatim when it happens.
    /// The last token-endpoint failure, kept so it can be reported.
    pub const LAST_AUTH_DIAGNOSTIC: &str = "youtube_last_auth_diagnostic";
    /// The day's API spending, so a restart does not start the count over.
    pub const QUOTA_STATE: &str = "youtube_quota_state";
}

/// A saved set of broadcast metadata (§4).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BroadcastPreset {
    pub id: i64,
    pub name: String,
    #[serde(flatten)]
    pub metadata: BroadcastMetadata,
}
