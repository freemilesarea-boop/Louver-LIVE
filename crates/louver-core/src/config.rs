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

    /// Target video bitrate in kbit/s, for streams that must be re-encoded.
    ///
    /// 1080p30 was 10 Mbps and is now 6. Measured on 30-second 1080p30 clips
    /// through this exact argv, varying only `-b:v`:
    ///
    /// | content | 4 Mbps | 6 Mbps | 8 Mbps | 10 Mbps |
    /// | --- | --- | --- | --- | --- |
    /// | slow gradient, SSIM | 0.99977 | 0.99985 | 0.99988 | 0.99988 |
    /// | slow gradient, size | 15.0 MB | 22.3 MB | 28.6 MB | 28.7 MB |
    /// | dense synthetic, SSIM | 0.99146 | 0.99539 | 0.99733 | 0.99842 |
    ///
    /// On the content this product exists for — a music video held on screen —
    /// the encoder cannot even spend 10 Mbps: 8 and 10 produce the same file to
    /// within 0.1 MB and the same SSIM to five decimal places. 6 Mbps is 22%
    /// smaller than that ceiling for an SSIM difference of 0.00003, which no
    /// viewer can see, and it sits inside YouTube's recommended range for a
    /// 1080p30 live ingest where 10 Mbps sits above it.
    ///
    /// Encode *time* barely moved across the whole sweep (8.5–11 s), so this is
    /// a cache-size and upload-headroom change, not a speed one — the speed
    /// came from not encoding conformant files at all.
    ///
    /// A source that already conforms keeps its own bitrate untouched: it is
    /// copied, not re-encoded, so this number never degrades it.
    pub fn video_kbps(self) -> u32 {
        match self {
            Self::P1080p30 => 6_000,
            // Unchanged: the sweep above was run at 1080p, and a number that
            // was not measured is not a number to change.
            Self::P720p30 => 4_000,
        }
    }

    /// H.264 profiles a copied video may use.
    ///
    /// Everything here decodes on the hardware YouTube viewers actually have.
    /// The 10-bit and 4:2:2/4:4:4 variants are absent on purpose — they carry a
    /// pixel format the profile does not allow anyway, so they are caught twice.
    pub fn allows_h264_profile(self, profile: &str) -> bool {
        const OK: [&str; 4] = ["baseline", "constrained baseline", "main", "high"];
        // An empty string means ffprobe did not say. A file that reached here
        // has already matched codec, geometry and pixel format, so an unnamed
        // profile is not grounds on its own to spend minutes re-encoding.
        profile.is_empty() || OK.contains(&profile.to_ascii_lowercase().as_str())
    }

    /// The H.264 level the encoder declares, as `-level` wants it.
    pub fn h264_level_str(self) -> String {
        let l = self.max_h264_level();
        format!("{}.{}", l / 10, l % 10)
    }

    /// Highest H.264 level a copied video may declare, ×10.
    ///
    /// The encoder targets exactly this, so a file this product made always
    /// satisfies its own check — 4.2 was hardcoded once, and at 720p that
    /// produced cache entries the compatibility check then rejected.
    pub fn max_h264_level(self) -> u32 {
        match self {
            Self::P1080p30 => 42,
            Self::P720p30 => 32,
        }
    }

    /// Longest keyframe gap a copied video may have, in seconds.
    ///
    /// A copy keeps whatever spacing it arrived with. Twice the interval we
    /// encode to is the point past which YouTube's ingest starts to suffer and
    /// a viewer joining mid-loop waits noticeably for a picture.
    pub fn max_copy_gop_secs(self) -> f64 {
        f64::from(self.gop()) / f64::from(self.fps()) * 2.0
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
        // 1080p30 at 6192 kbps ≈ 0.77 MB/s ≈ 2.79 GB/hour. Was 4.59 GB when
        // the video target was 10 Mbps; see `video_kbps` for why it is 6.
        let per_hour = OutputProfile::P1080p30.bytes_per_second() * 3600;
        assert!((2..4).contains(&(per_hour / 1_000_000_000)), "{per_hour}");
        // 720p is smaller still, and both stay well under a terabyte a day.
        assert!(OutputProfile::P720p30.bytes_per_second() < OutputProfile::P1080p30.bytes_per_second());
    }
}
