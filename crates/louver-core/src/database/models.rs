//! Row types shared by the database, the IPC layer and the UI.

use crate::config::{OutputProfile, StreamMode};
use crate::streaming::playlist::PlaybackMode;
use crate::streaming::state::StreamState;
use serde::{Deserialize, Serialize};

/// Whether a media file can be broadcast as-is or needs optimization (§7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaStatus {
    /// Probed but not yet checked against a profile.
    Imported,
    /// Matches the profile exactly; broadcast straight from the original.
    Compatible,
    /// Needs a normalized cache file before it can be broadcast.
    OptimizationRequired,
    /// A normalized cache file exists and is valid.
    Normalized,
    /// The source file is gone or unreadable.
    Missing,
    Failed,
}

impl MediaStatus {
    pub fn id(self) -> &'static str {
        match self {
            Self::Imported => "imported",
            Self::Compatible => "compatible",
            Self::OptimizationRequired => "optimization_required",
            Self::Normalized => "normalized",
            Self::Missing => "missing",
            Self::Failed => "failed",
        }
    }
    pub fn from_id(s: &str) -> Option<Self> {
        Some(match s {
            "imported" => Self::Imported,
            "compatible" => Self::Compatible,
            "optimization_required" => Self::OptimizationRequired,
            "normalized" => Self::Normalized,
            "missing" => Self::Missing,
            "failed" => Self::Failed,
            _ => return None,
        })
    }
    /// True when the file can go straight into a broadcast manifest.
    pub fn is_broadcast_ready(self) -> bool {
        matches!(self, Self::Compatible | Self::Normalized)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Media {
    pub id: i64,
    /// Absolute path of the user's original file, never modified (§8).
    pub source_path: String,
    pub display_name: String,
    pub status: MediaStatus,
    /// Cache identity of the source; changes when the file changes (§8).
    pub media_hash: String,
    /// Path of the normalized cache file, when one exists.
    pub normalized_path: Option<String>,
    /// Profile the cache file was produced for.
    pub normalized_profile: Option<String>,
    pub duration_secs: f64,
    /// Duration actually broadcast: the normalized file's whole-frame length.
    pub normalized_duration_secs: Option<f64>,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub video_codec: String,
    pub audio_codec: Option<String>,
    pub pixel_format: Option<String>,
    pub is_hdr: bool,
    pub file_size: u64,
    pub added_at: String,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Playlist {
    pub id: i64,
    pub name: String,
    pub playback_mode: PlaybackMode,
    pub output_profile: OutputProfile,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistItem {
    pub id: i64,
    pub playlist_id: i64,
    pub media_id: i64,
    /// 0-based play order.
    pub position: i64,
    pub enabled: bool,
}

/// Days of the week as a 7-bit mask, Monday = bit 0 (§19).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaysOfWeek(pub u8);

impl DaysOfWeek {
    pub const MONDAY: u8 = 1 << 0;
    pub const TUESDAY: u8 = 1 << 1;
    pub const WEDNESDAY: u8 = 1 << 2;
    pub const THURSDAY: u8 = 1 << 3;
    pub const FRIDAY: u8 = 1 << 4;
    pub const SATURDAY: u8 = 1 << 5;
    pub const SUNDAY: u8 = 1 << 6;

    pub fn everyday() -> Self {
        Self(0b0111_1111)
    }
    pub fn weekdays() -> Self {
        Self(Self::MONDAY | Self::TUESDAY | Self::WEDNESDAY | Self::THURSDAY | Self::FRIDAY)
    }
    pub fn is_empty(self) -> bool {
        self.0 & 0b0111_1111 == 0
    }
    /// `weekday_index` is 0 = Monday … 6 = Sunday, matching chrono's
    /// `num_days_from_monday`.
    pub fn contains_index(self, weekday_index: u32) -> bool {
        weekday_index < 7 && self.0 & (1 << weekday_index) != 0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub id: i64,
    pub playlist_id: i64,
    pub days_of_week: DaysOfWeek,
    /// Local wall-clock "HH:MM".
    pub start_time: String,
    /// Local wall-clock "HH:MM". May be earlier than `start_time`, meaning the
    /// broadcast runs past midnight (§19).
    pub end_time: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamSession {
    pub id: i64,
    pub playlist_id: i64,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub scheduled_end: Option<String>,
    pub state: StreamState,
    pub mode: StreamMode,
    pub playback_mode: PlaybackMode,
    /// Seed used to resolve the play order, so a recovered session keeps it.
    pub order_seed: i64,
    pub restart_count: i64,
    pub user_requested_stop: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventLevel {
    Info,
    Warn,
    Error,
}

impl EventLevel {
    pub fn id(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
    pub fn from_id(s: &str) -> Self {
        match s {
            "warn" => Self::Warn,
            "error" => Self::Error,
            _ => Self::Info,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamEvent {
    pub id: i64,
    pub session_id: Option<i64>,
    pub at: String,
    pub level: EventLevel,
    /// Stable code when the event is an error, e.g. `LL-STREAM-002`.
    pub code: Option<String>,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_status_round_trips() {
        for s in [
            MediaStatus::Imported, MediaStatus::Compatible, MediaStatus::OptimizationRequired,
            MediaStatus::Normalized, MediaStatus::Missing, MediaStatus::Failed,
        ] {
            assert_eq!(MediaStatus::from_id(s.id()), Some(s));
        }
    }

    #[test]
    fn only_compatible_and_normalized_are_broadcast_ready() {
        assert!(MediaStatus::Compatible.is_broadcast_ready());
        assert!(MediaStatus::Normalized.is_broadcast_ready());
        for s in [MediaStatus::Imported, MediaStatus::OptimizationRequired, MediaStatus::Missing, MediaStatus::Failed] {
            assert!(!s.is_broadcast_ready(), "{s:?} must not be broadcastable");
        }
    }

    #[test]
    fn day_mask_matches_chrono_monday_zero_indexing() {
        let wd = DaysOfWeek::weekdays();
        assert!(wd.contains_index(0), "Monday");
        assert!(wd.contains_index(4), "Friday");
        assert!(!wd.contains_index(5), "Saturday");
        assert!(!wd.contains_index(6), "Sunday");
        assert!(DaysOfWeek::everyday().contains_index(6));
        assert!(!DaysOfWeek(0).contains_index(0));
        assert!(DaysOfWeek(0).is_empty());
        assert!(!DaysOfWeek::everyday().is_empty());
        assert!(!DaysOfWeek::everyday().contains_index(7), "out of range");
    }
}
