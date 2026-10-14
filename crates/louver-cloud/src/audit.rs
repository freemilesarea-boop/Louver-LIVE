//! What YouTube says about a broadcast, turned into one word an operator can act on.
//!
//! This is the judgement half of the read-only production diagnosis. It is a
//! pure function of what was actually observed so that it can be read in one
//! sitting and tested without a network — the same reason
//! [`crate::youtube::next_step`] is one.
//!
//! The vocabulary is fixed by the operator runbook rather than by YouTube, and
//! the two do not line up one-to-one: YouTube has `created`, `revoked` and
//! `complete`, and the runbook has seven verdicts that do not include them. So
//! every verdict carries a reason in the operator's own language, and the
//! mapping for the states the runbook has no word for is spelled out below
//! rather than guessed at the call site.
//!
//! Nothing here reads a stream key, a token or a title. It takes status
//! strings and two booleans, which is all a verdict needs.

use crate::youtube::Stuck;

/// One word per broadcast, as the runbook names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// YouTube says the broadcast is live and we are sending to it.
    ConfirmedLive,
    /// Parked in the monitor stream's private preview. The fault behind the P0:
    /// video is arriving, the watch page has nothing, and someone has to press
    /// 실시간 시작.
    StuckTesting,
    /// YouTube has the ingest but has not left `ready`.
    StuckReady,
    /// YouTube is not receiving video, whatever this end believes.
    IngestNotActive,
    /// What we are sending to and what the customer is watching are not the
    /// same broadcast — or the encoder and YouTube disagree about whether
    /// anything is being sent at all.
    BroadcastMismatch,
    /// The question could not be asked because the credentials were refused.
    AuthError,
    /// The question could not be asked, or YouTube answered something this
    /// build has no verdict for. Never a success.
    Unknown,
}

impl Verdict {
    /// The exact token the runbook and the report use.
    pub fn label(self) -> &'static str {
        match self {
            Self::ConfirmedLive => "CONFIRMED LIVE",
            Self::StuckTesting => "STUCK TESTING",
            Self::StuckReady => "STUCK READY",
            Self::IngestNotActive => "INGEST NOT ACTIVE",
            Self::BroadcastMismatch => "BROADCAST MISMATCH",
            Self::AuthError => "AUTH ERROR",
            Self::Unknown => "UNKNOWN",
        }
    }

    /// Does an operator have to do something about this one?
    ///
    /// `CONFIRMED LIVE` is the only verdict that needs nothing. `UNKNOWN`
    /// counts as needing attention on purpose: not knowing is not the same as
    /// being fine, which is the whole lesson of the outage this came from.
    pub fn needs_attention(self) -> bool {
        self != Self::ConfirmedLive
    }
}

/// Whether YouTube could be asked at all, kept apart from what it said.
///
/// A refused credential and an unreachable network are different problems with
/// different remedies, and neither is evidence about the broadcast.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reach {
    /// YouTube answered.
    Answered,
    /// The token was refused — 401/403, an expired grant, a revoked consent.
    Refused,
    /// No answer: a timeout, a 5xx, a quota refusal, a DNS failure.
    ///
    /// The default, so a half-filled observation cannot read as a success.
    #[default]
    Unreachable,
}

/// Everything a verdict is allowed to depend on.
///
/// Deliberately strings and booleans rather than the API types: the point is
/// that the decision can be written down from a printed table, and tested the
/// same way.
#[derive(Debug, Clone, Copy)]
pub struct Observation<'a> {
    pub reach: Reach,
    /// `status.lifeCycleStatus` from `liveBroadcasts.list`.
    pub life_cycle_status: &'a str,
    /// `status.streamStatus` from `liveStreams.list`.
    pub stream_status: &'a str,
    /// `contentDetails.boundStreamId` as YouTube reports it.
    pub bound_stream_id: Option<&'a str>,
    /// The stream id 247streams has stored for this broadcast.
    pub stored_stream_id: Option<&'a str>,
    /// `contentDetails.enableAutoStart`.
    pub enable_auto_start: bool,
    /// `contentDetails.monitorStream.enableMonitorStream`.
    pub enable_monitor_stream: bool,
    /// Is this end actually sending? A live FFmpeg process for this broadcast.
    pub encoder_running: bool,
}

/// The verdict, and why — in that order, because the why is what gets acted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Finding {
    pub verdict: Verdict,
    pub reason: &'static str,
    /// Set when an operator pressing 실시간 시작 (or changing a setting) is the
    /// remedy, so the runbook text comes from one place.
    pub stuck: Option<Stuck>,
}

impl Finding {
    fn of(verdict: Verdict, reason: &'static str) -> Self {
        Self { verdict, reason, stuck: None }
    }

    fn stuck_at(verdict: Verdict, reason: &'static str, stuck: Stuck) -> Self {
        Self { verdict, reason, stuck: Some(stuck) }
    }

    /// YouTube answered 404 for the broadcast id 247streams has stored.
    ///
    /// Not an unreachable API and not a status: the broadcast this service
    /// believes in does not exist on the channel, so whatever the customer is
    /// watching, it is not this. Its own constructor because it is an
    /// API-layer fact rather than one of the status fields [`classify`] reads.
    pub fn broadcast_not_on_youtube() -> Self {
        Self::of(Verdict::BroadcastMismatch, "저장된 YouTube 방송 ID가 YouTube에 없습니다")
    }
}

/// One broadcast as YouTube described it, owned so it can be carried out of
/// the call that fetched it.
///
/// Holds status fields only. No title, no stream key, no token — a verdict
/// does not need them and a diagnosis must not print them.
#[derive(Debug, Clone, Default)]
pub struct Seen {
    pub reach: Reach,
    pub life_cycle_status: String,
    pub stream_status: String,
    pub bound_stream_id: Option<String>,
    pub enable_auto_start: bool,
    pub enable_monitor_stream: bool,
    /// `status.healthStatus.status` — `good`, `ok`, `bad`, `noData`.
    pub health_status: String,
    /// The reasons behind a `bad`/`noData` ingest. Reasons only; YouTube's
    /// longer descriptions can quote back the URL they are about.
    pub configuration_issues: Vec<String>,
    /// YouTube answered 404 for the stored broadcast id.
    pub not_found: bool,
    /// What went wrong, when `reach` is not `Answered`. For the operator's
    /// eyes; already masked by the core on its way out.
    pub error: Option<String>,
}

impl Seen {
    /// The verdict for this broadcast, given what this end knows.
    ///
    /// `stored_stream_id` and `encoder_running` come from 247streams rather
    /// than from YouTube, which is exactly why they are arguments: the whole
    /// point of the audit is to compare the two sides.
    pub fn finding(&self, stored_stream_id: Option<&str>, encoder_running: bool) -> Finding {
        if self.not_found {
            return Finding::broadcast_not_on_youtube();
        }
        classify(&Observation {
            reach: self.reach,
            life_cycle_status: &self.life_cycle_status,
            stream_status: &self.stream_status,
            bound_stream_id: self.bound_stream_id.as_deref(),
            stored_stream_id,
            enable_auto_start: self.enable_auto_start,
            enable_monitor_stream: self.enable_monitor_stream,
            encoder_running,
        })
    }
}

/// Why a broadcast sitting in a preview is sitting there.
///
/// `enableAutoStart` on with a monitor stream is the product's own old bug;
/// `enableAutoStart` off is a broadcast someone configured by hand.
fn why_parked(o: &Observation<'_>) -> Stuck {
    if o.enable_monitor_stream {
        Stuck::MonitorStream
    } else {
        Stuck::AutoStartOff
    }
}

/// Classify one broadcast.
///
/// Order matters and is the argument of the function:
///
/// 1. If YouTube could not be asked, nothing else observed is worth anything.
/// 2. A binding that points somewhere else makes every other field a fact
///    about a different broadcast, so it is settled before they are read.
/// 3. Only then does what YouTube said mean what it appears to mean.
pub fn classify(o: &Observation<'_>) -> Finding {
    match o.reach {
        Reach::Refused => {
            return Finding::of(
                Verdict::AuthError,
                "YouTube가 인증을 거부했습니다. 채널 재연결이 필요할 수 있습니다",
            )
        }
        Reach::Unreachable => {
            return Finding::of(
                Verdict::Unknown,
                "YouTube에 물어볼 수 없었습니다. 방송 상태에 대한 증거가 아닙니다",
            )
        }
        Reach::Answered => {}
    }

    // The binding, before anything that depends on it.
    if let (Some(stored), Some(bound)) = (o.stored_stream_id, o.bound_stream_id) {
        if stored != bound {
            return Finding::of(
                Verdict::BroadcastMismatch,
                "저장된 스트림과 YouTube가 이 방송에 연결한 스트림이 다릅니다",
            );
        }
    }
    if o.bound_stream_id.is_none() && o.stored_stream_id.is_some() {
        return Finding::of(
            Verdict::BroadcastMismatch,
            "YouTube 방송에 연결된 스트림이 없습니다. 보내는 영상이 이 방송에 도달하지 않습니다",
        );
    }

    match o.life_cycle_status {
        "live" | "liveStarting" => {
            if o.encoder_running {
                Finding::of(Verdict::ConfirmedLive, "YouTube가 라이브라고 답했고 인코더도 보내고 있습니다")
            } else {
                // Worth its own verdict: the channel is live and the video
                // feeding it is gone, which is a black screen for viewers.
                Finding::of(
                    Verdict::BroadcastMismatch,
                    "YouTube는 라이브인데 이 서버의 인코더는 보내고 있지 않습니다",
                )
            }
        }
        "testing" | "testStarting" => Finding::stuck_at(
            Verdict::StuckTesting,
            "YouTube가 미리보기에서 멈춰 있습니다. 공개 시청 페이지에는 아무것도 나오지 않습니다",
            why_parked(o),
        ),
        "ready" | "created" => {
            if o.stream_status == "active" {
                Finding::stuck_at(
                    Verdict::StuckReady,
                    "YouTube가 영상을 받고 있는데 방송은 시작되지 않았습니다",
                    why_parked(o),
                )
            } else {
                Finding::of(Verdict::IngestNotActive, "YouTube가 아직 영상을 받지 못했습니다")
            }
        }
        // YouTube has words the runbook does not. Mapped here, once, rather
        // than guessed at each call site.
        "complete" => {
            if o.encoder_running {
                Finding::of(
                    Verdict::BroadcastMismatch,
                    "YouTube 방송은 이미 종료되었는데 인코더가 계속 보내고 있습니다",
                )
            } else {
                Finding::of(Verdict::Unknown, "YouTube 방송이 종료된 상태입니다")
            }
        }
        "revoked" => Finding::of(Verdict::Unknown, "YouTube가 이 방송을 차단했습니다"),
        _ => Finding::of(Verdict::Unknown, "YouTube가 이 빌드가 모르는 상태를 답했습니다"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A broadcast that is working, which every case below varies from.
    fn healthy() -> Observation<'static> {
        Observation {
            reach: Reach::Answered,
            life_cycle_status: "live",
            stream_status: "active",
            bound_stream_id: Some("s-1"),
            stored_stream_id: Some("s-1"),
            enable_auto_start: true,
            enable_monitor_stream: false,
            encoder_running: true,
        }
    }

    #[test]
    fn a_working_broadcast_is_the_only_one_that_needs_nothing() {
        let f = classify(&healthy());
        assert_eq!(f.verdict, Verdict::ConfirmedLive);
        assert!(!f.verdict.needs_attention());
        assert_eq!(f.stuck, None);
    }

    #[test]
    fn the_p0_state_is_named_and_carries_its_remedy() {
        // Exactly what the outage looked like: autostart on, monitor stream on
        // because nobody turned it off, ingest arriving, parked in testing.
        let o = Observation { life_cycle_status: "testing", enable_monitor_stream: true, ..healthy() };
        let f = classify(&o);
        assert_eq!(f.verdict, Verdict::StuckTesting);
        assert_eq!(f.stuck, Some(Stuck::MonitorStream));
        assert!(f.stuck.unwrap().explain().contains("실시간 시작"), "the remedy names the button");
    }

    #[test]
    fn a_hand_configured_broadcast_is_told_apart_from_the_products_own_bug() {
        let o = Observation {
            life_cycle_status: "testing",
            enable_auto_start: false,
            enable_monitor_stream: false,
            ..healthy()
        };
        assert_eq!(classify(&o).stuck, Some(Stuck::AutoStartOff));
    }

    #[test]
    fn an_ingest_that_arrived_with_the_broadcast_still_ready_is_a_mismatch_not_a_wait() {
        let o = Observation { life_cycle_status: "ready", stream_status: "active", ..healthy() };
        assert_eq!(classify(&o).verdict, Verdict::StuckReady);
    }

    #[test]
    fn a_broadcast_waiting_for_video_is_not_called_stuck() {
        let o = Observation { life_cycle_status: "ready", stream_status: "inactive", ..healthy() };
        assert_eq!(classify(&o).verdict, Verdict::IngestNotActive);
    }

    #[test]
    fn a_stream_bound_somewhere_else_outranks_whatever_else_youtube_said() {
        // The lifecycle says live, and it is the wrong broadcast's lifecycle.
        let o = Observation { bound_stream_id: Some("someone-elses"), ..healthy() };
        let f = classify(&o);
        assert_eq!(f.verdict, Verdict::BroadcastMismatch);
        assert!(f.reason.contains("다릅니다"));
    }

    #[test]
    fn a_broadcast_with_nothing_bound_to_it_is_a_mismatch() {
        let o = Observation { bound_stream_id: None, ..healthy() };
        assert_eq!(classify(&o).verdict, Verdict::BroadcastMismatch);
    }

    #[test]
    fn youtube_live_with_a_dead_encoder_is_a_mismatch_not_a_success() {
        // Viewers are on a live channel watching nothing.
        let o = Observation { encoder_running: false, ..healthy() };
        let f = classify(&o);
        assert_eq!(f.verdict, Verdict::BroadcastMismatch);
        assert!(f.verdict.needs_attention());
    }

    #[test]
    fn an_encoder_still_sending_into_a_finished_broadcast_is_a_mismatch() {
        let o = Observation { life_cycle_status: "complete", ..healthy() };
        assert_eq!(classify(&o).verdict, Verdict::BroadcastMismatch);
    }

    #[test]
    fn a_finished_broadcast_nobody_is_sending_to_is_merely_finished() {
        let o = Observation { life_cycle_status: "complete", encoder_running: false, ..healthy() };
        assert_eq!(classify(&o).verdict, Verdict::Unknown);
    }

    #[test]
    fn a_refused_credential_is_its_own_verdict() {
        let o = Observation { reach: Reach::Refused, ..healthy() };
        assert_eq!(classify(&o).verdict, Verdict::AuthError);
    }

    #[test]
    fn an_unanswered_question_is_never_a_live_claim() {
        // The rule the outage was about, at the level of the audit: silence is
        // not success, even when every other field says the broadcast is fine.
        let o = Observation { reach: Reach::Unreachable, ..healthy() };
        let f = classify(&o);
        assert_eq!(f.verdict, Verdict::Unknown);
        assert!(f.verdict.needs_attention());
    }

    #[test]
    fn a_state_this_build_does_not_know_is_not_guessed() {
        let o = Observation { life_cycle_status: "somethingNew", ..healthy() };
        assert_eq!(classify(&o).verdict, Verdict::Unknown);
    }

    #[test]
    fn a_broadcast_youtube_has_never_heard_of_is_a_mismatch() {
        let seen = Seen { not_found: true, reach: Reach::Answered, ..Seen::default() };
        let f = seen.finding(Some("s-1"), true);
        assert_eq!(f.verdict, Verdict::BroadcastMismatch);
        assert!(f.reason.contains("없습니다"));
    }

    #[test]
    fn an_owned_observation_classifies_the_same_as_a_borrowed_one() {
        let seen = Seen {
            reach: Reach::Answered,
            life_cycle_status: "testing".into(),
            stream_status: "active".into(),
            bound_stream_id: Some("s-1".into()),
            enable_auto_start: true,
            enable_monitor_stream: true,
            ..Seen::default()
        };
        let f = seen.finding(Some("s-1"), true);
        assert_eq!(f.verdict, Verdict::StuckTesting);
        assert_eq!(f.stuck, Some(Stuck::MonitorStream));
    }

    #[test]
    fn an_unfilled_observation_defaults_to_knowing_nothing() {
        // `Seen::default()` is what a failed call leaves behind, so its
        // default must not read as a success.
        let f = Seen::default().finding(None, true);
        assert_eq!(f.verdict, Verdict::Unknown);
        assert!(f.verdict.needs_attention());
    }

    #[test]
    fn every_verdict_has_a_distinct_label() {
        let all = [
            Verdict::ConfirmedLive,
            Verdict::StuckTesting,
            Verdict::StuckReady,
            Verdict::IngestNotActive,
            Verdict::BroadcastMismatch,
            Verdict::AuthError,
            Verdict::Unknown,
        ];
        let mut seen = std::collections::BTreeSet::new();
        for v in all {
            assert!(seen.insert(v.label()), "duplicate label {}", v.label());
            assert_eq!(v.label(), v.label().to_uppercase(), "the runbook writes them in caps");
        }
        assert_eq!(seen.len(), 7, "the runbook names seven");
    }
}
