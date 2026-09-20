//! Output profiles and application paths.

use serde::{Deserialize, Serialize};

/// Broadcast output profile. V1 ships exactly two (§5); 4K/60fps are out of scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OutputProfile {
    /// 1920x1080 @ 30fps CFR, 10 Mbps — YouTube "1080p30 recommended".
    #[default]
    P1080p30,
    /// 1280x720 @ 30fps CFR, 4 Mbps — low bandwidth.
    P720p30,
}

impl OutputProfile {
    pub fn id(self) -> &'static str {
        match self {
            Self::P1080p30 => "1080p30",
            Self::P720p30 => "720p30",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "1080p30" => Some(Self::P1080p30),
            "720p30" => Some(Self::P720p30),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::P1080p30 => "1080p30 (권장)",
            Self::P720p30 => "720p30 (저대역폭)",
        }
    }

    pub fn width(self) -> u32 {
        match self {
            Self::P1080p30 => 1920,
            Self::P720p30 => 1280,
        }
    }

    pub fn height(self) -> u32 {
        match self {
            Self::P1080p30 => 1080,
            Self::P720p30 => 720,
        }
    }

    /// Frames per second. CFR in both profiles.
    pub fn fps(self) -> u32 {
        30
    }

    /// GOP length in frames — 2 second keyframe interval at 30fps (§5).
    pub fn gop(self) -> u32 {
        self.fps() * 2
    }

    /// Target video bitrate in kbit/s.
    pub fn video_kbps(self) -> u32 {
        match self {
            Self::P1080p30 => 10_000,
            Self::P720p30 => 4_000,
        }
    }

    pub fn audio_kbps(self) -> u32 {
        192
    }

    pub fn audio_sample_rate(self) -> u32 {
        48_000
    }

    pub fn audio_channels(self) -> u32 {
        2
    }

    /// MP4 video track timescale. Fixed across all normalized files so that the
    /// concat demuxer can stream-copy them as one continuous input (§14).
    pub fn video_timescale(self) -> u32 {
        30_000
    }

    /// Bytes per second of normalized output, for disk-usage estimation (§10).
    pub fn bytes_per_second(self) -> u64 {
        ((self.video_kbps() + self.audio_kbps()) as u64 * 1000) / 8
    }

    pub fn all() -> &'static [OutputProfile] {
        &[OutputProfile::P1080p30, OutputProfile::P720p30]
    }
}

/// How the live FFmpeg session moves data (§41).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StreamMode {
    /// Primary path: no encoders at all, pure remux. Validated in §74.
    #[default]
    StreamCopy,
    /// Fallback shown in the UI as "호환 모드".
    CompatibilityEncode,
}

impl StreamMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::StreamCopy => "STREAM COPY",
            Self::CompatibilityEncode => "COMPATIBILITY ENCODE",
        }
    }
}

/// Default YouTube RTMPS ingest endpoint (§15).
pub const DEFAULT_RTMPS_URL: &str = "rtmps://a.rtmps.youtube.com/live2";

/// Local test ingest. `npm run app` starts a listener here, so the local
/// broadcast test publishes over a real socket rather than into a file.
///
/// Deliberately not 1935: that is the standard RTMP port, and taking over
/// whatever a developer already has listening there — OBS, nginx-rtmp — would
/// be a surprise. Nothing normally listens on 1945.
pub const DEFAULT_LOCAL_TEST_URL: &str = "rtmp://127.0.0.1:1945/live/louver-test";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_round_trips_through_id() {
        for p in OutputProfile::all() {
            assert_eq!(OutputProfile::from_id(p.id()), Some(*p));
        }
        assert_eq!(OutputProfile::from_id("4k60"), None);
    }

    #[test]
    fn keyframe_interval_is_two_seconds() {
        for p in OutputProfile::all() {
            assert_eq!(p.gop(), p.fps() * 2, "{} GOP must be 2s", p.id());
        }
    }

    #[test]
    fn disk_estimate_is_sane() {
        // 1080p30 at 10192 kbps ≈ 1.27 MB/s ≈ 4.59 GB/hour
        let per_hour = OutputProfile::P1080p30.bytes_per_second() * 3600;
        assert!((4..6).contains(&(per_hour / 1_000_000_000)), "{per_hour}");
    }
}
