//! What a broadcast's YouTube preparation is doing, step by step.
//!
//! This exists because of a real failure that could not be diagnosed. A
//! scheduled window retried six times on a real Mac and wrote six identical
//! lines: `LL-YOUTUBE-004 · YouTube에 연결하지 못했습니다`. Every provisioning
//! call — refreshing the token, listing the broadcasts, creating one, finding
//! the ingestion stream, binding them, applying the metadata — fails with that
//! same code, so the log said only that *something* about YouTube did not
//! work, which is the one thing already known.
//!
//! So each call is named, and each one writes three possible lines:
//! `…_START`, `…_OK`, `…_FAIL`. A failed run now says which request Google
//! refused and what Google said about it, and a successful run leaves the
//! sequence behind so a scheduled start can be compared against a manual one.
//!
//! ## What is never written here
//!
//! Access tokens, refresh tokens, the client secret and the stream key. Not
//! masked — absent: no method on this type accepts one, the OK lines carry
//! resource ids only, and the FAIL lines carry [`crate::error::LouverError`],
//! whose detail is built from Google's `error` object alone. Ids are not
//! secrets: a broadcast id is in the watch URL and a stream id identifies an
//! endpoint without authorising anything.

use crate::error::LouverError;
use crate::logging::{LogTarget, Logger};
use std::sync::{Arc, Mutex};

/// One named call in getting a broadcast ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvisionStep {
    /// Trading the stored refresh token for an access token.
    TokenRefresh,
    /// `liveBroadcasts.list` — is there already a broadcast for this window?
    BroadcastList,
    /// `liveBroadcasts.insert` — there was not, so make one.
    BroadcastInsert,
    /// `liveStreams.list` — which ingestion endpoint does the saved key publish to?
    StreamList,
    /// `liveBroadcasts.bind` — attach the broadcast to that endpoint.
    BroadcastBind,
    /// The three metadata calls, as one step.
    MetadataApply,
    /// Waiting for FFmpeg's video to reach YouTube.
    StreamActive,
    /// `liveBroadcasts.transition` — take it live.
    BroadcastTransition,
}

/// Which start a sequence belongs to, so two runs in one log can be told
/// apart. The user's question — does a scheduled start do something different
/// from a manual one? — is only answerable if both are labelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvisionOrigin {
    Manual,
    Scheduled,
}

impl ProvisionOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Scheduled => "scheduled",
        }
    }
}

impl ProvisionStep {
    /// The event family, without the `_START`/`_OK`/`_FAIL` suffix.
    pub fn event(self) -> &'static str {
        match self {
            Self::TokenRefresh => "YOUTUBE_TOKEN_REFRESH",
            Self::BroadcastList => "YOUTUBE_BROADCAST_LIST",
            Self::BroadcastInsert => "YOUTUBE_BROADCAST_INSERT",
            Self::StreamList => "YOUTUBE_STREAM_LIST",
            Self::BroadcastBind => "YOUTUBE_BROADCAST_BIND",
            Self::MetadataApply => "YOUTUBE_METADATA_APPLY",
            Self::StreamActive => "YOUTUBE_STREAM_ACTIVE",
            Self::BroadcastTransition => "YOUTUBE_BROADCAST_TRANSITION",
        }
    }

    /// What the user is told failed, in the words of the thing they asked for
    /// rather than the API method that implements it.
    pub fn stage_label(self) -> &'static str {
        match self {
            Self::TokenRefresh => "Google 인증 갱신 실패",
            Self::BroadcastList => "예약 방송 목록 조회 실패",
            Self::BroadcastInsert => "예약 방송 생성 실패",
            Self::StreamList => "스트림 연결 실패",
            Self::BroadcastBind => "스트림 연결 실패",
            Self::MetadataApply => "방송 정보 적용 실패",
            Self::StreamActive => "스트림 수신 확인 실패",
            Self::BroadcastTransition => "LIVE 전환 실패",
        }
    }

    /// One line of advice under the stage, so the dashboard is not only
    /// telling the user which of our steps broke.
    pub fn remedy(self) -> &'static str {
        match self {
            Self::TokenRefresh => "설정 → YouTube에서 계정을 다시 연결해주세요.",
            Self::BroadcastList | Self::BroadcastInsert => {
                "YouTube 채널에서 실시간 스트리밍이 사용 설정되어 있는지 확인해주세요."
            }
            Self::StreamList | Self::BroadcastBind => {
                "설정에 입력한 스트림 키가 이 채널의 것인지 확인해주세요."
            }
            Self::MetadataApply => "방송 설정의 제목·설명·태그를 확인해주세요.",
            Self::StreamActive => "인터넷 연결과 스트림 키를 확인해주세요.",
            Self::BroadcastTransition => "잠시 후 자동으로 다시 시도합니다.",
        }
    }
}

/// How one step ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepOutcome {
    Started,
    Ok,
    Failed,
    /// Not needed this time — an existing broadcast was reused, so nothing was
    /// inserted. Recorded rather than omitted, because "the insert never ran"
    /// and "the insert ran and worked" look identical in a log that only
    /// writes what happened.
    Skipped,
}

/// What one step did, for the UI and for comparing two runs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StepRecord {
    pub step: ProvisionStep,
    pub outcome: StepOutcome,
    /// Ids and Google's own words. Never a credential — see the module docs.
    pub detail: Option<String>,
    pub error_code: Option<String>,
}

/// Writes the step lines and keeps the sequence.
///
/// Cloneable and shared: `provision_broadcast` passes the same recorder into
/// the metadata apply, so one run produces one trace in order.
#[derive(Debug, Clone)]
pub struct StepRecorder {
    logger: Arc<Logger>,
    origin: ProvisionOrigin,
    steps: Arc<Mutex<Vec<StepRecord>>>,
}

impl StepRecorder {
    pub fn new(logger: Arc<Logger>, origin: ProvisionOrigin) -> Self {
        Self { logger, origin, steps: Arc::new(Mutex::new(Vec::new())) }
    }

    pub fn origin(&self) -> ProvisionOrigin {
        self.origin
    }

    /// The sequence so far, oldest first.
    pub fn trace(&self) -> Vec<StepRecord> {
        self.steps.lock().unwrap().clone()
    }

    /// The step that failed, if one did.
    pub fn failed_step(&self) -> Option<ProvisionStep> {
        self.steps.lock().unwrap().iter().find(|r| r.outcome == StepOutcome::Failed).map(|r| r.step)
    }

    /// The call is about to be made. `note` is context, not a result — the
    /// window being prepared, the id being asked about.
    pub fn start(&self, step: ProvisionStep, note: &str) {
        self.record(step, StepOutcome::Started, note, None);
        self.logger.info(LogTarget::App, &self.line(step, "START", note));
    }

    /// It worked. `note` is what came back: an id, a count, a status.
    pub fn ok(&self, step: ProvisionStep, note: &str) {
        self.record(step, StepOutcome::Ok, note, None);
        self.logger.info(LogTarget::App, &self.line(step, "OK", note));
    }

    /// It was not needed.
    pub fn skipped(&self, step: ProvisionStep, note: &str) {
        self.record(step, StepOutcome::Skipped, note, None);
        self.logger.info(LogTarget::App, &self.line(step, "SKIP", note));
    }

    /// Google refused it, or it could not be reached.
    ///
    /// The error's own detail is what carries the method, the status and the
    /// reason — see [`crate::youtube::api::ApiFailure`] — so this adds the
    /// step's name and nothing else.
    pub fn fail(&self, step: ProvisionStep, err: &LouverError) -> LouverError {
        let detail = err.detail.clone().unwrap_or_else(|| err.message.clone());
        self.record(step, StepOutcome::Failed, &detail, Some(err.code_str.clone()));
        self.logger.warn(LogTarget::App, &self.line(step, "FAIL", &format!("{} · {}", err.code_str, detail)));
        err.clone()
    }

    /// Run a call, recording whichever way it goes.
    pub fn run<T>(
        &self,
        step: ProvisionStep,
        note: &str,
        call: impl FnOnce() -> crate::error::Result<T>,
        describe: impl FnOnce(&T) -> String,
    ) -> crate::error::Result<T> {
        self.start(step, note);
        match call() {
            Ok(v) => {
                self.ok(step, &describe(&v));
                Ok(v)
            }
            Err(e) => Err(self.fail(step, &e)),
        }
    }

    fn line(&self, step: ProvisionStep, suffix: &str, note: &str) -> String {
        let head = format!("{}_{}: origin={}", step.event(), suffix, self.origin.as_str());
        if note.is_empty() {
            head
        } else {
            format!("{head} · {note}")
        }
    }

    fn record(&self, step: ProvisionStep, outcome: StepOutcome, detail: &str, code: Option<String>) {
        self.steps.lock().unwrap().push(StepRecord {
            step,
            outcome,
            detail: (!detail.is_empty()).then(|| detail.to_string()),
            error_code: code,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{ErrorCode, LouverError};

    fn recorder() -> (StepRecorder, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let logger = Logger::new(dir.path()).unwrap();
        (StepRecorder::new(logger, ProvisionOrigin::Scheduled), dir)
    }

    #[test]
    fn every_step_has_the_event_name_the_log_reader_is_looking_for() {
        // These strings are what a support conversation greps for; they are
        // part of the contract, not an implementation detail.
        use ProvisionStep::*;
        assert_eq!(TokenRefresh.event(), "YOUTUBE_TOKEN_REFRESH");
        assert_eq!(BroadcastList.event(), "YOUTUBE_BROADCAST_LIST");
        assert_eq!(BroadcastInsert.event(), "YOUTUBE_BROADCAST_INSERT");
        assert_eq!(StreamList.event(), "YOUTUBE_STREAM_LIST");
        assert_eq!(BroadcastBind.event(), "YOUTUBE_BROADCAST_BIND");
        assert_eq!(MetadataApply.event(), "YOUTUBE_METADATA_APPLY");
        assert_eq!(StreamActive.event(), "YOUTUBE_STREAM_ACTIVE");
        assert_eq!(BroadcastTransition.event(), "YOUTUBE_BROADCAST_TRANSITION");
    }

    #[test]
    fn a_run_leaves_the_sequence_it_took() {
        let (rec, _d) = recorder();
        rec.start(ProvisionStep::BroadcastList, "window=2026-09-21T12:00:00Z");
        rec.ok(ProvisionStep::BroadcastList, "3개");
        rec.skipped(ProvisionStep::BroadcastInsert, "기존 방송 재사용");
        rec.fail(
            ProvisionStep::BroadcastBind,
            &LouverError::with_detail(ErrorCode::YoutubeApiFailed, "liveBroadcasts.bind HTTP 403"),
        );

        let trace = rec.trace();
        assert_eq!(trace.len(), 4);
        assert_eq!(trace[1].outcome, StepOutcome::Ok);
        assert_eq!(trace[2].outcome, StepOutcome::Skipped);
        assert_eq!(rec.failed_step(), Some(ProvisionStep::BroadcastBind));
        assert_eq!(trace[3].error_code.as_deref(), Some("LL-YOUTUBE-004"));
    }

    #[test]
    fn a_clean_run_reports_no_failed_step() {
        let (rec, _d) = recorder();
        rec.ok(ProvisionStep::TokenRefresh, "cached");
        assert_eq!(rec.failed_step(), None);
    }

    #[test]
    fn the_origin_is_on_every_line_so_two_runs_can_be_compared() {
        let (rec, dir) = recorder();
        rec.start(ProvisionStep::StreamList, "");
        rec.ok(ProvisionStep::StreamList, "stream=abc");
        let log = std::fs::read_to_string(dir.path().join("app.log")).unwrap();
        assert!(log.contains("YOUTUBE_STREAM_LIST_START: origin=scheduled"), "{log}");
        assert!(log.contains("YOUTUBE_STREAM_LIST_OK: origin=scheduled · stream=abc"), "{log}");
    }

    #[test]
    fn run_records_both_outcomes_without_the_caller_remembering_to() {
        let (rec, _d) = recorder();
        let ok: crate::error::Result<u32> =
            rec.run(ProvisionStep::StreamList, "", || Ok(7), |n| format!("{n}개"));
        assert_eq!(ok.unwrap(), 7);
        let err: crate::error::Result<u32> = rec.run(
            ProvisionStep::StreamList,
            "",
            || Err(LouverError::new(ErrorCode::YoutubeQuotaExceeded)),
            |_| String::new(),
        );
        assert_eq!(err.unwrap_err().code_str, "LL-YOUTUBE-005");
        let trace = rec.trace();
        assert_eq!(trace.len(), 4);
        assert_eq!(trace[1].detail.as_deref(), Some("7개"));
    }

    #[test]
    fn every_stage_label_and_remedy_is_written_for_a_person() {
        use ProvisionStep::*;
        for s in [
            TokenRefresh,
            BroadcastList,
            BroadcastInsert,
            StreamList,
            BroadcastBind,
            MetadataApply,
            StreamActive,
            BroadcastTransition,
        ] {
            // Korean, not an error code and not an API method name: the point
            // of the stage is that the user reads it instead of LL-YOUTUBE-004.
            assert!(!s.stage_label().is_empty());
            assert!(!s.remedy().is_empty());
            assert!(!s.stage_label().contains("LL-"), "{s:?}");
            assert!(!s.stage_label().contains('.'), "{s:?}");
        }
    }
}
