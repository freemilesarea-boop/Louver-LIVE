//! Broadcast scheduling (§19, §20).
//!
//! Schedules are wall-clock local times with a day-of-week mask. A schedule
//! whose end time is at or before its start time runs past midnight, so
//! `20:00 → 02:00` means "start Monday 20:00, stop Tuesday 02:00" and the
//! *start* day is the one the mask selects.
//!
//! Every decision goes through [`Scheduler::evaluate`], which takes the current
//! instant from an injected [`Clock`] so tests never wait (§57).

use crate::clock::Clock;
use crate::database::models::{DaysOfWeek, Schedule};
use crate::error::{ErrorCode, LouverError, Result};
use chrono::{Datelike, Duration, NaiveDateTime, NaiveTime, Timelike};
use serde::{Deserialize, Serialize};

/// Parse `"HH:MM"` (also accepts `"HH:MM:SS"`).
pub fn parse_time(s: &str) -> Result<NaiveTime> {
    NaiveTime::parse_from_str(s, "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(s, "%H:%M:%S"))
        .map_err(|_| LouverError::with_detail(ErrorCode::ScheduleInvalidTime, s))
}

pub fn validate(s: &Schedule) -> Result<()> {
    if s.days_of_week.is_empty() {
        return Err(LouverError::new(ErrorCode::ScheduleNoDays));
    }
    let (start, end) = (parse_time(&s.start_time)?, parse_time(&s.end_time)?);
    if start == end {
        return Err(LouverError::with_detail(ErrorCode::ScheduleInvalidTime, "start and end are identical"));
    }
    Ok(())
}

/// True when the schedule runs past midnight.
pub fn crosses_midnight(start: NaiveTime, end: NaiveTime) -> bool {
    end <= start
}

/// One concrete run of a schedule, resolved to absolute local datetimes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Occurrence {
    pub schedule_id: i64,
    pub playlist_id: i64,
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
}

impl Occurrence {
    pub fn contains(&self, t: NaiveDateTime) -> bool {
        t >= self.start && t < self.end
    }
    pub fn duration_secs(&self) -> i64 {
        (self.end - self.start).num_seconds()
    }
}

/// Build the occurrence that *starts* on `date`, if the mask selects that day.
fn occurrence_starting_on(
    s: &Schedule,
    date: chrono::NaiveDate,
    start: NaiveTime,
    end: NaiveTime,
) -> Option<Occurrence> {
    if !s.days_of_week.contains_index(date.weekday().num_days_from_monday()) {
        return None;
    }
    let start_dt = date.and_time(start);
    let end_dt = if crosses_midnight(start, end) {
        // Runs into the following day.
        date.succ_opt()?.and_time(end)
    } else {
        date.and_time(end)
    };
    Some(Occurrence { schedule_id: s.id, playlist_id: s.playlist_id, start: start_dt, end: end_dt })
}

/// The occurrence covering `now`, if any.
///
/// Both today's and yesterday's occurrence are considered: at 01:00 on Tuesday
/// the live broadcast belongs to Monday's `20:00 → 02:00` entry (§19).
pub fn active_occurrence(s: &Schedule, now: NaiveDateTime) -> Option<Occurrence> {
    if !s.enabled {
        return None;
    }
    let (start, end) = (parse_time(&s.start_time).ok()?, parse_time(&s.end_time).ok()?);
    let today = now.date();
    [today.pred_opt()?, today]
        .into_iter()
        .filter_map(|d| occurrence_starting_on(s, d, start, end))
        .find(|o| o.contains(now))
}

/// The next occurrence that begins strictly after `now`, searching two weeks.
pub fn next_occurrence(s: &Schedule, now: NaiveDateTime) -> Option<Occurrence> {
    if !s.enabled {
        return None;
    }
    let (start, end) = (parse_time(&s.start_time).ok()?, parse_time(&s.end_time).ok()?);
    (0..15)
        .filter_map(|i| {
            let d = now.date().checked_add_signed(Duration::days(i))?;
            occurrence_starting_on(s, d, start, end)
        })
        .find(|o| o.start > now)
}

/// What the scheduler wants the app to do right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScheduleDecision {
    /// Nothing scheduled; stay off.
    Idle { next: Option<Occurrence> },
    /// Inside a window — start (or keep) broadcasting this playlist.
    ShouldBroadcast { occurrence: Occurrence },
    /// A broadcast is running but its window has ended.
    ShouldStop { occurrence: Occurrence },
}

/// Evaluates schedules against an injected clock.
pub struct Scheduler<C: Clock> {
    clock: C,
}

impl<C: Clock> Scheduler<C> {
    pub fn new(clock: C) -> Self {
        Self { clock }
    }

    pub fn clock(&self) -> &C {
        &self.clock
    }

    pub fn now(&self) -> NaiveDateTime {
        self.clock.now_local()
    }

    /// Decide what should happen, given the schedules and which occurrence (if
    /// any) the running broadcast belongs to.
    ///
    /// `running` is the occurrence the current broadcast was started for; pass
    /// `None` when nothing is live. A manual broadcast is not the scheduler's
    /// business and should not be passed here.
    pub fn evaluate(&self, schedules: &[Schedule], running: Option<&Occurrence>) -> ScheduleDecision {
        let now = self.now();

        // Prefer the occurrence we are already running, so a schedule edited
        // mid-broadcast does not cause a needless restart.
        let active = running
            .filter(|o| o.contains(now))
            .cloned()
            .or_else(|| schedules.iter().filter_map(|s| active_occurrence(s, now)).min_by_key(|o| o.start));

        match (active, running) {
            (Some(o), _) => ScheduleDecision::ShouldBroadcast { occurrence: o },
            (None, Some(r)) => ScheduleDecision::ShouldStop { occurrence: r.clone() },
            (None, None) => ScheduleDecision::Idle {
                next: schedules.iter().filter_map(|s| next_occurrence(s, now)).min_by_key(|o| o.start),
            },
        }
    }

    /// Startup recovery: if the app launches inside a window, broadcast at once
    /// (§20). This is the same evaluation with no running session, named
    /// separately because it is the behaviour §20 calls out as critical.
    pub fn recover_on_startup(&self, schedules: &[Schedule]) -> Option<Occurrence> {
        match self.evaluate(schedules, None) {
            ScheduleDecision::ShouldBroadcast { occurrence } => Some(occurrence),
            _ => None,
        }
    }

    /// Seconds until the running occurrence ends, for the dashboard countdown.
    pub fn seconds_until_end(&self, o: &Occurrence) -> i64 {
        (o.end - self.now()).num_seconds().max(0)
    }
}

/// Render a day mask as the Korean weekday letters the UI shows (§27).
pub fn format_days(d: DaysOfWeek) -> String {
    if d == DaysOfWeek::everyday() {
        return "매일".into();
    }
    if d == DaysOfWeek::weekdays() {
        return "월~금".into();
    }
    const NAMES: [&str; 7] = ["월", "화", "수", "목", "금", "토", "일"];
    let v: Vec<&str> = (0..7).filter(|i| d.contains_index(*i)).map(|i| NAMES[i as usize]).collect();
    if v.is_empty() {
        "없음".into()
    } else {
        v.join(" ")
    }
}

/// Human-readable window length, handling the midnight case.
pub fn window_duration_secs(start: NaiveTime, end: NaiveTime) -> i64 {
    let s = start.num_seconds_from_midnight() as i64;
    let e = end.num_seconds_from_midnight() as i64;
    if crosses_midnight(start, end) {
        86_400 - s + e
    } else {
        e - s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::TestClock;

    fn sched(days: DaysOfWeek, start: &str, end: &str) -> Schedule {
        Schedule {
            id: 1,
            playlist_id: 7,
            days_of_week: days,
            start_time: start.into(),
            end_time: end.into(),
            enabled: true,
        }
    }

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    // 2026-03-02 is a Monday.
    const MON: &str = "2026-03-02";
    const TUE: &str = "2026-03-03";
    const SAT: &str = "2026-03-07";
    const SUN: &str = "2026-03-08";

    #[test]
    fn weekday_indices_match_the_calendar_we_assume() {
        assert_eq!(at(&format!("{MON} 00:00:00")).weekday().num_days_from_monday(), 0);
        assert_eq!(at(&format!("{SAT} 00:00:00")).weekday().num_days_from_monday(), 5);
        assert_eq!(at(&format!("{SUN} 00:00:00")).weekday().num_days_from_monday(), 6);
    }

    // --- ordinary same-day window ----------------------------------------

    #[test]
    fn inside_a_normal_window() {
        let s = sched(DaysOfWeek::weekdays(), "09:00", "18:00");
        let o = active_occurrence(&s, at(&format!("{MON} 12:10:00"))).expect("should be active");
        assert_eq!(o.start, at(&format!("{MON} 09:00:00")));
        assert_eq!(o.end, at(&format!("{MON} 18:00:00")));
        assert_eq!(o.playlist_id, 7);
    }

    #[test]
    fn outside_a_normal_window() {
        let s = sched(DaysOfWeek::weekdays(), "09:00", "18:00");
        assert!(active_occurrence(&s, at(&format!("{MON} 08:59:59"))).is_none());
        assert!(active_occurrence(&s, at(&format!("{MON} 18:00:00"))).is_none(), "end is exclusive");
        assert!(active_occurrence(&s, at(&format!("{MON} 23:00:00"))).is_none());
    }

    #[test]
    fn boundaries_are_start_inclusive_end_exclusive() {
        let s = sched(DaysOfWeek::everyday(), "09:00", "18:00");
        assert!(active_occurrence(&s, at(&format!("{MON} 09:00:00"))).is_some());
        assert!(active_occurrence(&s, at(&format!("{MON} 17:59:59"))).is_some());
        assert!(active_occurrence(&s, at(&format!("{MON} 18:00:00"))).is_none());
    }

    // --- midnight crossing (§19) ------------------------------------------

    #[test]
    fn a_window_crossing_midnight_is_active_before_and_after_midnight() {
        let s = sched(DaysOfWeek::weekdays(), "20:00", "02:00");
        // Monday evening — belongs to Monday's occurrence.
        let o = active_occurrence(&s, at(&format!("{MON} 21:30:00"))).unwrap();
        assert_eq!(o.start, at(&format!("{MON} 20:00:00")));
        assert_eq!(o.end, at(&format!("{TUE} 02:00:00")));

        // Tuesday 01:00 — still Monday's occurrence, not a new one.
        let o2 = active_occurrence(&s, at(&format!("{TUE} 01:00:00"))).unwrap();
        assert_eq!(o2.start, at(&format!("{MON} 20:00:00")));
        assert_eq!(o2.end, at(&format!("{TUE} 02:00:00")));
    }

    #[test]
    fn a_midnight_window_ends_on_time_the_next_morning() {
        let s = sched(DaysOfWeek::weekdays(), "20:00", "02:00");
        assert!(active_occurrence(&s, at(&format!("{TUE} 01:59:59"))).is_some());
        assert!(active_occurrence(&s, at(&format!("{TUE} 02:00:00"))).is_none());
        assert!(active_occurrence(&s, at(&format!("{TUE} 08:00:00"))).is_none());
    }

    #[test]
    fn the_start_day_owns_the_window_so_saturday_morning_runs_from_friday() {
        // Weekdays-only 20:00→02:00: Saturday 01:00 is Friday's tail and must run.
        let s = sched(DaysOfWeek::weekdays(), "20:00", "02:00");
        assert!(
            active_occurrence(&s, at(&format!("{SAT} 01:00:00"))).is_some(),
            "friday night's broadcast must continue into saturday"
        );
        // But Saturday evening itself is not scheduled.
        assert!(active_occurrence(&s, at(&format!("{SAT} 21:00:00"))).is_none());
    }

    #[test]
    fn the_20_to_08_success_scenario_from_the_spec() {
        // §64: 20:00 → 08:00, every day.
        let s = sched(DaysOfWeek::everyday(), "20:00", "08:00");
        assert!(active_occurrence(&s, at(&format!("{MON} 20:00:00"))).is_some());
        assert!(active_occurrence(&s, at(&format!("{TUE} 03:00:00"))).is_some());
        assert!(active_occurrence(&s, at(&format!("{TUE} 07:59:59"))).is_some());
        assert!(active_occurrence(&s, at(&format!("{TUE} 08:00:00"))).is_none());
        assert!(active_occurrence(&s, at(&format!("{TUE} 12:00:00"))).is_none());
        // And it starts again the same evening.
        let next = next_occurrence(&s, at(&format!("{TUE} 12:00:00"))).unwrap();
        assert_eq!(next.start, at(&format!("{TUE} 20:00:00")));
    }

    // --- day-of-week handling ---------------------------------------------

    #[test]
    fn a_day_not_in_the_mask_never_starts() {
        let s = sched(DaysOfWeek::weekdays(), "09:00", "18:00");
        assert!(active_occurrence(&s, at(&format!("{SAT} 12:00:00"))).is_none());
        assert!(active_occurrence(&s, at(&format!("{SUN} 12:00:00"))).is_none());
    }

    #[test]
    fn weekend_only_schedule() {
        let s = sched(DaysOfWeek(DaysOfWeek::SATURDAY | DaysOfWeek::SUNDAY), "10:00", "22:00");
        assert!(active_occurrence(&s, at(&format!("{SAT} 12:00:00"))).is_some());
        assert!(active_occurrence(&s, at(&format!("{SUN} 12:00:00"))).is_some());
        assert!(active_occurrence(&s, at(&format!("{MON} 12:00:00"))).is_none());
    }

    #[test]
    fn a_disabled_schedule_is_inert() {
        let mut s = sched(DaysOfWeek::everyday(), "00:00", "23:59");
        s.enabled = false;
        assert!(active_occurrence(&s, at(&format!("{MON} 12:00:00"))).is_none());
        assert!(next_occurrence(&s, at(&format!("{MON} 12:00:00"))).is_none());
    }

    // --- next occurrence --------------------------------------------------

    #[test]
    fn next_occurrence_finds_tomorrow_and_skips_the_weekend() {
        let s = sched(DaysOfWeek::weekdays(), "09:00", "18:00");
        let n = next_occurrence(&s, at(&format!("{MON} 19:00:00"))).unwrap();
        assert_eq!(n.start, at(&format!("{TUE} 09:00:00")));

        // Friday evening -> next is Monday.
        let n = next_occurrence(&s, at("2026-03-06 19:00:00")).unwrap();
        assert_eq!(n.start, at("2026-03-09 09:00:00"));
    }

    #[test]
    fn next_occurrence_is_strictly_in_the_future() {
        let s = sched(DaysOfWeek::everyday(), "09:00", "18:00");
        let n = next_occurrence(&s, at(&format!("{MON} 09:00:00"))).unwrap();
        assert_eq!(n.start, at(&format!("{TUE} 09:00:00")), "must not return the current window");
    }

    // --- startup recovery (§20) -------------------------------------------

    #[test]
    fn app_launched_inside_a_window_recovers_immediately() {
        // §20's worked example: 09:00–18:00, PC boots at 12:10.
        let clock = TestClock::parse(&format!("{MON} 12:10:00"));
        let sc = Scheduler::new(clock);
        let s = vec![sched(DaysOfWeek::weekdays(), "09:00", "18:00")];
        let o = sc.recover_on_startup(&s).expect("must resume at 12:10");
        assert_eq!(o.end, at(&format!("{MON} 18:00:00")));
        assert_eq!(sc.seconds_until_end(&o), 5 * 3600 + 50 * 60);
    }

    #[test]
    fn app_launched_outside_a_window_stays_off() {
        let sc = Scheduler::new(TestClock::parse(&format!("{MON} 19:30:00")));
        let s = vec![sched(DaysOfWeek::weekdays(), "09:00", "18:00")];
        assert!(sc.recover_on_startup(&s).is_none(), "must not broadcast outside the window");
    }

    #[test]
    fn a_pc_rebooted_after_midnight_resumes_the_previous_evenings_broadcast() {
        // §64: PC reboots at 03:00 during a 20:00→08:00 window.
        let sc = Scheduler::new(TestClock::parse(&format!("{TUE} 03:00:00")));
        let s = vec![sched(DaysOfWeek::everyday(), "20:00", "08:00")];
        let o = sc.recover_on_startup(&s).expect("must resume after a reboot");
        assert_eq!(o.start, at(&format!("{MON} 20:00:00")));
        assert_eq!(o.end, at(&format!("{TUE} 08:00:00")));
    }

    // --- decisions --------------------------------------------------------

    #[test]
    fn decision_transitions_across_a_window_without_sleeping() {
        let clock = TestClock::parse(&format!("{MON} 08:59:00"));
        let sc = Scheduler::new(clock.clone());
        let s = vec![sched(DaysOfWeek::weekdays(), "09:00", "18:00")];

        // Before: idle, with the next start known.
        match sc.evaluate(&s, None) {
            ScheduleDecision::Idle { next } => {
                assert_eq!(next.unwrap().start, at(&format!("{MON} 09:00:00")))
            }
            d => panic!("expected Idle, got {d:?}"),
        }

        // Start time arrives.
        clock.advance_minutes(1);
        let running = match sc.evaluate(&s, None) {
            ScheduleDecision::ShouldBroadcast { occurrence } => occurrence,
            d => panic!("expected ShouldBroadcast, got {d:?}"),
        };

        // Mid-window it keeps running.
        clock.advance_minutes(300);
        assert!(matches!(sc.evaluate(&s, Some(&running)), ScheduleDecision::ShouldBroadcast { .. }));

        // End time: stop.
        clock.set_str(&format!("{MON} 18:00:00"));
        match sc.evaluate(&s, Some(&running)) {
            ScheduleDecision::ShouldStop { occurrence } => assert_eq!(occurrence.start, running.start),
            d => panic!("expected ShouldStop, got {d:?}"),
        }
    }

    #[test]
    fn the_e2e_schedule_scenario_runs_on_a_virtual_clock() {
        // §57: start at now+1min, stop at now+3min — with no real waiting.
        let clock = TestClock::parse(&format!("{MON} 10:00:00"));
        let sc = Scheduler::new(clock.clone());
        let s = vec![sched(DaysOfWeek::everyday(), "10:01", "10:03")];

        assert!(matches!(sc.evaluate(&s, None), ScheduleDecision::Idle { .. }));
        clock.set_str(&format!("{MON} 10:01:00"));
        let o = match sc.evaluate(&s, None) {
            ScheduleDecision::ShouldBroadcast { occurrence } => occurrence,
            d => panic!("{d:?}"),
        };
        clock.set_str(&format!("{MON} 10:02:30"));
        assert!(matches!(sc.evaluate(&s, Some(&o)), ScheduleDecision::ShouldBroadcast { .. }));
        clock.set_str(&format!("{MON} 10:03:00"));
        assert!(matches!(sc.evaluate(&s, Some(&o)), ScheduleDecision::ShouldStop { .. }));
    }

    #[test]
    fn overlapping_schedules_pick_the_earliest_start() {
        let clock = TestClock::parse(&format!("{MON} 12:00:00"));
        let sc = Scheduler::new(clock);
        let mut a = sched(DaysOfWeek::everyday(), "09:00", "18:00");
        a.id = 1;
        a.playlist_id = 100;
        let mut b = sched(DaysOfWeek::everyday(), "11:00", "13:00");
        b.id = 2;
        b.playlist_id = 200;
        match sc.evaluate(&[b, a], None) {
            ScheduleDecision::ShouldBroadcast { occurrence } => {
                assert_eq!(occurrence.playlist_id, 100, "the earlier window wins");
            }
            d => panic!("{d:?}"),
        }
    }

    #[test]
    fn no_schedules_means_idle_with_no_next() {
        let sc = Scheduler::new(TestClock::parse(&format!("{MON} 12:00:00")));
        assert_eq!(sc.evaluate(&[], None), ScheduleDecision::Idle { next: None });
    }

    // --- validation and formatting ----------------------------------------

    #[test]
    fn validation_rejects_empty_days_and_bad_times() {
        assert_eq!(
            validate(&sched(DaysOfWeek(0), "09:00", "18:00")).unwrap_err().code,
            ErrorCode::ScheduleNoDays
        );
        assert_eq!(
            validate(&sched(DaysOfWeek::everyday(), "25:00", "18:00")).unwrap_err().code,
            ErrorCode::ScheduleInvalidTime
        );
        assert_eq!(
            validate(&sched(DaysOfWeek::everyday(), "09:00", "09:00")).unwrap_err().code,
            ErrorCode::ScheduleInvalidTime
        );
        validate(&sched(DaysOfWeek::everyday(), "20:00", "02:00")).unwrap();
    }

    #[test]
    fn time_parsing_accepts_both_forms() {
        assert_eq!(parse_time("09:05").unwrap(), NaiveTime::from_hms_opt(9, 5, 0).unwrap());
        assert_eq!(parse_time("09:05:30").unwrap(), NaiveTime::from_hms_opt(9, 5, 30).unwrap());
        assert!(parse_time("9am").is_err());
    }

    #[test]
    fn window_length_handles_midnight() {
        let t = |h, m| NaiveTime::from_hms_opt(h, m, 0).unwrap();
        assert_eq!(window_duration_secs(t(9, 0), t(18, 0)), 9 * 3600);
        assert_eq!(window_duration_secs(t(20, 0), t(2, 0)), 6 * 3600);
        assert_eq!(window_duration_secs(t(20, 0), t(8, 0)), 12 * 3600);
    }

    #[test]
    fn day_masks_format_for_the_ui() {
        assert_eq!(format_days(DaysOfWeek::everyday()), "매일");
        assert_eq!(format_days(DaysOfWeek::weekdays()), "월~금");
        assert_eq!(format_days(DaysOfWeek(DaysOfWeek::SATURDAY | DaysOfWeek::SUNDAY)), "토 일");
        assert_eq!(format_days(DaysOfWeek(0)), "없음");
    }

    #[test]
    fn occurrence_duration_is_correct_across_midnight() {
        let s = sched(DaysOfWeek::everyday(), "20:00", "08:00");
        let o = active_occurrence(&s, at(&format!("{MON} 22:00:00"))).unwrap();
        assert_eq!(o.duration_secs(), 12 * 3600);
    }
}
