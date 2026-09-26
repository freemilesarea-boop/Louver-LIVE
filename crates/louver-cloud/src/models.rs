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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub user_id: String,
    pub plan_id: String,
    pub plan_label: String,
    pub status: String,
    pub limits: std::collections::BTreeMap<String, i64>,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Broadcast {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub media_id: String,
    pub destination_id: String,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BroadcastEvent {
    pub id: i64,
    pub broadcast_id: String,
    pub at: String,
    pub level: String,
    pub message: String,
}
