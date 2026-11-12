//! Reminders, decided from the clock, the settings and whether the user is at the computer.
//! Pure logic: `lib.rs` feeds it every 30 s and shows the notifications.

use crate::settings::{parse_hm, Settings};
use chrono::{DateTime, Datelike, Duration, Local, Weekday};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nudge {
    /// Using the computer during work hours with no timer running.
    NoTimer,
    /// A timer still runs after the end of work hours.
    StillRunning,
}

/// Using the computer for this long without a timer gives a reminder...
const NO_TIMER_AFTER: i64 = 10 * 60;
/// ...repeated at most this often.
const NO_TIMER_EVERY: i64 = 30 * 60;
const STILL_RUNNING_EVERY: i64 = 60 * 60;
/// Seconds since the last input below which the user counts as at the computer.
const PRESENT: u64 = 120;

#[derive(Debug, Default)]
pub struct Reminders {
    active_secs: i64,
    last_no_timer: Option<DateTime<Local>>,
    last_still_running: Option<DateTime<Local>>,
}

fn workday(now: DateTime<Local>) -> bool {
    !matches!(now.weekday(), Weekday::Sat | Weekday::Sun)
}

fn due(last: Option<DateTime<Local>>, now: DateTime<Local>, every: i64) -> bool {
    last.is_none_or(|l| now - l >= Duration::seconds(every))
}

/// Within work hours on a workday (the only time the "no timer" reminder reads idle time).
pub fn in_work_hours(now: DateTime<Local>, s: &Settings) -> bool {
    match (parse_hm(&s.work_start), parse_hm(&s.work_end)) {
        (Ok(start), Ok(end)) => workday(now) && (start..end).contains(&now.time()),
        _ => false,
    }
}

impl Reminders {
    /// Call every `step_secs`. `idle_secs`: seconds since the last input (`None` if unknown).
    pub fn tick(
        &mut self,
        now: DateTime<Local>,
        step_secs: i64,
        s: &Settings,
        timer_running: bool,
        idle_secs: Option<u64>,
    ) -> Option<Nudge> {
        let (Ok(start), Ok(end)) = (parse_hm(&s.work_start), parse_hm(&s.work_end)) else {
            return None;
        };
        if !s.reminders || !workday(now) {
            self.active_secs = 0;
            return None;
        }
        let t = now.time();
        if timer_running {
            self.active_secs = 0;
            if t >= end && due(self.last_still_running, now, STILL_RUNNING_EVERY) {
                self.last_still_running = Some(now);
                return Some(Nudge::StillRunning);
            }
            return None;
        }
        let at_computer = idle_secs.is_some_and(|i| i < PRESENT);
        if !(start..end).contains(&t) || !at_computer {
            self.active_secs = 0;
            return None;
        }
        self.active_secs += step_secs;
        if self.active_secs >= NO_TIMER_AFTER && due(self.last_no_timer, now, NO_TIMER_EVERY) {
            self.last_no_timer = Some(now);
            return Some(Nudge::NoTimer);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    // 2026-11-05 is a Thursday, 2026-11-07 a Saturday.
    fn at(day: u32, h: u32, m: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 11, day, h, m, 0).unwrap()
    }

    fn run(r: &mut Reminders, from: DateTime<Local>, minutes: i64, running: bool, idle: Option<u64>) -> Vec<(i64, Nudge)> {
        let s = Settings::default();
        (0..minutes * 2)
            .filter_map(|i| {
                let now = from + Duration::seconds(i * 30);
                r.tick(now, 30, &s, running, idle).map(|n| (i / 2, n))
            })
            .collect()
    }

    #[test]
    fn no_timer_after_ten_active_minutes_then_every_thirty() {
        let mut r = Reminders::default();
        let got = run(&mut r, at(5, 10, 0), 45, false, Some(5));
        assert_eq!(got, vec![(9, Nudge::NoTimer), (39, Nudge::NoTimer)]);
    }

    #[test]
    fn no_reminder_when_away_outside_hours_or_weekend() {
        assert!(run(&mut Reminders::default(), at(5, 10, 0), 30, false, Some(600)).is_empty());
        assert!(run(&mut Reminders::default(), at(5, 10, 0), 30, false, None).is_empty());
        assert!(run(&mut Reminders::default(), at(5, 7, 0), 30, false, Some(5)).is_empty());
        assert!(run(&mut Reminders::default(), at(7, 10, 0), 30, false, Some(5)).is_empty());
    }

    #[test]
    fn still_running_after_work_end_hourly() {
        let mut r = Reminders::default();
        assert!(run(&mut r, at(5, 17, 0), 59, true, Some(5)).is_empty());
        let got = run(&mut r, at(5, 18, 0), 125, true, Some(5));
        assert_eq!(got, vec![(0, Nudge::StillRunning), (60, Nudge::StillRunning), (120, Nudge::StillRunning)]);
    }

    #[test]
    fn off_switch() {
        let s = Settings { reminders: false, ..Settings::default() };
        let mut r = Reminders::default();
        for i in 0..40 {
            assert_eq!(r.tick(at(5, 10, 0) + Duration::seconds(i * 30), 30, &s, false, Some(1)), None);
        }
    }
}
