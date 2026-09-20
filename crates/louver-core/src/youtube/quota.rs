//! Staying inside YouTube's free daily quota.
//!
//! The product rule is a cost rule: Louver Live's YouTube integration must
//! never put a bill on anyone's Google Cloud account. The Cloud side of that
//! is configuration — no billing account is linked to the project, so there is
//! nothing for Google to charge against and a request past the quota is
//! refused rather than billed (see `YOUTUBE_OAUTH_PRODUCTION.md`).
//!
//! This is the app's side of it: spend the free allowance deliberately, stop
//! before the end of it, and never keep hammering an endpoint that has already
//! said no. What it protects is not the bill — that is impossible by
//! configuration — but the *broadcast*: a run that burns the day's quota in an
//! hour leaves the rest of the day with no metadata and no chat.
//!
//! ## The numbers
//!
//! Google publishes a per-method cost table. It could not be fetched from the
//! machine this was written on (`developers.google.com` is unreachable from
//! here), so the costs below are the documented *shape* of that table — reads
//! are cheap, writes are not — applied as deliberate **upper** bounds, and
//! anything unrecognised is charged the write price. Overestimating stops the
//! app early; underestimating would let it run past the allowance and find out
//! from a 403. Only one of those two mistakes is safe, and this makes that one.
//!
//! The daily allowance itself is not guessed: [`FREE_DAILY_UNITS`] is the
//! default a new project is given, and it is the number to change if the
//! project's real allowance is ever different.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// The default daily allowance for a new YouTube Data API project.
pub const FREE_DAILY_UNITS: u32 = 10_000;

/// Stop this far short of the allowance.
///
/// The cost table here is an estimate, so the last stretch of the budget is
/// not trustworthy. Holding some back means the app stops on its own terms —
/// with a message the user can act on — rather than on a 403 partway through
/// applying a title.
pub const RESERVE_UNITS: u32 = 500;

/// A read. The cheapest thing the API does.
const READ: u32 = 1;
/// A write: insert, update or delete.
const WRITE: u32 = 50;
/// A search, which is the expensive one. Not used by this product, and priced
/// here so that adding one cannot quietly cost nothing.
const SEARCH: u32 = 100;

/// What one call costs, in quota units.
///
/// `path` is the request path or full URL; only the resource and the method
/// matter. Anything unrecognised is charged as a write, because guessing low
/// is the mistake that ends in a 403.
pub fn cost_of(method: &str, path: &str) -> u32 {
    let resource = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("");
    let resource = resource.split('?').next().unwrap_or("");
    if resource.eq_ignore_ascii_case("search") {
        return SEARCH;
    }
    match method.to_ascii_uppercase().as_str() {
        "GET" => READ,
        _ => WRITE,
    }
}

/// The day's spending, and whether Google has already said the day is over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct QuotaState {
    /// Which quota day this counts. See [`quota_day`].
    pub day: String,
    pub spent: u32,
    /// Google answered `quotaExceeded`. Nothing more is sent until the day
    /// rolls over, whatever the local count says.
    pub exhausted: bool,
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
            *self = QuotaState { day: today, spent: 0, exhausted: false };
        }
    }

    pub fn remaining(&self, cap: u32) -> u32 {
        if self.exhausted {
            return 0;
        }
        cap.saturating_sub(RESERVE_UNITS).saturating_sub(self.spent)
    }

    /// Can a call of this cost still be afforded today?
    pub fn can_afford(&self, cost: u32, cap: u32) -> bool {
        !self.exhausted && cost <= self.remaining(cap)
    }

    pub fn charge(&mut self, cost: u32) {
        self.spent = self.spent.saturating_add(cost);
    }

    /// Google's own answer, which overrides the local estimate in both
    /// directions: the estimate can be wrong, and this cannot.
    pub fn mark_exhausted(&mut self) {
        self.exhausted = true;
    }

    /// Percentage of the usable allowance spent, for the UI.
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

    /// A guard with no persistence and the default allowance, for tests.
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
    pub fn try_spend(&self, cost: u32) -> crate::error::Result<()> {
        let mut s = self.state.lock().unwrap();
        s.roll_over(Utc::now());
        if !s.can_afford(cost, self.cap) {
            return Err(crate::error::LouverError::with_detail(
                crate::error::ErrorCode::YoutubeQuotaExceeded,
                format!("오늘 사용량 {}/{} 단위", s.spent, self.cap),
            ));
        }
        s.charge(cost);
        (self.persist)(&s);
        Ok(())
    }

    /// Google said the day is over. Believe it over the local estimate.
    pub fn note_exhausted(&self) {
        let mut s = self.state.lock().unwrap();
        s.roll_over(Utc::now());
        s.mark_exhausted();
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
        self.quota.try_spend(cost_of(method, url))?;
        let answered = self.inner.request(method, url, bearer, body);
        if let Ok((status, text)) = &answered {
            // Google's own refusal latches the day shut, however much the
            // local estimate thought was left.
            if !(200..300).contains(status)
                && crate::youtube::api::classify(*status, text).code
                    == crate::error::ErrorCode::YoutubeQuotaExceeded
            {
                self.quota.note_exhausted();
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

    #[test]
    fn reads_are_cheap_and_writes_are_not() {
        assert_eq!(cost_of("GET", "/liveBroadcasts?part=id"), 1);
        assert_eq!(cost_of("GET", "/videos?part=snippet&id=x"), 1);
        assert_eq!(cost_of("PUT", "/videos?part=snippet"), 50);
        assert_eq!(cost_of("POST", "/liveChat/messages?part=snippet"), 50);
    }

    #[test]
    fn an_unknown_call_is_charged_as_a_write() {
        // Guessing low is the mistake that ends in a 403 mid-broadcast.
        assert_eq!(cost_of("PATCH", "/somethingNew"), 50);
        assert_eq!(cost_of("DELETE", "/playlistItems"), 50);
    }

    #[test]
    fn a_search_is_priced_as_the_expensive_call_it_is() {
        // Nothing here calls it. It is priced so that adding one cannot
        // quietly be free.
        assert_eq!(cost_of("GET", "https://www.googleapis.com/youtube/v3/search?q=x"), 100);
    }

    #[test]
    fn the_day_rolls_over_at_midnight_pacific_standard_time() {
        assert_eq!(quota_day(at("2026-09-20T07:59:00Z")), "2026-09-19");
        assert_eq!(quota_day(at("2026-09-20T08:00:00Z")), "2026-09-20");
        assert_eq!(quota_day(at("2026-09-20T23:00:00Z")), "2026-09-20");
    }

    #[test]
    fn a_new_day_starts_the_count_and_clears_the_refusal() {
        let mut q = QuotaState { day: "2026-09-19".into(), spent: 9_000, exhausted: true };
        q.roll_over(at("2026-09-20T08:00:00Z"));
        assert_eq!(q, QuotaState { day: "2026-09-20".into(), spent: 0, exhausted: false });
    }

    #[test]
    fn the_same_day_is_left_alone() {
        let mut q = QuotaState { day: "2026-09-20".into(), spent: 4_000, exhausted: false };
        q.roll_over(at("2026-09-20T23:00:00Z"));
        assert_eq!(q.spent, 4_000);
    }

    #[test]
    fn the_reserve_is_never_spent() {
        let mut q = QuotaState { day: "d".into(), spent: 0, exhausted: false };
        q.charge(FREE_DAILY_UNITS - RESERVE_UNITS);
        assert_eq!(q.remaining(FREE_DAILY_UNITS), 0);
        assert!(!q.can_afford(1, FREE_DAILY_UNITS));
        // The reserve exists because the cost table above is an estimate; the
        // app stops on its own terms rather than on a 403 mid-apply.
        assert!(q.spent < FREE_DAILY_UNITS);
    }

    #[test]
    fn googles_answer_beats_the_local_estimate() {
        let mut q = QuotaState { day: "d".into(), spent: 10, exhausted: false };
        assert!(q.can_afford(50, FREE_DAILY_UNITS));
        q.mark_exhausted();
        assert!(!q.can_afford(1, FREE_DAILY_UNITS));
        assert_eq!(q.remaining(FREE_DAILY_UNITS), 0);
        assert_eq!(q.used_percent(FREE_DAILY_UNITS), 100);
    }

    #[test]
    fn a_days_realistic_use_fits_comfortably() {
        // What a 24-hour broadcast actually spends: one metadata apply (two
        // reads, two writes, two verification reads) and a chat message every
        // 20 minutes.
        let mut q = QuotaState { day: "d".into(), spent: 0, exhausted: false };
        for (m, p) in [
            ("GET", "/liveBroadcasts"),
            ("PUT", "/liveBroadcasts"),
            ("GET", "/videos"),
            ("PUT", "/videos"),
            ("GET", "/videos"),
            ("GET", "/liveBroadcasts"),
        ] {
            q.charge(cost_of(m, p));
        }
        for _ in 0..72 {
            q.charge(cost_of("POST", "/liveChat/messages"));
        }
        assert_eq!(q.spent, 3_704);
        assert!(q.can_afford(WRITE, FREE_DAILY_UNITS), "a normal day must not run out");
        assert!(q.used_percent(FREE_DAILY_UNITS) < 50);
    }
}
