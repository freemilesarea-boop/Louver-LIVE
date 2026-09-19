//! Injectable clock so scheduler tests never sleep (§57).

use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use std::sync::{Arc, Mutex};

pub trait Clock: Send + Sync + std::fmt::Debug {
    /// Current wall-clock time in UTC.
    fn now_utc(&self) -> DateTime<Utc>;

    /// Current local time, which is what schedules are expressed in.
    fn now_local(&self) -> NaiveDateTime {
        Local
            .from_utc_datetime(&self.now_utc().naive_utc())
            .naive_local()
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_utc(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A clock the tests drive by hand. Schedules are evaluated in local time, so
/// `TestClock` stores a naive local instant and converts back to UTC.
#[derive(Debug, Clone)]
pub struct TestClock {
    local: Arc<Mutex<NaiveDateTime>>,
}

impl TestClock {
    pub fn at(local: NaiveDateTime) -> Self {
        Self { local: Arc::new(Mutex::new(local)) }
    }

    /// Parse `"2026-01-05 09:30:00"` as local time.
    pub fn parse(s: &str) -> Self {
        Self::at(
            NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
                .expect("TestClock::parse expects '%Y-%m-%d %H:%M:%S'"),
        )
    }

    pub fn set(&self, local: NaiveDateTime) {
        *self.local.lock().unwrap() = local;
    }

    pub fn set_str(&self, s: &str) {
        self.set(NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").expect("bad time"));
    }

    pub fn advance_minutes(&self, m: i64) {
        let mut g = self.local.lock().unwrap();
        *g += chrono::Duration::minutes(m);
    }
}

impl Clock for TestClock {
    fn now_utc(&self) -> DateTime<Utc> {
        let l = *self.local.lock().unwrap();
        Local
            .from_local_datetime(&l)
            .earliest()
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|| Utc.from_utc_datetime(&l))
    }

    fn now_local(&self) -> NaiveDateTime {
        *self.local.lock().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clock_reports_what_it_was_set_to() {
        let c = TestClock::parse("2026-03-01 20:00:00");
        assert_eq!(c.now_local().format("%H:%M").to_string(), "20:00");
        c.advance_minutes(150);
        assert_eq!(c.now_local().format("%Y-%m-%d %H:%M").to_string(), "2026-03-01 22:30");
    }

    #[test]
    fn system_clock_moves_forward() {
        let c = SystemClock;
        assert!(c.now_utc() <= SystemClock.now_utc());
    }
}
