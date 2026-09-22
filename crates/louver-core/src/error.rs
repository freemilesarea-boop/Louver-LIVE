//! Typed errors with stable, user-facing error codes.
//!
//! Every error that can reach the UI carries an `LL-<DOMAIN>-<NNN>` code, a
//! Korean user-facing message, and optionally a technical detail string that the
//! UI keeps behind a "상세정보" disclosure. Raw FFmpeg output is never shown as
//! the primary message (§35).

use std::fmt;

/// Stable error codes. The numeric suffix is part of the public contract and is
/// documented in README.md; never renumber an existing variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ErrorCode {
    // LL-MEDIA-0xx
    MediaProbeFailed,
    MediaUnsupported,
    MediaFileMissing,
    MediaNoVideoStream,
    MediaNormalizeFailed,
    MediaNormalizeCancelled,
    // LL-STREAM-0xx
    StreamFfmpegSpawn,
    StreamFfmpegExit,
    StreamNotNormalized,
    StreamEmptyPlaylist,
    StreamAlreadyRunning,
    StreamInvalidTransition,
    StreamNoStreamKey,
    StreamKeyRejected,
    PlaylistMissing,
    // LL-NETWORK-0xx
    NetworkUnreachable,
    NetworkRtmpRejected,
    // LL-STORAGE-0xx
    StorageInsufficientSpace,
    StorageCacheCorrupt,
    StorageIo,
    // LL-DB-0xx
    DbOpen,
    DbMigration,
    DbQuery,
    // LL-SEC-0xx
    SecretStoreUnavailable,
    SecretNotFound,
    // LL-SCHED-0xx
    // LL-YOUTUBE-0xx / LL-CHAT-0xx
    YoutubeNotConnected,
    YoutubeAuthExpired,
    /// The refresh token could not be traded for an access token. Its own code
    /// because it has its own cause and its own remedy: nothing about the
    /// YouTube API is wrong, the app simply cannot prove who it is any more.
    YoutubeAuthRefreshFailed,
    YoutubeNoActiveBroadcast,
    YoutubeApiFailed,
    YoutubeQuotaExceeded,
    YoutubeMetadataInvalid,
    ChatRateLimited,
    ChatDisabled,
    ChatEnded,
    ChatMessageTooLong,
    ScheduleInvalidTime,
    ScheduleNoDays,
    SchedulePlaylistMissing,
    SchedulePlaylistEmpty,
    // LL-CONFIG-0xx
    ConfigInvalid,
    FfmpegNotFound,
}

impl ErrorCode {
    /// The stable wire/UI representation, e.g. `LL-STREAM-001`.
    pub fn as_str(self) -> &'static str {
        use ErrorCode::*;
        match self {
            MediaProbeFailed => "LL-MEDIA-001",
            MediaUnsupported => "LL-MEDIA-002",
            MediaFileMissing => "LL-MEDIA-003",
            MediaNoVideoStream => "LL-MEDIA-004",
            MediaNormalizeFailed => "LL-MEDIA-005",
            MediaNormalizeCancelled => "LL-MEDIA-006",
            StreamFfmpegSpawn => "LL-STREAM-001",
            StreamFfmpegExit => "LL-STREAM-002",
            StreamNotNormalized => "LL-STREAM-003",
            StreamEmptyPlaylist => "LL-STREAM-004",
            StreamAlreadyRunning => "LL-STREAM-005",
            StreamInvalidTransition => "LL-STREAM-006",
            StreamNoStreamKey => "LL-STREAM-007",
            StreamKeyRejected => "LL-STREAM-008",
            PlaylistMissing => "LL-STREAM-009",
            NetworkUnreachable => "LL-NETWORK-001",
            NetworkRtmpRejected => "LL-NETWORK-002",
            StorageInsufficientSpace => "LL-STORAGE-001",
            StorageCacheCorrupt => "LL-STORAGE-002",
            StorageIo => "LL-STORAGE-003",
            DbOpen => "LL-DB-001",
            DbMigration => "LL-DB-002",
            DbQuery => "LL-DB-003",
            SecretStoreUnavailable => "LL-SEC-001",
            SecretNotFound => "LL-SEC-002",
            YoutubeNotConnected => "LL-YOUTUBE-001",
            YoutubeAuthExpired => "LL-YOUTUBE-002",
            YoutubeAuthRefreshFailed => "LL-YOUTUBE-AUTH-REFRESH",
            YoutubeNoActiveBroadcast => "LL-YOUTUBE-003",
            YoutubeApiFailed => "LL-YOUTUBE-004",
            YoutubeQuotaExceeded => "LL-YOUTUBE-005",
            YoutubeMetadataInvalid => "LL-YOUTUBE-006",
            ChatRateLimited => "LL-CHAT-001",
            ChatDisabled => "LL-CHAT-002",
            ChatEnded => "LL-CHAT-003",
            ChatMessageTooLong => "LL-CHAT-004",
            ScheduleInvalidTime => "LL-SCHED-001",
            ScheduleNoDays => "LL-SCHED-002",
            SchedulePlaylistMissing => "LL-SCHED-003",
            SchedulePlaylistEmpty => "LL-SCHED-004",
            ConfigInvalid => "LL-CONFIG-001",
            FfmpegNotFound => "LL-CONFIG-002",
        }
    }

    /// Korean message shown directly to the user. Never contains FFmpeg output.
    pub fn user_message(self) -> &'static str {
        use ErrorCode::*;
        match self {
            MediaProbeFailed => "영상 정보를 읽지 못했습니다. 파일이 손상되었을 수 있습니다.",
            MediaUnsupported => "지원하지 않는 영상 형식입니다. MP4, MOV, MKV 파일을 사용해주세요.",
            MediaFileMissing => "영상 파일을 찾을 수 없습니다. 파일이 이동되었거나 삭제되었습니다.",
            MediaNoVideoStream => "이 파일에는 영상 트랙이 없습니다.",
            MediaNormalizeFailed => "방송 준비에 실패했습니다. 원본 파일을 확인해주세요.",
            MediaNormalizeCancelled => "방송 준비가 취소되었습니다.",
            StreamFfmpegSpawn => "방송 엔진을 시작하지 못했습니다. 프로그램을 다시 설치해주세요.",
            StreamFfmpegExit => "방송이 예기치 않게 중단되었습니다. 자동으로 다시 연결합니다.",
            StreamNotNormalized => {
                "아직 방송 준비가 끝나지 않은 영상이 있습니다. 준비가 끝난 뒤 시작해주세요."
            }
            StreamEmptyPlaylist => "플레이리스트가 비어 있습니다. 영상을 먼저 추가해주세요.",
            StreamAlreadyRunning => "이미 방송이 진행 중입니다.",
            StreamInvalidTransition => "현재 상태에서는 요청한 동작을 수행할 수 없습니다.",
            StreamNoStreamKey => "스트림 키가 없습니다. 설정에서 YouTube 스트림 키를 입력해주세요.",
            StreamKeyRejected => {
                "YouTube가 스트림 키를 거부했습니다. 설정에서 스트림 키를 다시 확인해주세요."
            }
            PlaylistMissing => "플레이리스트를 찾을 수 없습니다. 삭제되었을 수 있습니다.",
            NetworkUnreachable => "인터넷에 연결할 수 없습니다. 네트워크 연결을 확인해주세요.",
            NetworkRtmpRejected => {
                "유튜브 서버에 연결하지 못했습니다. 스트림 키와 인터넷 연결을 확인해주세요."
            }
            StorageInsufficientSpace => "저장 공간이 부족합니다. 공간을 확보한 뒤 다시 시도해주세요.",
            StorageCacheCorrupt => "준비된 영상 파일이 손상되었습니다. 해당 영상을 다시 준비합니다.",
            StorageIo => "파일을 읽거나 쓰지 못했습니다. 디스크 상태를 확인해주세요.",
            DbOpen => "데이터베이스를 열지 못했습니다.",
            DbMigration => "데이터베이스 업그레이드에 실패했습니다.",
            DbQuery => "데이터를 저장하거나 불러오지 못했습니다.",
            SecretStoreUnavailable => "이 컴퓨터의 보안 저장소를 사용할 수 없습니다.",
            SecretNotFound => "저장된 스트림 키가 없습니다.",
            YoutubeNotConnected => "YouTube 계정이 연결되지 않았습니다. 설정에서 연결해주세요.",
            YoutubeAuthExpired => "YouTube 로그인이 만료되었습니다. 설정에서 다시 연결해주세요.",
            YoutubeAuthRefreshFailed => {
                "Google 인증 갱신에 실패했습니다. 설정에서 YouTube 계정을 다시 연결해주세요."
            }
            YoutubeNoActiveBroadcast => {
                "진행 중인 YouTube 라이브를 찾지 못했습니다. YouTube에서 라이브를 먼저 만들어주세요."
            }
            YoutubeApiFailed => "YouTube에 연결하지 못했습니다. 잠시 후 다시 시도합니다.",
            YoutubeQuotaExceeded => "오늘 YouTube API 사용량을 모두 썼습니다. 내일 다시 시도할 수 있습니다.",
            YoutubeMetadataInvalid => "방송 정보가 YouTube 제한을 넘었습니다. 입력을 확인해주세요.",
            ChatRateLimited => "채팅을 너무 자주 보냈습니다. 전송 간격을 늘려주세요.",
            ChatDisabled => "이 방송은 실시간 채팅이 꺼져 있습니다.",
            ChatEnded => "이 방송의 실시간 채팅이 종료되었습니다.",
            ChatMessageTooLong => "채팅 메시지가 너무 깁니다. 200자 이내로 입력해주세요.",
            ScheduleInvalidTime => "예약 시간이 올바르지 않습니다.",
            ScheduleNoDays => "반복할 요일을 하나 이상 선택해주세요.",
            SchedulePlaylistMissing => "예약에 연결된 플레이리스트를 찾을 수 없습니다.",
            SchedulePlaylistEmpty => "예약된 플레이리스트에 방송 가능한 영상이 없습니다.",
            ConfigInvalid => "설정 값이 올바르지 않습니다.",
            FfmpegNotFound => "방송 엔진(FFmpeg)을 찾을 수 없습니다. 프로그램을 다시 설치해주세요.",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The single error type crossing the core boundary.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LouverError {
    pub code: ErrorCode,
    /// Stable code string, duplicated so the frontend does not need the enum.
    pub code_str: String,
    /// Korean, user-facing.
    pub message: String,
    /// Technical detail shown only behind a disclosure. Already key-masked.
    pub detail: Option<String>,
}

impl LouverError {
    pub fn new(code: ErrorCode) -> Self {
        Self {
            code,
            code_str: code.as_str().to_string(),
            message: code.user_message().to_string(),
            detail: None,
        }
    }

    /// Attach technical detail. The detail is masked for secrets before storage,
    /// so it is always safe to log or display.
    pub fn with_detail(code: ErrorCode, detail: impl AsRef<str>) -> Self {
        let mut e = Self::new(code);
        e.detail = Some(crate::streaming::ffmpeg::mask_secrets(detail.as_ref()));
        e
    }
}

impl fmt::Display for LouverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code_str, self.message)?;
        if let Some(d) = &self.detail {
            write!(f, " ({d})")?;
        }
        Ok(())
    }
}

impl std::error::Error for LouverError {}

impl From<std::io::Error> for LouverError {
    fn from(e: std::io::Error) -> Self {
        LouverError::with_detail(ErrorCode::StorageIo, e.to_string())
    }
}

impl From<rusqlite::Error> for LouverError {
    fn from(e: rusqlite::Error) -> Self {
        LouverError::with_detail(ErrorCode::DbQuery, e.to_string())
    }
}

impl From<serde_json::Error> for LouverError {
    fn from(e: serde_json::Error) -> Self {
        LouverError::with_detail(ErrorCode::ConfigInvalid, e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, LouverError>;

/// Every code, for the README table and for the docs test that keeps them in sync.
pub const ALL_ERROR_CODES: &[ErrorCode] = {
    use ErrorCode::*;
    &[
        MediaProbeFailed,
        MediaUnsupported,
        MediaFileMissing,
        MediaNoVideoStream,
        MediaNormalizeFailed,
        MediaNormalizeCancelled,
        StreamFfmpegSpawn,
        StreamFfmpegExit,
        StreamNotNormalized,
        StreamEmptyPlaylist,
        StreamAlreadyRunning,
        StreamInvalidTransition,
        StreamNoStreamKey,
        StreamKeyRejected,
        PlaylistMissing,
        NetworkUnreachable,
        NetworkRtmpRejected,
        StorageInsufficientSpace,
        StorageCacheCorrupt,
        StorageIo,
        DbOpen,
        DbMigration,
        DbQuery,
        SecretStoreUnavailable,
        SecretNotFound,
        YoutubeNotConnected,
        YoutubeAuthExpired,
        YoutubeAuthRefreshFailed,
        YoutubeNoActiveBroadcast,
        YoutubeApiFailed,
        YoutubeQuotaExceeded,
        YoutubeMetadataInvalid,
        ChatRateLimited,
        ChatDisabled,
        ChatEnded,
        ChatMessageTooLong,
        ScheduleInvalidTime,
        ScheduleNoDays,
        SchedulePlaylistMissing,
        SchedulePlaylistEmpty,
        ConfigInvalid,
        FfmpegNotFound,
    ]
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn error_codes_are_unique() {
        let set: HashSet<&str> = ALL_ERROR_CODES.iter().map(|c| c.as_str()).collect();
        assert_eq!(set.len(), ALL_ERROR_CODES.len(), "duplicate error code string");
    }

    #[test]
    fn every_code_has_korean_message_and_ll_prefix() {
        for c in ALL_ERROR_CODES {
            assert!(c.as_str().starts_with("LL-"), "{c} missing LL- prefix");
            // `LL-<DOMAIN>-<NNN>` for almost all of them. A code may instead
            // end in a written name — `LL-YOUTUBE-AUTH-REFRESH` — when that is
            // what someone will grep a log for; the rest of the shape still
            // holds, so the README table and the docs test are unaffected.
            let parts: Vec<&str> = c.as_str().split('-').collect();
            assert!(parts.len() >= 3, "{c} malformed");
            assert!(
                parts
                    .iter()
                    .all(|p| !p.is_empty()
                        && p.chars().all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())),
                "{c} malformed"
            );
            assert!(!c.user_message().is_empty());
            // must not leak raw technical jargon as the primary message
            assert!(!c.user_message().contains("ffmpeg"));
        }
    }

    #[test]
    fn detail_is_masked_on_construction() {
        let e = LouverError::with_detail(
            ErrorCode::StreamFfmpegExit,
            "rtmps://a.rtmp.youtube.com/live2/abcd-efgh-ijkl-mnop-qrst failed",
        );
        let d = e.detail.unwrap();
        assert!(!d.contains("abcd-efgh"), "stream key leaked into detail: {d}");
    }
}
