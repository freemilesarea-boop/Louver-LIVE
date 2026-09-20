//! Staying inside YouTube's free daily quota.
//!
//! The product rule is a cost rule: Louver Live's YouTube integration must
//! never put a bill on anyone's Google Cloud account. The Cloud side of that
//! is configuration — no billing account is linked to the project, so there is
//! nothing for Google to charge against and a request past the quota is
//! refused rather than billed (see `YOUTUBE_OAUTH_PRODUCTION.md` §0).
//!
//! This is the app's side of it: spend the free allowance deliberately, stop
//! before the end of it, and never keep hammering an endpoint that has already
//! said no. What it protects is not the bill — that is impossible by
//! configuration — but the *broadcast*: a run that burns the day's allowance
//! in an hour leaves the rest of the day with no metadata and no chat.
//!
//! ## Where the numbers come from
//!
//! [`COSTS`] is the 2026 quota table, supplied by the product owner from
//! Google's official quota calculator. It could not be checked from the
//! machine this was written on — `developers.google.com` is unreachable from
//! here — so it is single-sourced, and this table is the one place to correct
//! if Google's ever differs. Everything else derives from it; there are no
//! quota numbers anywhere else in the codebase.
//!
//! ## Two kinds of allowance
//!
//! Most methods draw on one shared pool of units — 10,000 a day, a read
//! costing 1 and a write 50. A few draw on *their own* daily allowance counted
//! in **calls, not units**: `search.list` and `videos.insert` each get 100
//! calls a day and cost 1 unit apiece. Pricing those as 100-unit writes, as
//! this module used to, is wrong in both directions at once — it overstates
//! what they take from the shared pool and says nothing about the limit that
//! actually stops them. [`ApiMethod`] carries both, and [`QuotaState`] counts
//! both.
//!
//! Neither bucketed method is called by this product today, and
//! `the_app_calls_nothing_from_a_granular_bucket` keeps it that way on
//! purpose: they are modelled so that adding one cannot quietly mis-account.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The default daily allowance of combined units for a YouTube Data API
/// project.
pub const FREE_DAILY_UNITS: u32 = 10_000;

/// Stop this far short of the combined allowance.
///
/// Sized to finish what is already in flight: one complete metadata apply is
/// 103 units and one chat message is 50, so this leaves room for both to
/// complete rather than failing halfway through changing a title. It also
/// covers the case the table cannot — a method Google adds, or one this app
/// adds without updating [`COSTS`], which is charged the write price.
pub const RESERVE_UNITS: u32 = 200;

/// A daily allowance counted in calls rather than units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GranularBucket {
    /// Stable key, used for persistence. Never renamed.
    pub key: &'static str,
    pub daily_calls: u32,
}

/// `search.list`: 100 calls a day, 1 unit each.
pub const SEARCH_BUCKET: GranularBucket = GranularBucket { key: "search.list", daily_calls: 100 };
/// `videos.insert`: 100 calls a day, 1 unit each.
pub const VIDEO_INSERT_BUCKET: GranularBucket = GranularBucket { key: "videos.insert", daily_calls: 100 };

/// The YouTube Data API methods this product knows how to price.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ApiMethod {
    ChannelsList,
    LiveBroadcastsList,
    LiveBroadcastsUpdate,
    LiveBroadcastsInsert,
    LiveBroadcastsBind,
    LiveBroadcastsTransition,
    LiveStreamsList,
    LiveStreamsInsert,
    LiveStreamsUpdate,
    VideosList,
    VideosUpdate,
    LiveChatMessagesList,
    LiveChatMessagesInsert,
    /// Its own daily allowance of calls. Not used by this product.
    SearchList,
    /// Its own daily allowance of calls. Not used by this product.
    VideosInsert,
    /// Something not in the table. Charged as a write, because guessing low is
    /// the mistake that ends in a 403 partway through applying a title.
    Unknown,
}

/// What each method costs. The single source of the numbers; see the module
/// docs for where they came from.
pub const COSTS: &[(ApiMethod, u32, Option<GranularBucket>)] = &[
    (ApiMethod::ChannelsList, 1, None),
    (ApiMethod::LiveBroadcastsList, 1, None),
    (ApiMethod::LiveBroadcastsUpdate, 50, None),
    (ApiMethod::LiveBroadcastsInsert, 50, None),
    (ApiMethod::LiveBroadcastsBind, 50, None),
    (ApiMethod::LiveBroadcastsTransition, 50, None),
    (ApiMethod::LiveStreamsList, 1, None),
    (ApiMethod::LiveStreamsInsert, 50, None),
    (ApiMethod::LiveStreamsUpdate, 50, None),
    (ApiMethod::VideosList, 1, None),
    (ApiMethod::VideosUpdate, 50, None),
    (ApiMethod::LiveChatMessagesList, 1, None),
    (ApiMethod::LiveChatMessagesInsert, 50, None),
    (ApiMethod::SearchList, 1, Some(SEARCH_BUCKET)),
    (ApiMethod::VideosInsert, 1, Some(VIDEO_INSERT_BUCKET)),
    (ApiMethod::Unknown, 50, None),
];

impl ApiMethod {
    fn row(self) -> (u32, Option<GranularBucket>) {
        COSTS.iter().find(|(m, _, _)| *m == self).map(|(_, u, b)| (*u, *b)).unwrap_or((50, None))
    }

    /// Combined units this method spends.
    pub fn units(self) -> u32 {
        self.row().0
    }

    /// The method's own daily call allowance, where it has one.
    pub fn bucket(self) -> Option<GranularBucket> {
        self.row().1
    }

    /// Which method an outgoing request is.
    ///
    /// `url` may be a full URL or a bare path; only the resource and the verb
    /// matter. Anything unrecognised is [`ApiMethod::Unknown`].
    pub fn classify(verb: &str, url: &str) -> Self {
        let path = url.split('?').next().unwrap_or("");
        // Drop the scheme and host, so a full URL and a bare path classify the
        // same. The test harness points the API at a local base with no
        // `/youtube/v3` in it, and a request there costs exactly what the same
        // request costs against Google.
        let path = match path.split_once("://") {
            Some((_, rest)) => rest.find('/').map(|i| &rest[i..]).unwrap_or(""),
            None => path,
        };
        // Everything after the API version prefix, e.g. "liveChat/messages".
        let resource = path
            .rsplit_once("/youtube/v3/")
            .map(|(_, r)| r)
            .unwrap_or_else(|| path.trim_start_matches('/'))
            .trim_matches('/');
        // POST and PUT are both writes and both cost 50, but they are not the
        // same method, and a log line saying which one Google refused is the
        // difference between "a YouTube call failed" and "it would not let us
        // create the broadcast".
        let verb = if verb.eq_ignore_ascii_case("GET") {
            "GET"
        } else if verb.eq_ignore_ascii_case("PUT") {
            "PUT"
        } else {
            "POST"
        };
        match (resource, verb) {
            ("channels", "GET") => Self::ChannelsList,
            ("liveBroadcasts", "GET") => Self::LiveBroadcastsList,
            ("liveBroadcasts", "POST") => Self::LiveBroadcastsInsert,
            ("liveBroadcasts", "PUT") => Self::LiveBroadcastsUpdate,
            ("liveBroadcasts/bind", _) => Self::LiveBroadcastsBind,
            ("liveBroadcasts/transition", _) => Self::LiveBroadcastsTransition,
            ("liveStreams", "GET") => Self::LiveStreamsList,
            ("liveStreams", "POST") => Self::LiveStreamsInsert,
            ("liveStreams", "PUT") => Self::LiveStreamsUpdate,
            ("videos", "GET") => Self::VideosList,
            ("videos", "POST") => Self::VideosInsert,
            ("videos", "PUT") => Self::VideosUpdate,
            ("liveChat/messages", "GET") => Self::LiveChatMessagesList,
            ("liveChat/messages", _) => Self::LiveChatMessagesInsert,
            ("search", _) => Self::SearchList,
            _ => Self::Unknown,
        }
    }

    /// The method's name as Google's own documentation writes it, e.g.
    /// `liveBroadcasts.insert`.
    ///
    /// Used in the log and in the error detail, so a failure names the request
    /// that failed rather than the feature that wanted it.
    pub fn name(self) -> &'static str {
        match self {
            Self::ChannelsList => "channels.list",
            Self::LiveBroadcastsList => "liveBroadcasts.list",
            Self::LiveBroadcastsUpdate => "liveBroadcasts.update",
            Self::LiveBroadcastsInsert => "liveBroadcasts.insert",
            Self::LiveBroadcastsBind => "liveBroadcasts.bind",
            Self::LiveBroadcastsTransition => "liveBroadcasts.transition",
            Self::LiveStreamsList => "liveStreams.list",
            Self::LiveStreamsInsert => "liveStreams.insert",
            Self::LiveStreamsUpdate => "liveStreams.update",
            Self::VideosList => "videos.list",
            Self::VideosUpdate => "videos.update",
            Self::LiveChatMessagesList => "liveChatMessages.list",
            Self::LiveChatMessagesInsert => "liveChatMessages.insert",
            Self::SearchList => "search.list",
            Self::VideosInsert => "videos.insert",
            Self::Unknown => "youtube.unknown",
        }
    }
}

/// The day's spending, and whether Google has already said the day is over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct QuotaState {
    /// Which quota day this counts. See [`quota_day`].
    pub day: String,
    /// Combined units spent today.
    pub spent: u32,
    /// Google answered `quotaExceeded` on the combined pool. Nothing more is
    /// sent until the day rolls over, whatever the local count says.
    pub exhausted: bool,
    /// Calls made today against each granular bucket, keyed by
    /// [`GranularBucket::key`].
    #[serde(default)]
    pub bucket_calls: BTreeMap<String, u32>,
    /// Buckets Google has refused, which shut independently of the pool.
    #[serde(default)]
    pub exhausted_buckets: BTreeSet<String>,
}

/// Which quota day an instant falls in.
///
/// YouTube's quota resets at midnight Pacific. Working out Pacific from UTC
/// needs the daylight-saving rules, and this crate carries no timezone
/// database, so the boundary is taken at 08:00 UTC — midnight Pacific Standard
/// Time. During daylight time the real reset is an hour earlier, so this rolls
/// over late, which spends less rather than more. Erring the other way would
/// mean spending the new day's allowance against the old day's exhausted flag.
pub fn quota_day(now: DateTime<Utc>) -> String {
    (now - Duration::hours(8)).format("%Y-%m-%d").to_string()
}

impl QuotaState {
    /// Start the count again when the quota day has rolled over.
    pub fn roll_over(&mut self, now: DateTime<Utc>) {
        let today = quota_day(now);
        if self.day != today {
            *self = QuotaState { day: today, ..Default::default() };
        }
    }

    /// Combined units left before the reserve.
    pub fn remaining(&self, cap: u32) -> u32 {
        if self.exhausted {
            return 0;
        }
        cap.saturating_sub(RESERVE_UNITS).saturating_sub(self.spent)
    }

    pub fn bucket_calls_made(&self, b: GranularBucket) -> u32 {
        self.bucket_calls.get(b.key).copied().unwrap_or(0)
    }

    pub fn bucket_is_exhausted(&self, b: GranularBucket) -> bool {
        self.exhausted_buckets.contains(b.key) || self.bucket_calls_made(b) >= b.daily_calls
    }

    /// Can this call still be made today, on both allowances it draws on?
    pub fn can_afford(&self, m: ApiMethod, cap: u32) -> bool {
        if self.exhausted || m.units() > self.remaining(cap) {
            return false;
        }
        match m.bucket() {
            Some(b) => !self.bucket_is_exhausted(b),
            None => true,
        }
    }

    /// Charge a call to both allowances it draws on.
    pub fn charge(&mut self, m: ApiMethod) {
        self.spent = self.spent.saturating_add(m.units());
        if let Some(b) = m.bucket() {
            *self.bucket_calls.entry(b.key.to_string()).or_insert(0) += 1;
        }
    }

    /// Google's own answer, which overrides the local estimate: the estimate
    /// can be wrong, and this cannot.
    ///
    /// A refusal on a method with its own allowance shuts that allowance
    /// alone — the shared pool is untouched and the rest of the app carries on.
    pub fn mark_exhausted(&mut self, m: ApiMethod) {
        match m.bucket() {
            Some(b) => {
                self.exhausted_buckets.insert(b.key.to_string());
            }
            None => self.exhausted = true,
        }
    }

    /// Percentage of the usable combined allowance spent, for the UI.
    pub fn used_percent(&self, cap: u32) -> u8 {
        if self.exhausted {
            return 100;
        }
        let usable = cap.saturating_sub(RESERVE_UNITS).max(1);
        ((self.spent.min(usable) as u64 * 100) / usable as u64) as u8
    }
}

/// The day's ledger, shared by everything that calls YouTube.
///
/// One of these is held by the app and consulted by every request, so a budget
/// cannot be forgotten at a call site: the metering lives in the transport,
/// not in the callers.
pub struct QuotaGuard {
    state: std::sync::Mutex<QuotaState>,
    cap: u32,
    /// Called whenever the ledger changes, so it survives a restart. Without
    /// persistence a relaunch would start the day's count at zero and spend
    /// an allowance that is already gone.
    persist: Box<dyn Fn(&QuotaState) + Send + Sync>,
}

impl std::fmt::Debug for QuotaGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuotaGuard").field("state", &self.state).field("cap", &self.cap).finish()
    }
}

impl QuotaGuard {
    pub fn new(restored: QuotaState, cap: u32, persist: Box<dyn Fn(&QuotaState) + Send + Sync>) -> Self {
        Self { state: std::sync::Mutex::new(restored), cap, persist }
    }

    /// A guard with a fresh day, no persistence and the default allowance.
    pub fn unlimited_for_tests() -> Self {
        Self::new(QuotaState::default(), FREE_DAILY_UNITS, Box::new(|_| {}))
    }

    pub fn snapshot(&self) -> QuotaState {
        let mut s = self.state.lock().unwrap();
        s.roll_over(Utc::now());
        s.clone()
    }

    pub fn cap(&self) -> u32 {
        self.cap
    }

    pub fn is_exhausted(&self) -> bool {
        self.snapshot().exhausted
    }

    /// Charge a call up front, or refuse it.
    ///
    /// Charged before the request rather than after, because a request that
    /// gets as far as Google costs its units whatever the answer is.
    pub fn try_spend(&self, m: ApiMethod) -> crate::error::Result<()> {
        let mut s = self.state.lock().unwrap();
        s.roll_over(Utc::now());
        if !s.can_afford(m, self.cap) {
            let detail = match m.bucket() {
                Some(b) if s.bucket_is_exhausted(b) => {
                    format!("{} 오늘 호출 {}/{}회", b.key, s.bucket_calls_made(b), b.daily_calls)
                }
                _ => format!("오늘 사용량 {}/{} 단위", s.spent, self.cap),
            };
            return Err(crate::error::LouverError::with_detail(
                crate::error::ErrorCode::YoutubeQuotaExceeded,
                detail,
            ));
        }
        s.charge(m);
        (self.persist)(&s);
        Ok(())
    }

    /// Google said this allowance is done. Believe it over the local estimate.
    pub fn note_exhausted(&self, m: ApiMethod) {
        let mut s = self.state.lock().unwrap();
        s.roll_over(Utc::now());
        s.mark_exhausted(m);
        (self.persist)(&s);
    }
}

/// An [`HttpClient`](crate::youtube::api::HttpClient) that spends the day's
/// allowance before it spends the network.
///
/// Wrapping the transport rather than the call sites is the point: the chat
/// bot runs on its own thread and builds its own API client, and metering that
/// separately is the kind of thing that gets missed. Here there is one place
/// to look, and nothing can reach Google around it.
#[derive(Debug)]
pub struct MeteredClient {
    inner: std::sync::Arc<dyn crate::youtube::api::HttpClient>,
    quota: std::sync::Arc<QuotaGuard>,
}

impl MeteredClient {
    pub fn new(
        inner: std::sync::Arc<dyn crate::youtube::api::HttpClient>,
        quota: std::sync::Arc<QuotaGuard>,
    ) -> Self {
        Self { inner, quota }
    }

    pub fn quota(&self) -> &std::sync::Arc<QuotaGuard> {
        &self.quota
    }
}

impl crate::youtube::api::HttpClient for MeteredClient {
    fn request(
        &self,
        method: &str,
        url: &str,
        bearer: &str,
        body: Option<serde_json::Value>,
    ) -> crate::error::Result<(u16, String)> {
        let called = ApiMethod::classify(method, url);
        self.quota.try_spend(called)?;
        let answered = self.inner.request(method, url, bearer, body);
        if let Ok((status, text)) = &answered {
            // Google's own refusal latches the allowance shut, however much
            // the local count thought was left.
            if !(200..300).contains(status)
                && crate::youtube::api::classify(*status, text).code
                    == crate::error::ErrorCode::YoutubeQuotaExceeded
            {
                self.quota.note_exhausted(called);
            }
        }
        answered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn classify(verb: &str, url: &str) -> ApiMethod {
        ApiMethod::classify(verb, url)
    }

    #[test]
    fn every_call_the_app_makes_is_recognised() {
        // The exact URLs `api.rs` builds, full and path-only.
        assert_eq!(classify("GET", "/channels?part=snippet&mine=true"), ApiMethod::ChannelsList);
        assert_eq!(
            classify("GET", "/liveBroadcasts?part=id,snippet&broadcastStatus=active"),
            ApiMethod::LiveBroadcastsList
        );
        assert_eq!(
            classify("PUT", "/liveBroadcasts?part=id,snippet,status"),
            ApiMethod::LiveBroadcastsUpdate
        );
        assert_eq!(classify("GET", "/videos?part=snippet,status&id=x"), ApiMethod::VideosList);
        assert_eq!(classify("PUT", "/videos?part=snippet"), ApiMethod::VideosUpdate);
        assert_eq!(classify("POST", "/liveChat/messages?part=snippet"), ApiMethod::LiveChatMessagesInsert);
        assert_eq!(classify("GET", "/liveChat/messages?liveChatId=x"), ApiMethod::LiveChatMessagesList);
        assert_eq!(
            classify("GET", "https://www.googleapis.com/youtube/v3/videos?part=snippet"),
            ApiMethod::VideosList
        );
    }

    #[test]
    fn a_full_url_costs_the_same_as_the_path_it_ends_with() {
        // The host is not part of the price. A local test base classified as
        // Unknown once, which charged a 1-unit read as a 50-unit write.
        for base in ["", "https://www.googleapis.com/youtube/v3", "http://127.0.0.1:41234"] {
            assert_eq!(classify("GET", &format!("{base}/videos?part=snippet")), ApiMethod::VideosList);
            assert_eq!(classify("PUT", &format!("{base}/videos?part=snippet")), ApiMethod::VideosUpdate);
            assert_eq!(
                classify("POST", &format!("{base}/liveChat/messages?part=snippet")),
                ApiMethod::LiveChatMessagesInsert
            );
        }
    }

    #[test]
    fn the_costs_are_the_2026_table() {
        assert_eq!(ApiMethod::ChannelsList.units(), 1);
        assert_eq!(ApiMethod::LiveBroadcastsList.units(), 1);
        assert_eq!(ApiMethod::VideosList.units(), 1);
        assert_eq!(ApiMethod::LiveChatMessagesList.units(), 1);

        assert_eq!(ApiMethod::LiveBroadcastsUpdate.units(), 50);
        assert_eq!(ApiMethod::LiveBroadcastsInsert.units(), 50);
        assert_eq!(ApiMethod::LiveBroadcastsBind.units(), 50);
        assert_eq!(ApiMethod::LiveBroadcastsTransition.units(), 50);
        assert_eq!(ApiMethod::LiveStreamsInsert.units(), 50);
        assert_eq!(ApiMethod::LiveStreamsUpdate.units(), 50);
        assert_eq!(ApiMethod::VideosUpdate.units(), 50);
        assert_eq!(ApiMethod::LiveChatMessagesInsert.units(), 50);
    }

    #[test]
    fn a_bucketed_method_costs_one_unit_and_one_call() {
        // The 2026 change: not a 100-unit write. One unit against the shared
        // pool, and one of a hundred calls against its own allowance.
        assert_eq!(ApiMethod::SearchList.units(), 1);
        assert_eq!(ApiMethod::SearchList.bucket(), Some(SEARCH_BUCKET));
        assert_eq!(ApiMethod::VideosInsert.units(), 1);
        assert_eq!(ApiMethod::VideosInsert.bucket(), Some(VIDEO_INSERT_BUCKET));
        assert_eq!(SEARCH_BUCKET.daily_calls, 100);
        assert_eq!(VIDEO_INSERT_BUCKET.daily_calls, 100);
    }

    #[test]
    fn the_shared_pool_methods_have_no_bucket() {
        for m in [
            ApiMethod::ChannelsList,
            ApiMethod::LiveBroadcastsList,
            ApiMethod::LiveBroadcastsUpdate,
            ApiMethod::VideosList,
            ApiMethod::VideosUpdate,
            ApiMethod::LiveChatMessagesInsert,
        ] {
            assert_eq!(m.bucket(), None, "{m:?}");
        }
    }

    #[test]
    fn an_unknown_call_is_charged_as_a_write() {
        // Guessing low is the mistake that ends in a 403 mid-broadcast.
        assert_eq!(classify("PATCH", "/somethingNew"), ApiMethod::Unknown);
        assert_eq!(ApiMethod::Unknown.units(), 50);
        assert_eq!(ApiMethod::Unknown.bucket(), None);
    }

    #[test]
    fn a_bucket_runs_out_on_calls_rather_than_units() {
        let mut q = QuotaState { day: "d".into(), ..Default::default() };
        for _ in 0..SEARCH_BUCKET.daily_calls {
            assert!(q.can_afford(ApiMethod::SearchList, FREE_DAILY_UNITS));
            q.charge(ApiMethod::SearchList);
        }
        // A hundred calls, and only a hundred units off the shared pool — not
        // the 10,000 that pricing them as 100-unit writes would have claimed.
        assert_eq!(q.spent, 100);
        assert!(q.remaining(FREE_DAILY_UNITS) > 9_000, "the shared pool is barely touched");
        // But the bucket is finished.
        assert!(!q.can_afford(ApiMethod::SearchList, FREE_DAILY_UNITS));
        assert!(q.bucket_is_exhausted(SEARCH_BUCKET));
        // And nothing else is affected.
        assert!(q.can_afford(ApiMethod::VideosUpdate, FREE_DAILY_UNITS));
        assert!(q.can_afford(ApiMethod::VideosInsert, FREE_DAILY_UNITS), "a different bucket");
    }

    #[test]
    fn a_refusal_on_a_bucketed_method_does_not_close_the_shared_pool() {
        let mut q = QuotaState { day: "d".into(), ..Default::default() };
        q.mark_exhausted(ApiMethod::SearchList);
        assert!(!q.exhausted, "the shared pool is untouched");
        assert!(q.bucket_is_exhausted(SEARCH_BUCKET));
        assert!(q.can_afford(ApiMethod::LiveChatMessagesInsert, FREE_DAILY_UNITS));
    }

    #[test]
    fn a_refusal_on_a_pooled_method_closes_the_day() {
        let mut q = QuotaState { day: "d".into(), ..Default::default() };
        q.mark_exhausted(ApiMethod::VideosUpdate);
        assert!(q.exhausted);
        assert!(!q.can_afford(ApiMethod::VideosList, FREE_DAILY_UNITS));
    }

    #[test]
    fn the_day_rolls_over_at_midnight_pacific_standard_time() {
        assert_eq!(quota_day(at("2026-09-20T07:59:00Z")), "2026-09-19");
        assert_eq!(quota_day(at("2026-09-20T08:00:00Z")), "2026-09-20");
        assert_eq!(quota_day(at("2026-09-20T23:00:00Z")), "2026-09-20");
    }

    #[test]
    fn a_new_day_clears_the_pool_and_every_bucket() {
        let mut q =
            QuotaState { day: "2026-09-19".into(), spent: 9_000, exhausted: true, ..Default::default() };
        q.charge(ApiMethod::SearchList);
        q.mark_exhausted(ApiMethod::SearchList);

        q.roll_over(at("2026-09-20T08:00:00Z"));
        assert_eq!(q, QuotaState { day: "2026-09-20".into(), ..Default::default() });
        assert!(!q.bucket_is_exhausted(SEARCH_BUCKET));
    }

    #[test]
    fn the_same_day_is_left_alone() {
        let mut q = QuotaState { day: "2026-09-20".into(), spent: 4_000, ..Default::default() };
        q.roll_over(at("2026-09-20T23:00:00Z"));
        assert_eq!(q.spent, 4_000);
    }

    #[test]
    fn the_reserve_covers_finishing_what_is_in_flight() {
        let mut q = QuotaState { day: "d".into(), ..Default::default() };
        q.spent = FREE_DAILY_UNITS - RESERVE_UNITS;
        assert_eq!(q.remaining(FREE_DAILY_UNITS), 0);
        assert!(!q.can_afford(ApiMethod::VideosList, FREE_DAILY_UNITS));

        // One whole metadata apply plus a chat message still fits in what is
        // held back, which is the point of holding it back.
        let apply = 3 * ApiMethod::VideosList.units()
            + ApiMethod::VideosUpdate.units()
            + ApiMethod::LiveBroadcastsUpdate.units();
        assert!(apply + ApiMethod::LiveChatMessagesInsert.units() <= RESERVE_UNITS);
        assert!(q.spent < FREE_DAILY_UNITS);
    }

    #[test]
    fn a_days_realistic_use_fits_comfortably() {
        // What a 24-hour broadcast actually spends: one metadata apply — find
        // the broadcast, update it, read the video, write it, read both back —
        // and a chat message every 20 minutes.
        let mut q = QuotaState { day: "d".into(), ..Default::default() };
        for m in [
            ApiMethod::LiveBroadcastsList,
            ApiMethod::LiveBroadcastsUpdate,
            ApiMethod::VideosList,
            ApiMethod::VideosUpdate,
            ApiMethod::VideosList,
            ApiMethod::LiveBroadcastsList,
        ] {
            q.charge(m);
        }
        for _ in 0..72 {
            q.charge(ApiMethod::LiveChatMessagesInsert);
        }
        assert_eq!(q.spent, 3_704);
        assert!(q.can_afford(ApiMethod::VideosUpdate, FREE_DAILY_UNITS), "a normal day must not run out");
        assert!(q.used_percent(FREE_DAILY_UNITS) < 50);
    }

    #[test]
    fn the_app_calls_nothing_from_a_granular_bucket() {
        // Modelled so that adding one cannot quietly mis-account, and asserted
        // here so that adding one is a deliberate change to this test.
        let source = include_str!("api.rs");
        assert!(!source.contains("/search"), "search.list has its own daily call allowance");
        assert!(!source.contains(r#""POST", "/videos"#), "videos.insert has its own daily call allowance");
    }
}
