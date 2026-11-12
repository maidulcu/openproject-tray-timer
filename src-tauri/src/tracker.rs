//! Idle bookkeeping for the running timer. Pure logic, no I/O.
//!
//! What is kept: when the running timer started, when the user went idle, and when the app
//! last saw the computer awake. Nothing else.
//!
//! The rule: after the idle threshold with no mouse/keyboard input (or a sleep / app restart
//! gap at least that long), the timer stops at the moment the input stopped. The user then
//! says whether the away time was work away from the keyboard (a meeting, a call).

use crate::api::Task;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// A gap between heartbeats longer than this means the computer slept or the app was not
/// running (heartbeats come every 30 s).
pub const GAP: i64 = 90;

/// Local state for the one running timer (OpenProject allows one per user).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub entry_id: u64,
    pub started_at: DateTime<Utc>,
    /// Set while the user is idle right now (and the timer may still need stopping).
    #[serde(default)]
    pub idle_since: Option<DateTime<Utc>>,
    /// Last heartbeat: the app was running and the computer awake.
    #[serde(default)]
    pub last_seen: Option<DateTime<Utc>>,
    /// Started in this app (possibly offline): `started_at` is the real start, so the server's
    /// later creation time doesn't replace it.
    #[serde(default)]
    pub local_start: bool,
}

impl Session {
    pub fn new(entry_id: u64, started_at: DateTime<Utc>) -> Self {
        Self { entry_id, started_at, idle_since: None, last_seen: None, local_start: false }
    }

    /// The user went idle at `since`. Idle that began before the timer started counts from
    /// the start. Returns the moment the timer should stop.
    pub fn on_idle(&mut self, since: DateTime<Utc>) -> DateTime<Utc> {
        *self.idle_since.get_or_insert(since.max(self.started_at))
    }

    /// Heartbeat at `now`. If the last one is too long ago (sleep, shutdown, app killed) and
    /// the gap is at least `min_away`, returns when the away time began.
    pub fn heartbeat(&mut self, now: DateTime<Utc>, min_away: Duration) -> Option<DateTime<Utc>> {
        let last = self.last_seen.replace(now)?;
        if (now - last).num_seconds() < GAP {
            return None;
        }
        let from = self.idle_since.unwrap_or(last).max(self.started_at);
        (now - from >= min_away).then_some(from)
    }

    /// Minutes to send as `hours` when the timer stops at `until`: rounded to the nearest
    /// minute, at least 1.
    pub fn worked_minutes(&self, until: DateTime<Utc>) -> i64 {
        (((until - self.started_at).num_seconds().max(0) + 30) / 60).max(1)
    }

    pub fn worked_secs(&self, now: DateTime<Utc>) -> i64 {
        (now - self.started_at).num_seconds().max(0)
    }
}

/// A timer the app stopped because the user was away. Waits for the user's answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdleStop {
    pub task: Task,
    pub comment: String,
    /// When input stopped (the timer was stopped at this time).
    pub from: DateTime<Utc>,
    /// When the user came back; `None` while still away.
    pub to: Option<DateTime<Utc>>,
}

impl IdleStop {
    pub fn minutes(&self) -> Option<i64> {
        self.to.map(|to| (((to - self.from).num_seconds().max(0)) + 30) / 60)
    }
}

/// Tasks whose title says nothing about the work, so their time needs a note. Servers may
/// enforce more (or other) rules; their refusals are shown as they come.
pub const NOTE_REQUIRED_TASKS: &[&str] = &["General work"];

pub fn note_required(subject: &str) -> bool {
    NOTE_REQUIRED_TASKS.iter().any(|t| subject.trim().eq_ignore_ascii_case(t))
}

/// Title of the task breaks are logged on (prayer, tea, lunch). Its project must never be
/// billed. The "Break" answer is offered only when the server has an open task with this title.
pub const BREAK_TASK: &str = "Breaks";

pub fn is_break_task(subject: &str) -> bool {
    subject.trim().eq_ignore_ascii_case(BREAK_TASK)
}

/// The time entry's note. Optional (without one, the task title describes the work), except on
/// the tasks in `NOTE_REQUIRED_TASKS`.
pub fn clean_comment(comment: &str, subject: &str) -> Result<String, String> {
    let c = comment.trim();
    if c.is_empty() && note_required(subject) {
        return Err(format!("Time on \"{}\" needs a note saying what you did.", subject.trim()));
    }
    Ok(c.to_string())
}

/// A comment that must say something (a meeting entry, a reason on a task).
pub fn require_text(text: &str, what: &str) -> Result<String, String> {
    let t = text.trim();
    if t.chars().count() < 3 {
        return Err(format!("{what} is required."));
    }
    Ok(t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(min: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 5, 9, 0, 0).unwrap() + Duration::minutes(min)
    }
    const TEN: i64 = 10;

    #[test]
    fn worked_minutes_rounds_and_has_a_floor() {
        let s = Session::new(1, t(0));
        assert_eq!(s.worked_minutes(t(42)), 42);
        assert_eq!(s.worked_minutes(t(0) + Duration::seconds(5)), 1);
        assert_eq!(s.worked_minutes(t(-5)), 1);
    }

    #[test]
    fn idle_stops_at_idle_start_clamped_to_timer_start() {
        let mut s = Session::new(1, t(10));
        assert_eq!(s.on_idle(t(30)), t(30));
        assert_eq!(s.on_idle(t(35)), t(30), "a repeated idle signal keeps the first start");
        let mut s = Session::new(1, t(10));
        assert_eq!(s.on_idle(t(0)), t(10));
    }

    #[test]
    fn heartbeats_in_time_are_quiet() {
        let mut s = Session::new(1, t(0));
        assert_eq!(s.heartbeat(t(1), Duration::minutes(TEN)), None, "first beat only records");
        assert_eq!(s.heartbeat(t(1) + Duration::seconds(30), Duration::minutes(TEN)), None);
    }

    #[test]
    fn long_gap_is_away_from_the_last_beat() {
        let mut s = Session::new(1, t(0));
        s.heartbeat(t(20), Duration::minutes(TEN));
        assert_eq!(s.heartbeat(t(80), Duration::minutes(TEN)), Some(t(20)));
        assert_eq!(s.last_seen, Some(t(80)));
    }

    #[test]
    fn short_sleep_is_ignored() {
        let mut s = Session::new(1, t(0));
        s.heartbeat(t(20), Duration::minutes(TEN));
        assert_eq!(s.heartbeat(t(25), Duration::minutes(TEN)), None);
    }

    #[test]
    fn gap_after_idle_starts_at_idle() {
        let mut s = Session::new(1, t(0));
        s.heartbeat(t(20), Duration::minutes(TEN));
        s.on_idle(t(15));
        assert_eq!(s.heartbeat(t(60), Duration::minutes(TEN)), Some(t(15)));
    }

    #[test]
    fn idle_stop_minutes() {
        let task = Task { id: 1, subject: "x".into(), project: "p".into() };
        let mut st = IdleStop { task, comment: "c".into(), from: t(10), to: None };
        assert_eq!(st.minutes(), None);
        st.to = Some(t(47));
        assert_eq!(st.minutes(), Some(37));
    }

    #[test]
    fn comments() {
        assert_eq!(clean_comment("  ", "Build login form").unwrap(), "");
        assert!(clean_comment("  ", "General work").is_err());
        assert!(clean_comment("", " general WORK ").is_err());
        assert_eq!(clean_comment(" Fixed login bug ", "General work").unwrap(), "Fixed login bug");
        assert!(require_text("ok", "A note").is_err());
        assert_eq!(require_text(" Planning call ", "A note").unwrap(), "Planning call");
    }
}
