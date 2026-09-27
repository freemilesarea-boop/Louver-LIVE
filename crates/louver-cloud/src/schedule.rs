//! When a scheduled broadcast should start, and when it should stop. §8.
//!
//! All of the deciding is in [`decide`], which is a pure function of the stored
//! schedule and the current instant. That is the whole design: nothing about a
//! due time is held in memory, so a server that restarts inside a scheduled
//! window reaches the same conclusion it did a minute earlier, and a schedule
//! recovers with no recovery code.
//!
//! Times are stored as UTC. `offset_minutes` says which local day an instant
//! falls on, so a daily repeat means the same clock time to the user without
//! this service carrying a timezone database.

use crate::models::{DesiredState, Schedule};
use crate::{CloudError, Result};
use chrono::{DateTime, Datelike, Duration, FixedOffset, TimeZone, Utc};

/// How late a missed start may still be honoured.
///
/// A server that was down for an hour should pick the broadcast back up. One
/// that was down for a week should not suddenly start last Tuesday's stream.
pub const CATCH_UP: Duration = Duration::hours(6);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleAction {
    /// Start now, on account of this occurrence. The instant is recorded so the
    /// same window cannot start twice.
    Start { occurrence: DateTime<Utc> },
    /// The window is over.
    Stop,
}

pub fn validate(s: &Schedule) -> Result<()> {
    if !s.enabled {
        return Ok(());
    }
    let start = s
        .start_at
        .as_deref()
        .and_then(parse)
        .ok_or_else(|| CloudError::Invalid("시작 시각을 입력해 주세요".into()))?;
    if let Some(stop) = s.stop_at.as_deref() {
        let stop = parse(stop).ok_or_else(|| CloudError::Invalid("종료 시각을 확인해 주세요".into()))?;
        if stop <= start {
            return Err(CloudError::Invalid("종료 시각은 시작 시각보다 뒤여야 합니다".into()));
        }
    }
    if !(0..=0b111_1111).contains(&s.repeat_days) {
        return Err(CloudError::Invalid("반복 요일 값이 올바르지 않습니다".into()));
    }
    if !(-14 * 60..=14 * 60).contains(&s.offset_minutes) {
        return Err(CloudError::Invalid("시간대 오프셋이 올바르지 않습니다".into()));
    }
    Ok(())
}

/// What this schedule asks for right now, if anything.
pub fn decide(s: &Schedule, desired: DesiredState, now: DateTime<Utc>) -> Option<ScheduleAction> {
    if !s.enabled {
        return None;
    }
    let start = parse(s.start_at.as_deref()?)?;
    let last_run = s.last_run_at.as_deref().and_then(parse);

    if desired == DesiredState::Running {
        // Only a schedule with an end stops anything, and only the occurrence
        // that is actually playing.
        let stop_at = parse(s.stop_at.as_deref()?)?;
        let window = stop_at - start;
        let ran_at = last_run?;
        return (now >= ran_at + window).then_some(ScheduleAction::Stop);
    }

    let occurrence = next_occurrence(s, start, now)?;
    // Already run, or not yet due, or so late that starting would surprise
    // whoever set it.
    if last_run.is_some_and(|r| r >= occurrence) || occurrence > now || now - occurrence > CATCH_UP {
        return None;
    }
    // A window that has already ended must not start late.
    if let Some(stop_at) = s.stop_at.as_deref().and_then(parse) {
        if now >= occurrence + (stop_at - start) {
            return None;
        }
    }
    Some(ScheduleAction::Start { occurrence })
}

/// The most recent occurrence at or before `now`.
///
/// For a one-shot that is the stored instant. For a repeat it is the same local
/// clock time on the most recent enabled day, which is why the offset is stored
/// alongside the instant.
fn next_occurrence(s: &Schedule, start: DateTime<Utc>, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if s.repeat_days == 0 {
        return Some(start);
    }
    let zone = FixedOffset::east_opt((s.offset_minutes * 60) as i32)?;
    let local_start = start.with_timezone(&zone);
    let time_of_day = local_start.time();

    // Today, then backwards. Eight days covers a week plus the day the
    // catch-up window can reach into.
    for back in 0..8 {
        let day = now.with_timezone(&zone).date_naive() - Duration::days(back);
        if !day_enabled(s.repeat_days, day.weekday().num_days_from_monday() as i64) {
            continue;
        }
        let candidate = zone.from_local_datetime(&day.and_time(time_of_day)).single()?.with_timezone(&Utc);
        if candidate <= now && candidate >= start {
            return Some(candidate);
        }
    }
    None
}

/// Monday is bit 0, matching the desktop's own day mask.
pub fn day_enabled(mask: i64, day_from_monday: i64) -> bool {
    mask & (1 << day_from_monday) != 0
}

fn parse(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn once(start: &str) -> Schedule {
        Schedule { enabled: true, start_at: Some(start.into()), ..Default::default() }
    }

    #[test]
    fn a_one_shot_starts_once_and_never_again() {
        let s = once("2026-10-01T12:00:00Z");
        assert_eq!(decide(&s, DesiredState::Stopped, at("2026-10-01T11:59:00Z")), None, "too early");
        assert_eq!(
            decide(&s, DesiredState::Stopped, at("2026-10-01T12:00:30Z")),
            Some(ScheduleAction::Start { occurrence: at("2026-10-01T12:00:00Z") })
        );

        // Once it has run, the same instant never comes due again.
        let ran = Schedule { last_run_at: Some("2026-10-01T12:00:00Z".into()), ..s.clone() };
        assert_eq!(decide(&ran, DesiredState::Stopped, at("2026-10-01T12:05:00Z")), None);

        // And a start missed by a week is not started a week late.
        assert_eq!(decide(&s, DesiredState::Stopped, at("2026-10-08T12:00:00Z")), None);
        // An hour late is still honoured — that is a server that was rebooting.
        assert!(decide(&s, DesiredState::Stopped, at("2026-10-01T13:00:00Z")).is_some());
    }

    #[test]
    fn a_disabled_schedule_asks_for_nothing() {
        let s = Schedule { enabled: false, ..once("2026-10-01T12:00:00Z") };
        assert_eq!(decide(&s, DesiredState::Stopped, at("2026-10-01T12:01:00Z")), None);
    }

    #[test]
    fn a_daily_repeat_comes_due_at_the_same_local_time() {
        // 21:00 in Seoul (UTC+9) is 12:00 UTC.
        let s = Schedule {
            enabled: true,
            start_at: Some("2026-10-01T12:00:00Z".into()),
            timezone: "Asia/Seoul".into(),
            offset_minutes: 540,
            repeat_days: 0b111_1111,
            ..Default::default()
        };

        // Two days later, one minute after 21:00 Seoul time.
        let got = decide(&s, DesiredState::Stopped, at("2026-10-03T12:01:00Z"));
        assert_eq!(got, Some(ScheduleAction::Start { occurrence: at("2026-10-03T12:00:00Z") }));

        // Having run today, it waits for tomorrow rather than restarting now.
        let ran = Schedule { last_run_at: Some("2026-10-03T12:00:00Z".into()), ..s.clone() };
        assert_eq!(decide(&ran, DesiredState::Stopped, at("2026-10-03T12:30:00Z")), None);
        assert_eq!(
            decide(&ran, DesiredState::Stopped, at("2026-10-04T12:00:10Z")),
            Some(ScheduleAction::Start { occurrence: at("2026-10-04T12:00:00Z") })
        );
    }

    #[test]
    fn weekdays_only_skips_the_weekend() {
        // 2026-10-03 is a Saturday, 2026-10-05 a Monday.
        let s = Schedule {
            enabled: true,
            start_at: Some("2026-10-01T09:00:00Z".into()),
            repeat_days: 0b001_1111,
            ..Default::default()
        };
        // Saturday at 09:01: the most recent enabled day is Friday, more than
        // the catch-up window ago, so nothing is due.
        assert_eq!(decide(&s, DesiredState::Stopped, at("2026-10-03T09:01:00Z")), None);
        // Monday is due.
        assert_eq!(
            decide(&s, DesiredState::Stopped, at("2026-10-05T09:00:20Z")),
            Some(ScheduleAction::Start { occurrence: at("2026-10-05T09:00:00Z") })
        );
    }

    #[test]
    fn a_window_that_has_ended_does_not_start_late() {
        let s = Schedule {
            enabled: true,
            start_at: Some("2026-10-01T12:00:00Z".into()),
            stop_at: Some("2026-10-01T14:00:00Z".into()),
            ..Default::default()
        };
        // Inside the window: start.
        assert!(decide(&s, DesiredState::Stopped, at("2026-10-01T13:00:00Z")).is_some());
        // After it: nothing, even though it is within the catch-up window.
        assert_eq!(decide(&s, DesiredState::Stopped, at("2026-10-01T14:30:00Z")), None);
    }

    #[test]
    fn a_running_broadcast_stops_at_the_end_of_its_own_occurrence() {
        let s = Schedule {
            enabled: true,
            start_at: Some("2026-10-01T12:00:00Z".into()),
            stop_at: Some("2026-10-01T14:00:00Z".into()),
            repeat_days: 0b111_1111,
            last_run_at: Some("2026-10-05T12:00:00Z".into()),
            ..Default::default()
        };
        // Two hours into the occurrence that is actually playing, not the one
        // the stored stop_at names.
        assert_eq!(decide(&s, DesiredState::Running, at("2026-10-05T13:59:00Z")), None);
        assert_eq!(decide(&s, DesiredState::Running, at("2026-10-05T14:00:00Z")), Some(ScheduleAction::Stop));
    }

    #[test]
    fn a_schedule_without_an_end_never_stops_itself() {
        let s = Schedule { last_run_at: Some("2026-10-01T12:00:00Z".into()), ..once("2026-10-01T12:00:00Z") };
        assert_eq!(decide(&s, DesiredState::Running, at("2026-12-25T00:00:00Z")), None);
    }

    #[test]
    fn validation_catches_the_mistakes_a_form_can_make() {
        assert!(validate(&Schedule::default()).is_ok(), "a disabled schedule needs nothing");
        assert!(validate(&Schedule { enabled: true, ..Default::default() }).is_err(), "no start");
        assert!(validate(&once("not a time")).is_err());
        let backwards =
            Schedule { stop_at: Some("2026-10-01T11:00:00Z".into()), ..once("2026-10-01T12:00:00Z") };
        assert!(validate(&backwards).is_err(), "stop before start");
        let bad_days = Schedule { repeat_days: 999, ..once("2026-10-01T12:00:00Z") };
        assert!(validate(&bad_days).is_err());
    }
}
