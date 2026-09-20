//! Getting a YouTube broadcast ready, without the user opening YouTube Studio.
//!
//! A scheduled window arrives with nothing on the channel: no live broadcast,
//! nothing "upcoming", nothing to apply a title to. The app used to stop there
//! and say "make a live broadcast in YouTube first", which is not something a
//! sleeping user can do — so the window passed with the stream never starting.
//!
//! This module decides what to do instead. It is deliberately pure: it takes
//! what Google reported and returns what should happen next, so the choices
//! that matter — reuse this broadcast or create one, bind this stream or
//! refuse — are testable without a network.
//!
//! The order matters and is not negotiable:
//!
//! 1. find or create the broadcast for this window,
//! 2. bind it to the ingestion stream the saved key publishes to,
//! 3. apply the metadata,
//! 4. *then* start FFmpeg,
//! 5. and only once the stream is receiving video, go live.
//!
//! Steps 1–3 before FFmpeg because a broadcast that goes live first is live
//! under whatever title it already had. Step 5 after, because YouTube refuses
//! to take a broadcast live while its stream is inactive.

use super::api::{LiveBroadcast, LiveStream};
use crate::error::{ErrorCode, LouverError, Result};

/// Which broadcast a window should use, and whether it has to be made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BroadcastChoice {
    /// One this app already prepared for this window.
    Reuse(LiveBroadcast),
    /// Nothing suitable exists; create one.
    Create,
}

/// The only two states a broadcast can be in and still be taken over.
///
/// An allowlist rather than a list of exclusions, because the list this has to
/// be right about is the one Google may add to. `testing` and `live` are in
/// progress, `complete` and `revoked` are over, and none of the four is
/// something to rename and stream into.
const REUSABLE_STATUSES: [&str; 2] = ["created", "ready"];

/// Pick the broadcast for a window out of what the channel already has.
///
/// Reuse is narrow on purpose. The candidates now arrive unfiltered — Google
/// will not accept `mine=true` and `broadcastStatus=upcoming` in one request,
/// so the whole of the channel's list comes back and the narrowing happens
/// here — and an arbitrary broadcast in it may be something the user set up by
/// hand for another time entirely. Taking that one over would rename their
/// broadcast and stream into it. A candidate has to be scheduled for *this*
/// window; anything else means create, including "there is exactly one other
/// upcoming broadcast and it is probably the right one".
///
/// `window_start` and `tolerance_secs` describe the window being started now.
pub fn choose_broadcast(
    candidates: &[LiveBroadcast],
    window_start: chrono::DateTime<chrono::Utc>,
    tolerance_secs: i64,
) -> BroadcastChoice {
    let mut best: Option<(i64, &LiveBroadcast)> = None;
    for b in candidates {
        if !REUSABLE_STATUSES.contains(&b.life_cycle_status.as_str()) {
            continue;
        }
        let Some(scheduled) = b.scheduled_start_time.as_deref() else { continue };
        let Ok(at) = chrono::DateTime::parse_from_rfc3339(scheduled) else { continue };
        let drift = (at.with_timezone(&chrono::Utc) - window_start).num_seconds().abs();
        if drift <= tolerance_secs && best.map(|(d, _)| drift < d).unwrap_or(true) {
            best = Some((drift, b));
        }
    }
    match best {
        Some((_, b)) => BroadcastChoice::Reuse(b.clone()),
        None => BroadcastChoice::Create,
    }
}

/// How far from the window's start a broadcast may be scheduled and still be
/// considered the same window's.
///
/// Wide enough to cover a retry a minute or two into the window, narrow enough
/// that tomorrow's broadcast at the same time is never mistaken for today's.
pub const REUSE_TOLERANCE_SECS: i64 = 15 * 60;

/// The ingestion endpoint the saved stream key publishes to.
///
/// Matched rather than guessed. Picking "the first stream" produces a
/// broadcast bound to an endpoint the app is not publishing to: YouTube shows
/// it waiting for video forever while FFmpeg happily sends elsewhere, and
/// nothing in either place says why.
///
/// The key is compared and dropped. It is not returned, logged or stored.
pub fn stream_for_key<'a>(streams: &'a [LiveStream], stream_key: &str) -> Result<&'a LiveStream> {
    streams.iter().find(|s| s.matches_key(stream_key)).ok_or_else(|| {
        LouverError::with_detail(
            ErrorCode::YoutubeApiFailed,
            "이 스트림 키에 해당하는 YouTube 스트림을 찾지 못했습니다. \
             YouTube Studio의 스트림 키와 앱에 저장된 키가 같은지 확인해주세요.",
        )
    })
}

/// Did the bind take?
///
/// `liveBroadcasts.bind` answers 200 and returns the broadcast; this checks
/// that the broadcast now names the stream that was asked for, because a
/// broadcast bound to something else looks identical until it never goes live.
pub fn verify_bound(broadcast: &LiveBroadcast, stream_id: &str) -> Result<()> {
    match broadcast.bound_stream_id.as_deref() {
        Some(id) if id == stream_id => Ok(()),
        other => Err(LouverError::with_detail(
            ErrorCode::YoutubeApiFailed,
            format!(
                "방송을 스트림에 연결하지 못했습니다 (요청 {stream_id}, 실제 {})",
                other.unwrap_or("없음")
            ),
        )),
    }
}

/// What to do about taking the broadcast live, once FFmpeg is publishing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoLive {
    /// The stream is not receiving video yet. Asking YouTube to go live now
    /// is refused, with an error that reads like a permissions problem.
    WaitForStream,
    /// YouTube will take it live by itself when the stream starts.
    AutoStart,
    /// Ask for it.
    Transition,
    /// Already there.
    AlreadyLive,
}

/// Decide the next step toward LIVE from what Google currently reports.
pub fn go_live_step(broadcast: &LiveBroadcast, stream: &LiveStream) -> GoLive {
    if broadcast.life_cycle_status == "live" {
        return GoLive::AlreadyLive;
    }
    if !stream.is_active() {
        return GoLive::WaitForStream;
    }
    if broadcast.enable_auto_start {
        return GoLive::AutoStart;
    }
    GoLive::Transition
}

/// How long to wait before the next attempt at a start that failed.
///
/// Bounded and increasing: a window whose first attempt hit a transient API
/// error should recover within it, and one that is failing for a reason that
/// will not clear should not spend the day retrying. The last value repeats
/// for the rest of the window.
pub const RETRY_BACKOFF_SECS: [u64; 4] = [5, 10, 20, 30];

pub fn retry_delay_secs(attempt: u32) -> u64 {
    let i = (attempt as usize).min(RETRY_BACKOFF_SECS.len() - 1);
    RETRY_BACKOFF_SECS[i]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::youtube::metadata::Privacy;

    fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&chrono::Utc)
    }

    fn broadcast(id: &str, scheduled: Option<&str>, status: &str) -> LiveBroadcast {
        LiveBroadcast {
            id: id.into(),
            title: "t".into(),
            privacy: Privacy::Unlisted,
            active_live_chat_id: None,
            life_cycle_status: status.into(),
            bound_stream_id: None,
            scheduled_start_time: scheduled.map(str::to_string),
            enable_auto_start: false,
            enable_auto_stop: false,
        }
    }

    #[test]
    fn an_empty_channel_means_create() {
        // The reported case: a scheduled window arrives and the channel has
        // nothing on it. Creating is the whole point — the user is asleep.
        assert_eq!(
            choose_broadcast(&[], at("2026-09-20T11:58:00Z"), REUSE_TOLERANCE_SECS),
            BroadcastChoice::Create
        );
    }

    #[test]
    fn a_broadcast_for_this_window_is_reused_rather_than_duplicated() {
        // A retry inside the window must not leave two broadcasts behind.
        let mine = broadcast("b1", Some("2026-09-20T11:58:00Z"), "ready");
        let choice =
            choose_broadcast(std::slice::from_ref(&mine), at("2026-09-20T11:59:30Z"), REUSE_TOLERANCE_SECS);
        assert_eq!(choice, BroadcastChoice::Reuse(mine));
    }

    #[test]
    fn someone_elses_broadcast_at_another_time_is_left_alone() {
        // Taking over an arbitrary upcoming broadcast would rename the user's
        // own scheduled stream and publish into it.
        let theirs = broadcast("theirs", Some("2026-09-21T20:00:00Z"), "ready");
        assert_eq!(
            choose_broadcast(&[theirs], at("2026-09-20T11:58:00Z"), REUSE_TOLERANCE_SECS),
            BroadcastChoice::Create
        );
    }

    #[test]
    fn a_broadcast_already_live_is_never_taken_over() {
        let live = broadcast("live", Some("2026-09-20T11:58:00Z"), "live");
        assert_eq!(
            choose_broadcast(&[live], at("2026-09-20T11:58:00Z"), REUSE_TOLERANCE_SECS),
            BroadcastChoice::Create
        );
    }

    #[test]
    fn the_closest_candidate_wins() {
        let near = broadcast("near", Some("2026-09-20T11:58:00Z"), "ready");
        let far = broadcast("far", Some("2026-09-20T12:08:00Z"), "ready");
        let choice = choose_broadcast(&[far, near.clone()], at("2026-09-20T11:58:30Z"), REUSE_TOLERANCE_SECS);
        assert_eq!(choice, BroadcastChoice::Reuse(near));
    }

    #[test]
    fn a_broadcast_with_no_scheduled_time_is_not_a_candidate() {
        assert_eq!(
            choose_broadcast(
                &[broadcast("x", None, "ready")],
                at("2026-09-20T11:58:00Z"),
                REUSE_TOLERANCE_SECS
            ),
            BroadcastChoice::Create
        );
    }

    // --- stream matching ---

    fn stream(id: &str, key: &str, status: &str) -> LiveStream {
        // Built through the parser so the private key field is set the same
        // way Google's response sets it.
        crate::youtube::api::parse_stream_for_tests(&serde_json::json!({
            "id": id,
            "snippet": { "title": "Main" },
            "status": { "streamStatus": status },
            "cdn": { "ingestionInfo": { "streamName": key } },
        }))
    }

    #[test]
    fn the_stream_is_the_one_the_saved_key_publishes_to() {
        let streams =
            [stream("s-other", "other-key", "inactive"), stream("s-mine", "abcd-efgh-ijkl", "inactive")];
        assert_eq!(stream_for_key(&streams, "abcd-efgh-ijkl").unwrap().id, "s-mine");
        // Not "the first one", which would bind a broadcast to an endpoint
        // nothing is publishing to.
        assert_ne!(stream_for_key(&streams, "abcd-efgh-ijkl").unwrap().id, "s-other");
    }

    #[test]
    fn a_key_with_no_matching_stream_says_so_instead_of_guessing() {
        let streams = [stream("s1", "other-key", "inactive")];
        let e = stream_for_key(&streams, "abcd-efgh-ijkl").unwrap_err();
        assert_eq!(e.code_str, "LL-YOUTUBE-004");
        // And the message says what to compare, without printing either key.
        let text = format!("{} {}", e.message, e.detail.clone().unwrap_or_default());
        assert!(text.contains("스트림 키"));
        assert!(!text.contains("abcd-efgh-ijkl"), "the key must not appear: {text}");
        assert!(!text.contains("other-key"), "{text}");
    }

    #[test]
    fn an_empty_ingestion_key_never_matches() {
        // A stream Google returned without a key must not match an empty
        // saved key and bind the broadcast to the wrong endpoint.
        let streams = [stream("s1", "", "inactive")];
        assert!(stream_for_key(&streams, "").is_err());
    }

    #[test]
    fn the_stream_key_is_not_in_the_debug_rendering() {
        let s = stream("s1", "abcd-efgh-ijkl", "active");
        let rendered = format!("{s:?}");
        assert!(!rendered.contains("abcd-efgh-ijkl"), "{rendered}");
        assert!(rendered.contains("s1"));
    }

    // --- binding ---

    #[test]
    fn a_bind_is_confirmed_from_what_google_returns() {
        let mut b = broadcast("b1", Some("2026-09-20T11:58:00Z"), "ready");
        b.bound_stream_id = Some("s-mine".into());
        assert!(verify_bound(&b, "s-mine").is_ok());
    }

    #[test]
    fn a_bind_that_landed_elsewhere_is_an_error_not_a_success() {
        let mut b = broadcast("b1", Some("2026-09-20T11:58:00Z"), "ready");
        b.bound_stream_id = Some("s-other".into());
        let e = verify_bound(&b, "s-mine").unwrap_err();
        assert!(e.detail.unwrap().contains("s-other"));

        b.bound_stream_id = None;
        assert!(verify_bound(&b, "s-mine").is_err(), "unbound is not bound");
    }

    // --- going live ---

    #[test]
    fn nothing_is_asked_of_youtube_while_the_stream_is_silent() {
        // Transitioning a broadcast whose stream is inactive is refused, with
        // an error that reads like a permissions problem and is not one.
        let b = broadcast("b1", Some("2026-09-20T11:58:00Z"), "ready");
        assert_eq!(go_live_step(&b, &stream("s1", "k", "inactive")), GoLive::WaitForStream);
    }

    #[test]
    fn an_auto_start_broadcast_is_left_to_youtube() {
        let mut b = broadcast("b1", Some("2026-09-20T11:58:00Z"), "ready");
        b.enable_auto_start = true;
        assert_eq!(go_live_step(&b, &stream("s1", "k", "active")), GoLive::AutoStart);
    }

    #[test]
    fn a_broadcast_that_will_not_auto_start_is_transitioned() {
        let b = broadcast("b1", Some("2026-09-20T11:58:00Z"), "ready");
        assert_eq!(go_live_step(&b, &stream("s1", "k", "active")), GoLive::Transition);
    }

    #[test]
    fn a_broadcast_already_live_is_left_where_it_is() {
        let mut b = broadcast("b1", Some("2026-09-20T11:58:00Z"), "live");
        b.enable_auto_start = true;
        assert_eq!(go_live_step(&b, &stream("s1", "k", "active")), GoLive::AlreadyLive);
    }

    // --- retry ---

    #[test]
    fn the_retry_backs_off_and_then_holds() {
        assert_eq!(retry_delay_secs(0), 5);
        assert_eq!(retry_delay_secs(1), 10);
        assert_eq!(retry_delay_secs(2), 20);
        assert_eq!(retry_delay_secs(3), 30);
        // Bounded, not exponential forever: a window that keeps failing must
        // keep trying at a steady, cheap rate rather than spinning or giving
        // up on the day.
        assert_eq!(retry_delay_secs(50), 30);
    }
}
