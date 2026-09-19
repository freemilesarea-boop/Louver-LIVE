//! Pre-broadcast checks (§29).
//!
//! §29 is explicit that we must not invent an upload-speed number when there is
//! no measurement server. CHECK 7 therefore reports reachability of the ingest
//! endpoint, and [`crate::system::SpeedTestProvider`] is left as the seam for a
//! real provider later.

use crate::database::models::Media;
use crate::error::ErrorCode;
use crate::system::{parse_rtmp_host, NetworkChecker};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckOutcome {
    Pass,
    /// Broadcasting can proceed, but the user should know.
    Warn,
    /// Broadcasting is blocked.
    Fail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub id: String,
    pub label: String,
    pub outcome: CheckOutcome,
    pub detail: String,
    pub code: Option<String>,
}

impl CheckResult {
    fn pass(id: &str, label: &str, detail: impl Into<String>) -> Self {
        Self { id: id.into(), label: label.into(), outcome: CheckOutcome::Pass, detail: detail.into(), code: None }
    }
    fn warn(id: &str, label: &str, detail: impl Into<String>) -> Self {
        Self { id: id.into(), label: label.into(), outcome: CheckOutcome::Warn, detail: detail.into(), code: None }
    }
    fn fail(id: &str, label: &str, detail: impl Into<String>, code: ErrorCode) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            outcome: CheckOutcome::Fail,
            detail: detail.into(),
            code: Some(code.as_str().to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreflightReport {
    pub checks: Vec<CheckResult>,
    pub can_broadcast: bool,
}

impl PreflightReport {
    pub fn first_failure(&self) -> Option<&CheckResult> {
        self.checks.iter().find(|c| c.outcome == CheckOutcome::Fail)
    }
}

/// Inputs gathered by the caller so the checks themselves stay pure and testable.
pub struct PreflightInput<'a> {
    pub playlist_exists: bool,
    /// Media rows for the enabled playlist items, in play order.
    pub media: &'a [Media],
    pub has_stream_key: bool,
    pub rtmps_url: &'a str,
    pub ffmpeg_ok: bool,
    pub ffmpeg_version: Option<&'a str>,
    pub license_allows_broadcast: bool,
    /// Dry runs skip the network and key checks.
    pub dry_run: bool,
}

/// Run CHECK 1–7 (§29).
pub fn run(input: &PreflightInput<'_>, net: &dyn NetworkChecker) -> PreflightReport {
    let mut checks = Vec::new();

    // CHECK 1 — playlist valid
    checks.push(if !input.playlist_exists {
        CheckResult::fail("playlist", "플레이리스트", "선택된 플레이리스트가 없습니다", ErrorCode::StreamEmptyPlaylist)
    } else if input.media.is_empty() {
        CheckResult::fail("playlist", "플레이리스트", "플레이리스트가 비어 있습니다", ErrorCode::StreamEmptyPlaylist)
    } else {
        CheckResult::pass("playlist", "플레이리스트", format!("{}개 영상", input.media.len()))
    });

    // CHECK 2 — every file present
    let missing: Vec<&str> = input
        .media
        .iter()
        .filter(|m| {
            let p = m.normalized_path.as_deref().unwrap_or(&m.source_path);
            !std::path::Path::new(p).is_file()
        })
        .map(|m| m.display_name.as_str())
        .collect();
    checks.push(if missing.is_empty() {
        CheckResult::pass("files", "파일 확인", "모든 파일이 존재합니다")
    } else {
        CheckResult::fail(
            "files",
            "파일 확인",
            format!("{}개 파일을 찾을 수 없습니다: {}", missing.len(), missing.join(", ")),
            ErrorCode::MediaFileMissing,
        )
    });

    // CHECK 3 — everything normalized or natively compatible
    let unready: Vec<&str> = input
        .media
        .iter()
        .filter(|m| !m.status.is_broadcast_ready())
        .map(|m| m.display_name.as_str())
        .collect();
    checks.push(if unready.is_empty() {
        CheckResult::pass("normalized", "방송 규격", "모든 영상이 방송 규격입니다")
    } else {
        CheckResult::fail(
            "normalized",
            "방송 규격",
            format!("{}개 영상의 최적화가 필요합니다: {}", unready.len(), unready.join(", ")),
            ErrorCode::StreamNotNormalized,
        )
    });

    // CHECK 4 — stream key
    checks.push(if input.dry_run {
        CheckResult::pass("stream_key", "스트림 키", "테스트 모드에서는 필요하지 않습니다")
    } else if input.has_stream_key {
        CheckResult::pass("stream_key", "스트림 키", "저장된 스트림 키를 사용합니다")
    } else {
        CheckResult::fail("stream_key", "스트림 키", ErrorCode::StreamNoStreamKey.user_message(), ErrorCode::StreamNoStreamKey)
    });

    // CHECK 5 — internet connectivity
    checks.push(if input.dry_run {
        CheckResult::pass("internet", "인터넷 연결", "테스트 모드에서는 확인하지 않습니다")
    } else if net.can_reach("www.google.com", 443, Duration::from_secs(5)) {
        CheckResult::pass("internet", "인터넷 연결", "연결됨")
    } else {
        CheckResult::fail("internet", "인터넷 연결", ErrorCode::NetworkUnreachable.user_message(), ErrorCode::NetworkUnreachable)
    });

    // CHECK 6 — FFmpeg
    checks.push(if input.ffmpeg_ok {
        CheckResult::pass("ffmpeg", "방송 엔진", input.ffmpeg_version.unwrap_or("FFmpeg 사용 가능"))
    } else {
        CheckResult::fail("ffmpeg", "방송 엔진", ErrorCode::FfmpegNotFound.user_message(), ErrorCode::FfmpegNotFound)
    });

    // CHECK 7 — ingest endpoint reachability.
    // Deliberately not a bandwidth figure: §29 forbids a fabricated Mbps value.
    checks.push(if input.dry_run {
        CheckResult::pass("ingest", "업로드 네트워크", "테스트 모드에서는 확인하지 않습니다")
    } else {
        match parse_rtmp_host(input.rtmps_url) {
            None => CheckResult::fail("ingest", "업로드 네트워크", "RTMPS 주소 형식이 올바르지 않습니다", ErrorCode::ConfigInvalid),
            Some((host, port)) => {
                if net.can_reach(&host, port, Duration::from_secs(8)) {
                    CheckResult::pass("ingest", "업로드 네트워크", format!("{host}:{port} 연결 가능"))
                } else {
                    // A warning, not a failure: FFmpeg may still succeed, and
                    // we would rather attempt the broadcast than refuse it.
                    CheckResult::warn("ingest", "업로드 네트워크", format!("{host}:{port}에 미리 연결하지 못했습니다"))
                }
            }
        }
    });

    // Licence gate (§47) — not one of the seven, but it blocks RTMPS.
    if !input.dry_run && !input.license_allows_broadcast {
        checks.push(CheckResult::fail("license", "라이선스", ErrorCode::LicenseMissing.user_message(), ErrorCode::LicenseMissing));
    }

    let can_broadcast = !checks.iter().any(|c| c.outcome == CheckOutcome::Fail);
    PreflightReport { checks, can_broadcast }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::models::MediaStatus;

    #[derive(Debug)]
    struct Net(bool);
    impl NetworkChecker for Net {
        fn can_reach(&self, _h: &str, _p: u16, _t: Duration) -> bool {
            self.0
        }
    }

    fn media_at(path: &std::path::Path, status: MediaStatus) -> Media {
        Media {
            id: 1,
            source_path: path.to_string_lossy().into_owned(),
            display_name: "a.mp4".into(),
            status,
            media_hash: "h".into(),
            normalized_path: Some(path.to_string_lossy().into_owned()),
            normalized_profile: Some("1080p30".into()),
            duration_secs: 10.0,
            normalized_duration_secs: Some(10.0),
            width: 1920,
            height: 1080,
            fps: 30.0,
            video_codec: "h264".into(),
            audio_codec: Some("aac".into()),
            pixel_format: Some("yuv420p".into()),
            is_hdr: false,
            file_size: 1,
            added_at: String::new(),
            last_error: None,
        }
    }

    fn good_input<'a>(media: &'a [Media]) -> PreflightInput<'a> {
        PreflightInput {
            playlist_exists: true,
            media,
            has_stream_key: true,
            rtmps_url: "rtmps://a.rtmps.youtube.com/live2",
            ffmpeg_ok: true,
            ffmpeg_version: Some("ffmpeg version 6.1.1"),
            license_allows_broadcast: true,
            dry_run: false,
        }
    }

    #[test]
    fn a_healthy_setup_passes_all_seven_checks() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a.mp4");
        std::fs::write(&f, b"x").unwrap();
        let m = vec![media_at(&f, MediaStatus::Normalized)];
        let r = run(&good_input(&m), &Net(true));
        assert!(r.can_broadcast, "{:?}", r.first_failure());
        assert_eq!(r.checks.len(), 7);
        assert!(r.checks.iter().all(|c| c.outcome == CheckOutcome::Pass));
    }

    #[test]
    fn an_empty_playlist_blocks_broadcasting() {
        let r = run(&good_input(&[]), &Net(true));
        assert!(!r.can_broadcast);
        assert_eq!(r.first_failure().unwrap().id, "playlist");
        assert_eq!(r.first_failure().unwrap().code.as_deref(), Some("LL-STREAM-004"));
    }

    #[test]
    fn a_missing_file_blocks_broadcasting_and_names_it() {
        let m = vec![media_at(std::path::Path::new("/no/such/file.mp4"), MediaStatus::Normalized)];
        let r = run(&good_input(&m), &Net(true));
        assert!(!r.can_broadcast);
        let f = r.checks.iter().find(|c| c.id == "files").unwrap();
        assert_eq!(f.outcome, CheckOutcome::Fail);
        assert!(f.detail.contains("a.mp4"));
    }

    #[test]
    fn an_unoptimized_file_blocks_broadcasting() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a.mp4");
        std::fs::write(&f, b"x").unwrap();
        let m = vec![media_at(&f, MediaStatus::OptimizationRequired)];
        let r = run(&good_input(&m), &Net(true));
        assert!(!r.can_broadcast);
        assert_eq!(
            r.checks.iter().find(|c| c.id == "normalized").unwrap().code.as_deref(),
            Some("LL-STREAM-003")
        );
    }

    #[test]
    fn a_missing_stream_key_blocks_a_live_broadcast_but_not_a_dry_run() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a.mp4");
        std::fs::write(&f, b"x").unwrap();
        let m = vec![media_at(&f, MediaStatus::Normalized)];

        let mut i = good_input(&m);
        i.has_stream_key = false;
        assert!(!run(&i, &Net(true)).can_broadcast);

        // §30: a dry run must work with no key at all.
        i.dry_run = true;
        let r = run(&i, &Net(false));
        assert!(r.can_broadcast, "dry run blocked by {:?}", r.first_failure());
    }

    #[test]
    fn no_internet_blocks_a_live_broadcast() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a.mp4");
        std::fs::write(&f, b"x").unwrap();
        let m = vec![media_at(&f, MediaStatus::Normalized)];
        let r = run(&good_input(&m), &Net(false));
        assert!(!r.can_broadcast);
        assert_eq!(r.checks.iter().find(|c| c.id == "internet").unwrap().outcome, CheckOutcome::Fail);
        // But an unreachable ingest host alone is only a warning.
        assert_eq!(r.checks.iter().find(|c| c.id == "ingest").unwrap().outcome, CheckOutcome::Warn);
    }

    #[test]
    fn a_missing_ffmpeg_blocks_broadcasting() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a.mp4");
        std::fs::write(&f, b"x").unwrap();
        let m = vec![media_at(&f, MediaStatus::Normalized)];
        let mut i = good_input(&m);
        i.ffmpeg_ok = false;
        let r = run(&i, &Net(true));
        assert!(!r.can_broadcast);
        assert_eq!(r.checks.iter().find(|c| c.id == "ffmpeg").unwrap().code.as_deref(), Some("LL-CONFIG-002"));
    }

    #[test]
    fn preflight_never_reports_a_fabricated_upload_speed() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a.mp4");
        std::fs::write(&f, b"x").unwrap();
        let m = vec![media_at(&f, MediaStatus::Normalized)];
        let r = run(&good_input(&m), &Net(true));
        let ingest = r.checks.iter().find(|c| c.id == "ingest").unwrap();
        // §29: reachability only — no invented Mbps figure.
        assert!(!ingest.detail.to_lowercase().contains("mbps"), "{}", ingest.detail);
    }

    #[test]
    fn a_bad_rtmps_url_is_a_failure() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a.mp4");
        std::fs::write(&f, b"x").unwrap();
        let m = vec![media_at(&f, MediaStatus::Normalized)];
        let mut i = good_input(&m);
        i.rtmps_url = "https://not-an-rtmp-url";
        let r = run(&i, &Net(true));
        assert!(!r.can_broadcast);
        assert_eq!(r.checks.iter().find(|c| c.id == "ingest").unwrap().outcome, CheckOutcome::Fail);
    }

    #[test]
    fn no_licence_blocks_rtmps_but_allows_a_dry_run() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a.mp4");
        std::fs::write(&f, b"x").unwrap();
        let m = vec![media_at(&f, MediaStatus::Normalized)];
        let mut i = good_input(&m);
        i.license_allows_broadcast = false;
        let r = run(&i, &Net(true));
        assert!(!r.can_broadcast, "§47: broadcasting requires a licence");
        assert_eq!(r.checks.iter().find(|c| c.id == "license").unwrap().code.as_deref(), Some("LL-LICENSE-001"));

        i.dry_run = true;
        assert!(run(&i, &Net(true)).can_broadcast, "dev/testing must not need a licence");
    }

    #[test]
    fn a_compatible_file_needs_no_normalized_copy() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a.mp4");
        std::fs::write(&f, b"x").unwrap();
        let mut m = media_at(&f, MediaStatus::Compatible);
        m.normalized_path = None; // broadcast straight from the source
        let r = run(&good_input(&[m]), &Net(true));
        assert!(r.can_broadcast, "{:?}", r.first_failure());
    }
}
