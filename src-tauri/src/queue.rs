//! Timer actions waiting for the server (offline, or queued behind one that is waiting).
//! Pure data and bookkeeping; `core` sends them in order when the server answers again.
//!
//! Entry id 0 stands for the timer started while offline: its real id is known once its
//! `Start` has been sent, and the later actions are pointed at it then.

use crate::api::Task;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// Entry id of a timer started offline whose `Start` has not reached the server yet.
pub const PENDING: u64 = 0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// Start a timer (it ran from `at`; the server only learns about it later).
    Start { task: Task, comment: String, at: DateTime<Utc> },
    /// Change the note of a running timer.
    Comment { entry_id: u64, comment: String },
    /// Point a running timer at another task.
    Retarget { entry_id: u64, task: Task },
    /// A comment on the task itself (why it needs more time than estimated).
    TaskComment { task: Task, text: String },
    /// Stop a running timer with these hours and note (stopped at `at`).
    Stop { entry_id: u64, task: Task, minutes: i64, comment: String, at: DateTime<Utc> },
    /// A finished entry: a timer that ran entirely offline, or an away period that was work.
    Log { task: Task, day: NaiveDate, minutes: i64, comment: String },
}

impl Action {
    /// One line for the window and the tray ("Stop: 1:20 on #12 Fix login").
    pub fn describe(&self) -> String {
        let hm = |m: &i64| format!("{}:{:02}", m / 60, m % 60);
        match self {
            Action::Start { task, .. } => format!("Start timer on #{} {}", task.id, task.subject),
            Action::Comment { .. } => "Change the timer's note".into(),
            Action::Retarget { task, .. } => format!("Move the timer to #{} {}", task.id, task.subject),
            Action::TaskComment { task, .. } => format!("Comment on #{} {}", task.id, task.subject),
            Action::Stop { minutes, .. } => format!("Stop the timer ({})", hm(minutes)),
            Action::Log { task, minutes, .. } => format!("Log {} on #{} {}", hm(minutes), task.id, task.subject),
        }
    }

    /// The task and note a refusal can be fixed on, if the action has a note.
    pub fn note(&self) -> Option<&str> {
        match self {
            Action::Start { comment, .. }
            | Action::Comment { comment, .. }
            | Action::Stop { comment, .. }
            | Action::Log { comment, .. } => Some(comment),
            _ => None,
        }
    }

    pub fn set_note(&mut self, note: &str) {
        match self {
            Action::Start { comment, .. }
            | Action::Comment { comment, .. }
            | Action::Stop { comment, .. }
            | Action::Log { comment, .. } => *comment = note.trim().to_string(),
            _ => {}
        }
    }

    /// The task the action logs time on (a reason for going over its estimate goes there).
    pub fn task(&self) -> Option<&Task> {
        match self {
            Action::Start { task, .. }
            | Action::Retarget { task, .. }
            | Action::TaskComment { task, .. }
            | Action::Stop { task, .. }
            | Action::Log { task, .. } => Some(task),
            Action::Comment { .. } => None,
        }
    }

    /// Lower the hours of a `Stop` or `Log` (e.g. the server refused more than a day's work).
    pub fn cap_minutes(&mut self, max: i64) {
        if let Action::Stop { minutes, .. } | Action::Log { minutes, .. } = self {
            *minutes = (*minutes).min(max.max(1));
        }
    }

    pub fn minutes(&self) -> Option<i64> {
        match self {
            Action::Stop { minutes, .. } | Action::Log { minutes, .. } => Some(*minutes),
            _ => None,
        }
    }

    pub fn entry_id(&self) -> Option<u64> {
        match self {
            Action::Comment { entry_id, .. } | Action::Retarget { entry_id, .. } | Action::Stop { entry_id, .. } => {
                Some(*entry_id)
            }
            _ => None,
        }
    }
}

/// A queued action the server refused: sending pauses until the user fixes or drops it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Refusal {
    pub action: Action,
    pub messages: Vec<String>,
}

/// The offline timer was stopped before its `Start` was sent: replace everything queued for it
/// with one finished entry (`Log`). Returns false if its `Start` was already sent.
pub fn fold_offline_timer(queue: &mut Vec<Action>, task: &Task, comment: &str, minutes: i64, day: NaiveDate) -> bool {
    let Some(i) = queue.iter().position(|a| matches!(a, Action::Start { .. })) else {
        return false;
    };
    queue.retain(|a| !matches!(a, Action::Start { .. }) && a.entry_id() != Some(PENDING));
    let log = Action::Log { task: task.clone(), day, minutes, comment: comment.trim().to_string() };
    queue.insert(i.min(queue.len()), log);
    true
}

/// The offline timer's `Start` reached the server as entry `id`: point the rest at it.
pub fn resolve_pending(queue: &mut [Action], id: u64) {
    for a in queue.iter_mut() {
        match a {
            Action::Comment { entry_id, .. } | Action::Retarget { entry_id, .. } | Action::Stop { entry_id, .. }
                if *entry_id == PENDING =>
            {
                *entry_id = id
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn task(id: u64) -> Task {
        Task { id, subject: format!("T{id}"), project: "P".into() }
    }
    fn at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 10, 9, 0, 0).unwrap()
    }
    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 10).unwrap()
    }

    #[test]
    fn offline_start_then_stop_becomes_one_log() {
        let mut q = vec![
            Action::Stop { entry_id: 5, task: task(3), minutes: 30, comment: "a".into(), at: at() },
            Action::Start { task: task(1), comment: "x".into(), at: at() },
            Action::Comment { entry_id: PENDING, comment: "y".into() },
            Action::Retarget { entry_id: PENDING, task: task(2) },
        ];
        assert!(fold_offline_timer(&mut q, &task(2), "y", 45, day()));
        assert_eq!(q.len(), 2);
        assert!(matches!(&q[0], Action::Stop { entry_id: 5, .. }));
        assert_eq!(q[1], Action::Log { task: task(2), day: day(), minutes: 45, comment: "y".into() });
    }

    #[test]
    fn fold_needs_a_queued_start() {
        let mut q = vec![Action::Comment { entry_id: 7, comment: "y".into() }];
        assert!(!fold_offline_timer(&mut q, &task(1), "y", 10, day()));
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn pending_actions_get_the_real_id() {
        let mut q = vec![
            Action::Comment { entry_id: PENDING, comment: "y".into() },
            Action::Stop { entry_id: PENDING, task: task(1), minutes: 3, comment: "y".into(), at: at() },
            Action::Stop { entry_id: 9, task: task(1), minutes: 3, comment: "y".into(), at: at() },
        ];
        resolve_pending(&mut q, 42);
        assert!(matches!(q[0], Action::Comment { entry_id: 42, .. }));
        assert!(matches!(q[1], Action::Stop { entry_id: 42, .. }));
        assert!(matches!(q[2], Action::Stop { entry_id: 9, .. }));
    }

    #[test]
    fn notes_can_be_fixed() {
        let mut a = Action::Log { task: task(1), day: day(), minutes: 5, comment: "Jonas did it".into() };
        a.set_note(" Fixed it ");
        assert_eq!(a.note(), Some("Fixed it"));
        assert_eq!(a.describe(), "Log 0:05 on #1 T1");
    }
}
