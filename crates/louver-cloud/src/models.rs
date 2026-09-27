//! Rows the cloud owns, and the states a broadcast moves through.

use serde::{Deserialize, Serialize};

/// What the operator wants a broadcast to be doing.
///
/// Kept apart from [`RuntimeState`] because they answer different questions and
/// routinely disagree: a broadcast whose FFmpeg has just died is
/// `desired = Running, runtime = Reconnecting`. Recovery after a server restart
/// reads this column and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesiredState {
    /// Created, or deliberately stopped. The watchdog must leave it alone.
    Stopped,
    Running,
}

impl DesiredState {
    pub fn id(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Running => "running",
        }
    }
    pub fn from_id(s: &str) -> Option<Self> {
        Some(match s {
            "stopped" => Self::Stopped,
            "running" => Self::Running,
            _ => return None,
        })
    }
}

/// What a broadcast is actually doing, as the engine reports it.
///
/// These are [`louver_core::streaming::state::StreamState`]'s eight states in
/// the names the cloud API uses. The engine's machine is not duplicated — this
/// is a projection of it, and [`RuntimeState::from_engine`] is the only place
/// the two vocabularies meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RuntimeState {
    Created,
    Preparing,
    Starting,
    Running,
    Reconnecting,
    Stopping,
    Stopped,
    Failed,
}

impl RuntimeState {
    pub fn id(self) -> &'static str {
        match self {
            Self::Created => "CREATED",
            Self::Preparing => "PREPARING",
            Self::Starting => "STARTING",
            Self::Running => "RUNNING",
            Self::Reconnecting => "RECONNECTING",
            Self::Stopping => "STOPPING",
            Self::Stopped => "STOPPED",
            Self::Failed => "FAILED",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        Some(match s {
            "CREATED" => Self::Created,
            "PREPARING" => Self::Preparing,
            "STARTING" => Self::Starting,
            "RUNNING" => Self::Running,
            "RECONNECTING" => Self::Reconnecting,
            "STOPPING" => Self::Stopping,
            "STOPPED" => Self::Stopped,
            "FAILED" => Self::Failed,
            _ => return None,
        })
    }

    pub fn from_engine(s: louver_core::streaming::state::StreamState) -> Self {
        use louver_core::streaming::state::StreamState as E;
        match s {
            E::Idle => Self::Created,
            E::Preparing => Self::Preparing,
            E::Connecting => Self::Starting,
            E::Live => Self::Running,
            E::Reconnecting => Self::Reconnecting,
            E::Stopping => Self::Stopping,
            E::Stopped => Self::Stopped,
            E::Error => Self::Failed,
        }
    }

    /// Does this count against `max_concurrent_streams`?
    ///
    /// Reconnecting counts: the process is coming back and the slot is taken.
    /// Counting only `Running` would let a user start a fourth broadcast during
    /// a three-second blip.
    pub fn occupies_a_slot(self) -> bool {
        matches!(self, Self::Preparing | Self::Starting | Self::Running | Self::Reconnecting)
    }
}

/// Whether an uploaded file can be broadcast yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaState {
    /// Bytes are stored; nothing has looked inside yet.
    Uploaded,
    Analysing,
    /// Being remuxed or encoded into the broadcast profile.
    Preparing,
    Ready,
    Failed,
}

impl MediaState {
    pub fn id(self) -> &'static str {
        match self {
            Self::Uploaded => "uploaded",
            Self::Analysing => "analysing",
            Self::Preparing => "preparing",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }
    pub fn from_id(s: &str) -> Option<Self> {
        Some(match s {
            "uploaded" => Self::Uploaded,
            "analysing" => Self::Analysing,
            "preparing" => Self::Preparing,
            "ready" => Self::Ready,
            "failed" => Self::Failed,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub email: String,
    pub plan_id: String,
    pub created_at: String,
    /// What this person is called, as they typed it. A display name and nothing
    /// more — sign-in is by email, so this is never an identifier and never
    /// unique. `None` on every account made before signup asked for one.
    pub name: Option<String>,
    /// When the terms and the privacy policy were agreed to, written by the
    /// server's clock. `None` for an account that predates the checkbox, and for
    /// one the bootstrap CLI made: a shell script cannot agree on a person's
    /// behalf.
    pub terms_accepted_at: Option<String>,
    pub privacy_accepted_at: Option<String>,
}

/// A plan is a bag of named limits, never a name the code branches on.
///
/// `entitlement::limit()` looks limits up by key so that adding a plan is a row
/// and never an `if`. §3 asks for this explicitly and it is also what makes a
/// per-customer override possible later without touching any call site.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub label: String,
    pub limits: std::collections::BTreeMap<String, i64>,
    /// Whole won. Money is never a float: ₩19,900 is `19900`, and there is no
    /// arithmetic below the won in Korea anyway.
    pub monthly_price_krw: i64,
    /// Who the plan is for, in one line, for the pricing page.
    pub description: String,
    /// Is this plan offered? The unsubscribed plan and any internal one are not,
    /// and `GET /api/plans` filters on this rather than on a list of names.
    pub active: bool,
    pub sort_order: i64,
}

impl Plan {
    /// How many broadcasts this plan may run at once. The entitlement that
    /// distinguishes the three paid plans.
    pub fn max_concurrent_streams(&self) -> i64 {
        self.limits.get(crate::entitlement::MAX_CONCURRENT_STREAMS).copied().unwrap_or(0)
    }

    /// Can somebody on this plan broadcast at all?
    ///
    /// Asked of the plan rather than of its name, so the unsubscribed plan is
    /// simply a plan whose answer is no.
    pub fn can_broadcast(&self) -> bool {
        self.max_concurrent_streams() > 0
    }
}

/// What a user is entitled to, and why.
///
/// `status` and `plan` are reported separately because they fail separately: a
/// lapsed card leaves the plan in place and the status not active, and the
/// difference is what the dashboard has to explain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub user_id: String,
    /// Kept for the clients written before `plan` existed. For an unsubscribed
    /// account this is the unsubscribed plan's id, not an empty string.
    pub plan_id: String,
    pub plan_label: String,
    /// `active` or `unsubscribed`.
    pub status: String,
    pub limits: std::collections::BTreeMap<String, i64>,
    /// The single question every caller actually asks. True only when the
    /// status is active **and** the plan grants something — so an account on the
    /// unsubscribed plan cannot become entitled by a status column alone.
    pub active: bool,
    /// The plan being paid for. `None` when there is no subscription, which is
    /// what lets a client tell "no plan" from "Basic".
    pub plan: Option<Plan>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudMedia {
    pub id: String,
    pub user_id: String,
    pub filename: String,
    pub size_bytes: i64,
    pub state: MediaState,
    pub duration_secs: f64,
    pub width: i64,
    pub height: i64,
    pub fps: f64,
    pub video_codec: String,
    pub audio_codec: Option<String>,
    pub container: String,
    pub bitrate_bps: i64,
    /// Where the original sits in the storage backend.
    pub storage_path: String,
    /// Where the prepared, broadcastable copy sits. `None` until it exists.
    pub prepared_path: Option<String>,
    pub prepared_duration_secs: Option<f64>,
    pub last_error: Option<String>,
    pub created_at: String,
}

/// A YouTube ingest target. The key itself is never in this struct.
///
/// It lives in the [`crate::credentials::CredentialStore`] under the account
/// name `destination:<id>`, sealed. What travels to a browser is `key_masked`,
/// which is why there is no `key` field to forget to strip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamDestination {
    pub id: String,
    pub user_id: String,
    pub label: String,
    pub rtmps_url: String,
    pub key_masked: String,
    pub created_at: String,
    /// How this destination is driven. Every row written so far is a stream key
    /// somebody pasted, which can send video and can change nothing else about
    /// the broadcast on the platform. §5.
    pub kind: DestinationKind,
}

/// Who may watch, on the destination that is eventually connected.
///
/// Stored and shown by 247streams. It is **not** applied to YouTube: a manual
/// RTMPS key cannot set privacy, and pretending otherwise would be the one
/// mistake here a user could not recover from. See `DestinationKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Privacy {
    Public,
    Unlisted,
    Private,
}

impl Privacy {
    pub fn id(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Unlisted => "unlisted",
            Self::Private => "private",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "public" => Some(Self::Public),
            "unlisted" => Some(Self::Unlisted),
            "private" => Some(Self::Private),
            _ => None,
        }
    }
}

/// How a destination is driven.
///
/// The distinction is §5's: a YouTube-connected destination can set a title and
/// a privacy, and a pasted key cannot, so every feature that depends on that
/// asks which it is rather than assuming. Both kinds send video through exactly
/// the same worker — the difference is only in what else can be done.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DestinationKind {
    /// A stream key pasted from YouTube Studio. Sends video; changes nothing
    /// about the broadcast on YouTube's side.
    ManualRtmps,
    /// An account connected through OAuth. 247streams creates the live
    /// broadcast itself, titles it, sets its privacy and asks YouTube for an
    /// ingestion address of its own, so the user pastes nothing. The row is
    /// written by [`crate::youtube::Youtube::provision`].
    YoutubeAccount,
}

impl DestinationKind {
    pub fn id(self) -> &'static str {
        match self {
            Self::ManualRtmps => "manual_rtmps",
            Self::YoutubeAccount => "youtube_account",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "manual_rtmps" => Some(Self::ManualRtmps),
            "youtube_account" => Some(Self::YoutubeAccount),
            _ => None,
        }
    }

    /// Can this destination carry a title, description or privacy to the
    /// platform? Only an account can.
    pub fn can_publish_metadata(self) -> bool {
        matches!(self, Self::YoutubeAccount)
    }
}

/// What the sender is asked to produce. §6.
///
/// `Auto` everywhere is the default and the only combination a user has to
/// understand: the server sends the prepared file as it is, which is the
/// cheapest and most stable thing it can do. The explicit values are stored and
/// validated so that a broadcast keeps them, and are applied by the encoder path
/// rather than the copy path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamSettings {
    /// `auto`, `720p` or `1080p`.
    pub resolution: String,
    /// `auto`, `30` or `60`.
    pub fps: String,
    /// 0 means auto.
    pub video_bitrate_kbps: i64,
    /// 0 means auto.
    pub audio_bitrate_kbps: i64,
}

impl Default for StreamSettings {
    fn default() -> Self {
        Self { resolution: "auto".into(), fps: "auto".into(), video_bitrate_kbps: 0, audio_bitrate_kbps: 0 }
    }
}

impl StreamSettings {
    pub fn is_auto(&self) -> bool {
        self.resolution == "auto"
            && self.fps == "auto"
            && self.video_bitrate_kbps == 0
            && self.audio_bitrate_kbps == 0
    }

    /// Refuse a combination that would produce a stream YouTube drops.
    ///
    /// The ranges are YouTube's own recommendations, widened enough not to argue
    /// with someone who knows what they are doing.
    pub fn validate(&self) -> crate::Result<()> {
        use crate::CloudError::Invalid;
        if !["auto", "720p", "1080p"].contains(&self.resolution.as_str()) {
            return Err(Invalid("해상도는 auto, 720p, 1080p 중 하나여야 합니다".into()));
        }
        if !["auto", "30", "60"].contains(&self.fps.as_str()) {
            return Err(Invalid("프레임레이트는 auto, 30, 60 중 하나여야 합니다".into()));
        }
        if self.video_bitrate_kbps != 0 && !(1_000..=51_000).contains(&self.video_bitrate_kbps) {
            return Err(Invalid("영상 비트레이트는 1000~51000 kbps 범위여야 합니다".into()));
        }
        if self.audio_bitrate_kbps != 0 && !(64..=512).contains(&self.audio_bitrate_kbps) {
            return Err(Invalid("음성 비트레이트는 64~512 kbps 범위여야 합니다".into()));
        }
        // 1080p60 at a bitrate meant for 720p30 looks worse than either. The
        // floor is what YouTube itself asks for at that size.
        let floor = match (self.resolution.as_str(), self.fps.as_str()) {
            ("1080p", "60") => 4_500,
            ("1080p", _) => 3_000,
            ("720p", "60") => 2_250,
            ("720p", _) => 1_500,
            _ => 0,
        };
        if self.video_bitrate_kbps != 0 && floor != 0 && self.video_bitrate_kbps < floor {
            return Err(Invalid(format!(
                "{} {}fps 에는 최소 {floor} kbps 가 필요합니다",
                self.resolution, self.fps
            )));
        }
        Ok(())
    }
}

/// When a broadcast should start by itself. §8.
///
/// Times are stored as UTC and only ever rendered in the user's zone, because a
/// server that moves between regions must not move a broadcast with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Schedule {
    pub enabled: bool,
    /// RFC 3339, UTC. The first (or next) start.
    pub start_at: Option<String>,
    /// RFC 3339, UTC. Optional automatic stop.
    pub stop_at: Option<String>,
    /// IANA zone the user chose. Shown to them, and never used for arithmetic.
    pub timezone: String,
    /// Minutes east of UTC when the schedule was set. Which local day an
    /// instant falls on is decided with this, so that a daily repeat means the
    /// same clock time to the user without this service carrying a timezone
    /// database. A DST change moves a repeat by an hour until it is saved again.
    #[serde(default)]
    pub offset_minutes: i64,
    /// Bitmask, Monday = bit 0. `0` means "once, at `start_at`".
    pub repeat_days: i64,
    /// The last occurrence this schedule actually started, so one window is
    /// never started twice — including after a restart.
    pub last_run_at: Option<String>,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            enabled: false,
            start_at: None,
            stop_at: None,
            timezone: "UTC".into(),
            offset_minutes: 0,
            repeat_days: 0,
            last_run_at: None,
        }
    }
}

/// One video in a broadcast's playlist. §1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BroadcastItem {
    pub id: String,
    pub broadcast_id: String,
    pub media_id: String,
    /// 0-based, and dense: reordering rewrites every row's position.
    pub position: i64,
    pub enabled: bool,
    /// How many times in a row this item plays. 1 is once.
    pub repeat_count: i64,
    // --- joined from `media`, for a UI that must not fetch twice -----------
    pub filename: String,
    pub duration_secs: f64,
    pub state: MediaState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Broadcast {
    pub id: String,
    pub user_id: String,
    pub name: String,
    /// The first item's media, kept so that every reader written before
    /// playlists — including a rollback to the previous release — still finds
    /// the column it expects.
    pub media_id: String,
    pub destination_id: String,
    /// Play the whole playlist again when it ends. §2's 24/7 setting.
    pub loop_forever: bool,
    pub desired_state: DesiredState,
    pub runtime_state: RuntimeState,
    pub restart_count: i64,
    pub last_error: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub stopped_at: Option<String>,
    pub last_heartbeat: Option<String>,
    /// Metering, for §22. Written by the manager as the broadcast runs.
    pub bytes_sent: i64,
    pub uptime_secs: i64,
    pub ffmpeg_exit_code: Option<i64>,
    /// The process actually sending, while one is running. For an operator
    /// looking at `top` beside the dashboard, and for nothing else: it is not
    /// a handle anything in the API acts on.
    pub ffmpeg_pid: Option<i64>,

    // --- 247streams broadcast metadata (§4) -------------------------------
    // Ours. Applied to the platform only by a destination that can
    // (`DestinationKind::can_publish_metadata`).
    pub title: String,
    pub description: String,
    /// Comma separated, as typed.
    pub tags: String,
    pub category: String,
    pub privacy: Privacy,

    pub settings: StreamSettings,
    pub schedule: Schedule,

    // --- playlist runtime, written by the worker (§2, §10) ---------------
    /// Videos in the playlist, as the editor shows them.
    pub item_count: i64,
    /// Entries in one pass, once `repeat_count` is expanded. What the
    /// "2 / 8" on the dashboard counts.
    pub play_count: i64,
    /// 1-based for display; 0 when nothing is playing.
    pub current_index: i64,
    pub current_item: Option<String>,
    pub next_item: Option<String>,
    pub current_position_secs: f64,
    pub current_duration_secs: f64,
    /// One pass through the playlist, in seconds.
    pub cycle_duration_secs: f64,

    /// Where this broadcast's YouTube resources are, when an account is
    /// connected. Every field is `None` for a pasted stream key, which is how
    /// the two providers coexist without a branch in the sending path. §7.
    pub youtube: crate::youtube::YoutubeLink,
}

/// A broadcast plus its playlist, for the screen that edits one.
#[derive(Debug, Clone, Serialize)]
pub struct BroadcastDetail {
    #[serde(flatten)]
    pub broadcast: Broadcast,
    pub items: Vec<BroadcastItem>,
}

impl Broadcast {
    /// Average bitrate over the broadcast's life, in bits per second.
    ///
    /// Bytes over uptime rather than FFmpeg's instantaneous figure, because
    /// what a month costs is the average, not the moment.
    pub fn average_bitrate_bps(&self) -> i64 {
        if self.uptime_secs <= 0 {
            return 0;
        }
        self.bytes_sent.saturating_mul(8) / self.uptime_secs
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BroadcastEvent {
    pub id: i64,
    pub broadcast_id: String,
    pub at: String,
    pub level: String,
    pub message: String,
}

/// The fields of a broadcast a user may change. Absent means "leave it".
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BroadcastPatch {
    pub name: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub tags: Option<String>,
    pub category: Option<String>,
    pub privacy: Option<Privacy>,
    pub loop_forever: Option<bool>,
    pub destination_id: Option<String>,
    pub settings: Option<StreamSettings>,
    pub schedule: Option<Schedule>,
}

/// Where the playlist has got to, as the worker sees it.
#[derive(Debug, Clone, Default)]
pub struct PlaylistProgress {
    /// 1-based for display. 0 when nothing is playing.
    pub index: i64,
    pub current_item: Option<String>,
    pub next_item: Option<String>,
    pub position_secs: f64,
    pub duration_secs: f64,
    pub cycle_secs: f64,
    /// Entries in one pass. 0 leaves the stored value alone.
    pub play_count: i64,
}
