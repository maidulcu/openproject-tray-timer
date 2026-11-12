//! The timer's behaviour, independent of the UI: talks to OpenProject and keeps the idle
//! bookkeeping in step with the running timer. The Tauri layer (`lib.rs`) only wires this to
//! the tray, the window and the idle source, so tests can drive `Core` directly.
//!
//! Rules:
//! - A timer starts with its note (what you are working on), so stopping takes one click. The
//!   note is optional, except on tasks like "General work" (`tracker::NOTE_REQUIRED_TASKS`).
//! - The server may refuse a stop (HTTP 422: too many hours, over the task's estimate, a word
//!   the note must not contain...). The timer then keeps running and `stop_problem` holds the
//!   server's reasons until the user fixes the note, adds a reason on the task or picks an
//!   earlier end, and stops again.
//! - After the idle threshold with no input, or a sleep / app-restart gap that long, the
//!   timer stops at the moment input stopped. When the user is back they say whether the
//!   away time was work (a meeting, a call); only then is it logged, as its own entry.
//! - Offline (or while earlier actions still wait), every action is queued with its real time
//!   and the local timer carries on. The queue is sent in order when the server answers; a
//!   timer that started and stopped offline goes up as one finished entry. A queued action
//!   the server refuses pauses the queue (`refusal`) until the user fixes or drops it.

use crate::api::{self, ApiError, Client, Task, TimeEntry, User};
use crate::idle::IdleEvent;
use crate::queue::{self, Action, Refusal, PENDING};
use crate::tracker::{self, clean_comment, require_text, IdleStop, Session};
use chrono::{DateTime, Duration, Local, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// What the app says when the server's timer is no longer the one it was showing.
pub const STOPPED_ELSEWHERE: &str =
    "This timer was already stopped (in the browser or on another computer), so nothing was changed. The app now shows the server's state.";

/// Tasks remembered for starting a timer offline.
const MAX_CACHED_TASKS: usize = 300;

/// What is saved between runs.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    session: Option<Session>,
    idle_stop: Option<IdleStop>,
    /// When the user came back while an idle stop could not reach the server yet.
    back_at: Option<DateTime<Utc>>,
    stop_problem: Option<StopProblem>,
    /// The running timer as last known (the server's, or one started offline).
    timer: Option<TimeEntry>,
    user: Option<User>,
    queue: Vec<Action>,
    refusal: Option<Refusal>,
    tasks: Vec<Task>,
    break_task: Option<Task>,
}

/// A stop the server refused; the timer still runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StopProblem {
    /// The server's reasons, as it gave them.
    pub messages: Vec<String>,
    /// Stop at this time (where input stopped) rather than when the user retries.
    pub until: Option<DateTime<Utc>>,
}

/// The server's reasons ask for a reason on the task (it is over its estimate).
pub fn needs_reason(messages: &[String]) -> bool {
    messages.iter().any(|m| m.to_lowercase().contains("estimate"))
}

impl StopProblem {
    pub fn needs_reason(&self) -> bool {
        needs_reason(&self.messages)
    }
}

/// What the user adds when stopping: a changed note, a reason posted on the task, an end
/// time earlier than now.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct StopInput {
    pub comment: Option<String>,
    pub reason: Option<String>,
    pub until: Option<DateTime<Utc>>,
}

#[derive(Debug)]
pub enum Stop {
    /// Stopped (on the server, or queued to be sent while offline).
    Done,
    /// Refused by the server (or a local check); the timer still runs. See `stop_problem`.
    Refused(Vec<String>),
}

pub struct Core {
    pub client: Option<Client>,
    pub user: Option<User>,
    /// The running timer: the server's, or one started offline (id `queue::PENDING`).
    pub timer: Option<TimeEntry>,
    pub session: Option<Session>,
    /// A timer stopped for being away, waiting for the user's answer.
    pub idle_stop: Option<IdleStop>,
    pub stop_problem: Option<StopProblem>,
    /// Actions waiting for the server, oldest first.
    pub queue: Vec<Action>,
    /// A queued action the server refused; sending is paused until it is fixed or dropped.
    pub refusal: Option<Refusal>,
    /// The server could not be reached the last time it was tried.
    pub offline: bool,
    /// Something the user should know that isn't an error (e.g. a change dropped because the
    /// timer was stopped elsewhere). Shown once.
    pub notice: Option<String>,
    tasks: Vec<Task>,
    /// The open task titled "Breaks", if the server has one (where breaks are logged).
    pub break_task: Option<Task>,
    back_at: Option<DateTime<Utc>>,
    pub idle_threshold_secs: i64,
    /// Where the state is saved between runs (`None` in tests).
    state_path: Option<PathBuf>,
}

fn local_day(t: DateTime<Utc>) -> NaiveDate {
    t.with_timezone(&Local).date_naive()
}

impl Core {
    pub fn new(state_path: Option<PathBuf>, idle_threshold_secs: i64) -> Self {
        let saved: Saved = state_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            client: None,
            user: saved.user,
            timer: saved.timer,
            session: saved.session,
            idle_stop: saved.idle_stop,
            stop_problem: saved.stop_problem,
            queue: saved.queue,
            refusal: saved.refusal,
            offline: false,
            notice: None,
            tasks: saved.tasks,
            break_task: saved.break_task,
            back_at: saved.back_at,
            idle_threshold_secs,
            state_path,
        }
    }

    fn save(&self) {
        let Some(path) = &self.state_path else { return };
        let saved = Saved {
            session: self.session.clone(),
            idle_stop: self.idle_stop.clone(),
            back_at: self.back_at,
            stop_problem: self.stop_problem.clone(),
            timer: self.timer.clone(),
            user: self.user.clone(),
            queue: self.queue.clone(),
            refusal: self.refusal.clone(),
            tasks: self.tasks.clone(),
            break_task: self.break_task.clone(),
        };
        let res = serde_json::to_string_pretty(&saved).map_err(|e| e.to_string()).and_then(|j| {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            // Write then rename, so a crash never leaves half a file (the queue is in it).
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, j).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, path).map_err(|e| e.to_string())
        });
        if let Err(e) = res {
            log::warn!("could not save timer state: {e}");
        }
    }

    pub fn client(&self) -> Result<&Client, String> {
        self.client.as_ref().ok_or_else(|| "Not set up: open Settings first.".to_string())
    }

    fn idle_threshold(&self) -> Duration {
        Duration::seconds(self.idle_threshold_secs.max(1))
    }

    /// Actions are waiting (or one was refused): new ones queue behind them to keep the order.
    pub fn queue_busy(&self) -> bool {
        !self.queue.is_empty() || self.refusal.is_some()
    }

    // ---- task cache (for starting offline) ----

    pub fn remember_tasks(&mut self, tasks: &[Task]) {
        for t in tasks.iter().rev() {
            self.tasks.retain(|x| x.id != t.id);
            self.tasks.insert(0, t.clone());
        }
        self.tasks.truncate(MAX_CACHED_TASKS);
        self.save();
    }

    /// Cached tasks matching `text` (subject or ID), for searching offline.
    pub fn cached_tasks(&self, text: &str) -> Vec<Task> {
        let q = text.trim().to_lowercase();
        self.tasks
            .iter()
            .filter(|t| q.is_empty() || t.id.to_string() == q || t.subject.to_lowercase().contains(&q))
            .take(25)
            .cloned()
            .collect()
    }

    async fn find_task(&mut self, id: u64) -> Result<Task, String> {
        if let Some(t) = self.tasks.iter().find(|t| t.id == id) {
            return Ok(t.clone());
        }
        match self.client()?.task(id).await {
            Ok(t) => {
                self.remember_tasks(std::slice::from_ref(&t));
                Ok(t)
            }
            Err(e) if e.is_offline() => {
                self.offline = true;
                Err("You're offline and this task isn't in the app's list yet: pick a recent task.".into())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    // ---- sending, now or later ----

    fn enqueue(&mut self, a: Action) {
        self.queue.push(a);
        self.save();
    }

    /// Send `a` now, or queue it if offline or behind waiting actions. Returns the entry the
    /// server saved when it was sent now.
    async fn submit(&mut self, a: Action) -> Result<Option<TimeEntry>, ApiError> {
        if self.offline || self.queue_busy() {
            self.enqueue(a);
            return Ok(None);
        }
        let client = self.client().map_err(ApiError::Server)?.clone();
        match self.send_action(&client, &a).await {
            Ok(e) => Ok(e),
            Err(e) if e.is_offline() => {
                self.offline = true;
                self.enqueue(a);
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    async fn send_action(&mut self, c: &Client, a: &Action) -> api::Result<Option<TimeEntry>> {
        let e = match a {
            Action::Start { task, comment, at } => {
                let e = c.start_timer(task.id, local_day(*at), comment).await?;
                queue::resolve_pending(&mut self.queue, e.id);
                if let Some(t) = self.timer.as_mut().filter(|t| t.id == PENDING) {
                    t.id = e.id;
                }
                if let Some(s) = self.session.as_mut().filter(|s| s.entry_id == PENDING) {
                    s.entry_id = e.id;
                }
                e
            }
            Action::Comment { entry_id, comment } => c.update_comment(*entry_id, comment).await?,
            Action::Retarget { entry_id, task } => c.retarget_timer(*entry_id, task.id).await?,
            Action::TaskComment { task, text } => {
                c.comment_on_task(task.id, text).await?;
                return Ok(None);
            }
            Action::Stop { entry_id, minutes, comment, .. } => c.stop_timer(*entry_id, *minutes, comment).await?,
            Action::Log { task, day, minutes, comment } => c.log_time(task.id, *day, *minutes, comment).await?,
        };
        self.offline = false;
        Ok(Some(e))
    }

    /// Send waiting actions in order, until the queue is empty, the server is unreachable, or
    /// it refuses one (then `refusal` holds it).
    pub async fn flush(&mut self) -> Result<(), String> {
        if self.refusal.is_some() || self.queue.is_empty() {
            return Ok(());
        }
        let client = self.client()?.clone();
        while let Some(a) = self.queue.first().cloned() {
            match self.send_action(&client, &a).await {
                Ok(_) => {
                    self.queue.remove(0);
                    self.save();
                }
                Err(e) if e.is_offline() => {
                    self.offline = true;
                    return Ok(());
                }
                // The timer was stopped or deleted elsewhere: nothing left to change.
                Err(e) if e.is_not_allowed() && a.entry_id().is_some() => {
                    self.queue.remove(0);
                    self.notice = Some(STOPPED_ELSEWHERE.into());
                    self.save();
                }
                Err(e) => {
                    let messages = match e {
                        ApiError::Refused(m) => m,
                        other => vec![other.to_string()],
                    };
                    self.queue.remove(0);
                    self.refusal = Some(Refusal { action: a, messages });
                    self.save();
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Send the refused action again: with a fixed note, a reason posted on its task first,
    /// or at most `minutes`.
    pub async fn retry_refused(&mut self, note: Option<String>, reason: Option<String>, minutes: Option<i64>) -> Result<(), String> {
        let mut r = self.refusal.clone().ok_or("Nothing is waiting.")?;
        if let Some(n) = note {
            r.action.set_note(&n);
        }
        if let Some(m) = minutes {
            r.action.cap_minutes(m);
        }
        let reason = reason.filter(|t| !t.trim().is_empty()).map(|t| require_text(&t, "The reason")).transpose()?;
        self.refusal = None;
        self.queue.insert(0, r.action.clone());
        if let (Some(text), Some(task)) = (reason, r.action.task()) {
            self.queue.insert(0, Action::TaskComment { task: task.clone(), text });
        }
        self.save();
        self.refresh().await
    }

    /// Drop the refused action. Dropping a timer's start also drops what was queued for it,
    /// and the local timer if it is that one.
    pub fn discard_refused(&mut self) -> Result<(), String> {
        let r = self.refusal.take().ok_or("Nothing is waiting.")?;
        if matches!(r.action, Action::Start { .. }) {
            self.queue.retain(|a| a.entry_id() != Some(PENDING));
            if self.timer.as_ref().is_some_and(|t| t.id == PENDING) {
                self.set_timer(None);
            }
        }
        self.save();
        Ok(())
    }

    /// Match the local session to the running timer (adopts timers started elsewhere).
    fn set_timer(&mut self, timer: Option<TimeEntry>) {
        self.session = match (&timer, self.session.take()) {
            (Some(t), Some(mut s)) if s.entry_id == t.id => {
                if !s.local_start {
                    s.started_at = t.started_at;
                }
                Some(s)
            }
            (Some(t), _) => {
                let mut s = Session::new(t.id, t.started_at);
                s.local_start = t.id == PENDING;
                Some(s)
            }
            (None, _) => None,
        };
        if timer.is_none() {
            self.stop_problem = None;
        }
        self.timer = timer;
        self.save();
    }

    pub async fn connect(&mut self, client: Client) -> Result<(), String> {
        self.client = Some(client.clone());
        match client.me().await {
            Ok(user) => {
                self.user = Some(user);
                self.offline = false;
                // Where breaks go: the open task titled "Breaks" (kept while offline).
                if let Ok(found) = client.search_tasks(tracker::BREAK_TASK).await {
                    self.break_task = found.into_iter().find(|t| tracker::is_break_task(&t.subject));
                }
            }
            // Keep working from what was saved; the key was checked when it was entered.
            Err(e) if e.is_offline() => {
                self.offline = true;
                return Ok(());
            }
            Err(e) => {
                self.client = None;
                return Err(e.to_string());
            }
        }
        self.refresh().await
    }

    /// Send waiting actions, then (if none are left) take the server's running timer.
    pub async fn refresh(&mut self) -> Result<(), String> {
        self.flush().await?;
        if self.queue_busy() {
            return Ok(());
        }
        match self.client()?.running_timer().await {
            Ok(t) => {
                self.offline = false;
                self.set_timer(t);
                Ok(())
            }
            Err(e) if e.is_offline() => {
                self.offline = true;
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// Before changing the running timer: is it still the one running on the server? If it
    /// was stopped elsewhere, take the server's state and say so. Skipped offline or while
    /// changes wait (the server is then behind the app, not ahead of it).
    async fn still_running(&mut self) -> Result<(), String> {
        let Some(id) = self.timer.as_ref().map(|t| t.id).filter(|id| *id != PENDING) else {
            return Ok(());
        };
        if self.offline || self.queue_busy() {
            return Ok(());
        }
        match self.client()?.running_timer().await {
            Ok(t) if t.as_ref().map(|t| t.id) == Some(id) => Ok(()),
            Ok(t) => {
                self.set_timer(t);
                Err(STOPPED_ELSEWHERE.into())
            }
            Err(e) if e.is_offline() => {
                self.offline = true;
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// A change on the running timer came back "not allowed": it was stopped elsewhere.
    async fn gone(&mut self) -> String {
        let _ = self.refresh().await;
        STOPPED_ELSEWHERE.into()
    }

    pub async fn start(&mut self, task_id: u64, comment: &str) -> Result<(), String> {
        let task = self.find_task(task_id).await?;
        let comment = clean_comment(comment, &task.subject)?;
        if !self.offline && !self.queue_busy() {
            // A timer started in the web UI or on another computer?
            self.refresh().await?;
        }
        if let Some(t) = &self.timer {
            return Err(format!("A timer is already running on \"{}\". Stop it first.", t.task.subject));
        }
        let now = Utc::now();
        let entry = TimeEntry {
            id: PENDING,
            ongoing: true,
            task: task.clone(),
            spent_on: local_day(now).to_string(),
            started_at: now,
            comment: comment.clone(),
        };
        self.set_timer(Some(entry));
        match self.submit(Action::Start { task, comment, at: now }).await {
            Ok(_) => {
                self.save();
                Ok(())
            }
            Err(e) => {
                self.set_timer(None);
                Err(e.to_string())
            }
        }
    }

    /// Fix the task of the running timer: all its time moves to `task_id`.
    pub async fn retarget(&mut self, task_id: u64) -> Result<(), String> {
        let task = self.find_task(task_id).await?;
        let id = self.timer.as_ref().ok_or("No timer is running.")?.id;
        self.still_running().await?;
        match self.submit(Action::Retarget { entry_id: id, task: task.clone() }).await {
            Err(e) if e.is_not_allowed() => return Err(self.gone().await),
            r => r.map_err(|e| e.to_string())?,
        };
        if let Some(t) = self.timer.as_mut() {
            t.task = task;
        }
        self.save();
        Ok(())
    }

    /// Change what the running timer says you are working on.
    pub async fn set_comment(&mut self, comment: &str) -> Result<(), String> {
        let timer = self.timer.as_ref().ok_or("No timer is running.")?;
        let comment = clean_comment(comment, &timer.task.subject)?;
        let id = timer.id;
        self.still_running().await?;
        match self.submit(Action::Comment { entry_id: id, comment: comment.clone() }).await {
            Err(e) if e.is_not_allowed() => return Err(self.gone().await),
            r => r.map_err(|e| e.to_string())?,
        };
        if let Some(t) = self.timer.as_mut() {
            t.comment = comment;
        }
        self.save();
        Ok(())
    }

    /// Stop the running timer, with its note. `input` may change the note, post a reason on
    /// the task first, or end earlier than `now`. A refusal keeps the timer running.
    pub async fn stop(&mut self, now: DateTime<Utc>, input: StopInput) -> Result<Stop, String> {
        self.stop_at(now, input, None).await
    }

    /// `idle_from`: the app's own end time (where input stopped); a user's `input.until` can
    /// only be earlier than now.
    async fn stop_at(&mut self, now: DateTime<Utc>, input: StopInput, idle_from: Option<DateTime<Utc>>) -> Result<Stop, String> {
        let timer = self.timer.clone().ok_or("No timer is running.")?;
        let note = input.comment.as_deref().unwrap_or(&timer.comment);
        let until = idle_from
            .or(input.until.map(|u| u.min(now)))
            .or(self.stop_problem.as_ref().and_then(|p| p.until))
            .unwrap_or(now);
        let keep_until = idle_from.is_some() || input.until.is_some();
        let comment = match clean_comment(note, &timer.task.subject) {
            Ok(c) => c,
            Err(msg) => return Ok(self.refused(vec![msg], until, keep_until)),
        };
        let reason = input.reason.filter(|r| !r.trim().is_empty()).map(|r| require_text(&r, "The reason")).transpose()?;
        if idle_from.is_none() {
            self.still_running().await?;
        }
        let session = self.session.as_ref().ok_or("No timer is running.")?;
        let away_from = session.idle_since;
        let minutes = session.worked_minutes(until);
        let day = local_day(session.started_at);

        // Started offline and never sent: it goes up as one finished entry.
        if timer.id == PENDING && queue::fold_offline_timer(&mut self.queue, &timer.task, &comment, minutes, day) {
            if let Some(text) = reason {
                let i = self.queue.iter().position(|a| matches!(a, Action::Log { .. })).unwrap_or(0);
                self.queue.insert(i, Action::TaskComment { task: timer.task.clone(), text });
            }
            self.finish_stop(timer.task, comment, away_from);
            return Ok(Stop::Done);
        }
        if let Some(text) = reason {
            self.submit(Action::TaskComment { task: timer.task.clone(), text }).await.map_err(|e| e.to_string())?;
        }
        let stop = Action::Stop { entry_id: timer.id, task: timer.task.clone(), minutes, comment: comment.clone(), at: until };
        match self.submit(stop).await {
            Ok(_) => {
                self.finish_stop(timer.task, comment, away_from);
                Ok(Stop::Done)
            }
            Err(ApiError::Refused(msgs)) => Ok(self.refused(msgs, until, keep_until || away_from.is_some())),
            Err(e) if e.is_not_allowed() => Err(self.gone().await),
            Err(e) => Err(e.to_string()),
        }
    }

    fn finish_stop(&mut self, task: Task, comment: String, away_from: Option<DateTime<Utc>>) {
        if let Some(from) = away_from {
            // Stopped because the user was away: ask about that time when they are back.
            let to = self.back_at.take();
            self.idle_stop = Some(IdleStop { task, comment, from, to });
        }
        self.set_timer(None);
    }

    fn refused(&mut self, messages: Vec<String>, until: DateTime<Utc>, keep_until: bool) -> Stop {
        self.stop_problem = Some(StopProblem { messages: messages.clone(), until: keep_until.then_some(until) });
        self.save();
        Stop::Refused(messages)
    }

    /// Stop the running timer (if the server allows) and start one on another task.
    pub async fn stop_and_start(
        &mut self,
        task_id: u64,
        comment: &str,
        now: DateTime<Utc>,
        input: StopInput,
    ) -> Result<Stop, String> {
        let task = self.find_task(task_id).await?;
        clean_comment(comment, &task.subject)?;
        let stop = self.stop(now, input).await?;
        if let Stop::Done = &stop {
            self.start(task_id, comment).await?;
        }
        Ok(stop)
    }

    /// Stop the running timer at `from`, when input stopped. If the server refuses, the
    /// timer keeps running and the user sorts it out when back (`stop_problem`).
    async fn stop_for_idle(&mut self, from: DateTime<Utc>) -> Result<(), String> {
        let Some(session) = self.session.as_mut() else { return Ok(()) };
        session.idle_since = Some(from);
        self.save();
        if self.timer.is_none() {
            return Ok(());
        }
        self.stop_at(Utc::now(), StopInput::default(), Some(from))
            .await
            .map(|_| ())
            .map_err(|e| format!("Could not stop the timer after you went away: {e}"))
    }

    /// The user is back at `at`. Returns true when something needs their answer: the
    /// "were you working?" question, or a stop the server refused.
    async fn come_back(&mut self, at: DateTime<Utc>) -> Result<bool, String> {
        let pending = self.session.as_ref().and_then(|s| s.idle_since).filter(|_| self.timer.is_some());
        if let Some(from) = pending {
            // Not stopped yet (refused, or a server error): remember the return, try again.
            self.back_at.get_or_insert(at);
            self.save();
            if self.stop_problem.is_none() {
                self.stop_for_idle(from).await?;
            }
        }
        if let Some(st) = self.idle_stop.as_mut().filter(|s| s.to.is_none()) {
            st.to = Some(self.back_at.take().unwrap_or(at).max(st.from));
        }
        self.save();
        Ok(self.needs_answer())
    }

    /// Something waits for the user: a refused stop or queued action, or an away period.
    pub fn needs_answer(&self) -> bool {
        self.stop_problem.is_some()
            || self.refusal.is_some()
            || self.idle_stop.as_ref().is_some_and(|s| s.to.is_some())
    }

    /// Feed an idle event. Returns true when the "were you working?" question is due.
    pub async fn on_idle_event(&mut self, ev: &IdleEvent) -> Result<bool, String> {
        match ev {
            IdleEvent::Idle { since } => {
                let Some(s) = self.session.as_mut() else { return Ok(false) };
                let from = s.on_idle(*since);
                self.stop_for_idle(from).await?;
                Ok(false)
            }
            IdleEvent::Active { at } => self.come_back(*at).await,
            IdleEvent::Unavailable(_) => Ok(false),
        }
    }

    /// Every 30 s: notices sleep / app-restart gaps and retries a stop that failed.
    /// Returns true when the "were you working?" question is due.
    pub async fn tick(&mut self, now: DateTime<Utc>) -> Result<bool, String> {
        let min = self.idle_threshold();
        let Some(s) = self.session.as_mut() else {
            return Ok(false);
        };
        if let Some(from) = s.heartbeat(now, min) {
            s.idle_since = Some(from);
            return self.come_back(now).await;
        }
        // Retry an idle stop that failed with a server error (not one the server refused).
        let retry = s.idle_since.filter(|from| now - *from >= min).filter(|_| self.stop_problem.is_none());
        self.save();
        if let Some(from) = retry {
            let was_back = self.back_at.is_some();
            self.stop_for_idle(from).await?;
            return Ok(was_back && self.needs_answer());
        }
        Ok(false)
    }

    /// Answer the "what was this time?" question with "a break": the away time is logged on the
    /// Breaks task (never billed), with `note` or "Break". `restart` as below.
    pub async fn resolve_idle_as_break(&mut self, note: &str, restart: Option<String>) -> Result<(), String> {
        let task = self
            .break_task
            .clone()
            .ok_or("There's no \"Breaks\" task on the server: choose \"I was away\" instead.")?;
        let st = self.idle_stop.clone().ok_or("Nothing to answer.")?;
        let minutes = st.minutes().ok_or("You are still away.")?;
        let restart = restart.map(|c| clean_comment(&c, &st.task.subject)).transpose()?;
        let note = match note.trim() {
            "" => "Break".to_string(),
            n => n.to_string(),
        };
        let log = Action::Log { task, day: local_day(st.from), minutes: minutes.max(1), comment: note };
        self.submit(log).await.map_err(|e| e.to_string())?;
        self.idle_stop = None;
        self.save();
        if let Some(comment) = restart {
            self.start(st.task.id, &comment).await?;
        }
        Ok(())
    }

    /// Answer the "were you working?" question. `meeting`: the task and comment the away
    /// time was work on (logged as its own entry). `restart`: start the timer again on the
    /// same task with this comment. `reason`: posted on the meeting's task first, when the
    /// server asked for one (over the task's estimate).
    pub async fn resolve_idle_stop(
        &mut self,
        meeting: Option<(u64, String)>,
        restart: Option<String>,
        reason: Option<String>,
    ) -> Result<Option<TimeEntry>, String> {
        let st = self.idle_stop.clone().ok_or("Nothing to answer.")?;
        let minutes = st.minutes().ok_or("You are still away.")?;
        let restart = restart.map(|c| clean_comment(&c, &st.task.subject)).transpose()?;
        let logged = match meeting {
            Some((task_id, comment)) => {
                let comment = require_text(&comment, "A note saying what the meeting or call was")?;
                let task = self.find_task(task_id).await?;
                if let Some(reason) = reason.filter(|r| !r.trim().is_empty()) {
                    let text = require_text(&reason, "The reason")?;
                    self.submit(Action::TaskComment { task: task.clone(), text }).await.map_err(|e| e.to_string())?;
                }
                let log = Action::Log { task, day: local_day(st.from), minutes: minutes.max(1), comment };
                self.submit(log).await.map_err(|e| e.to_string())?
            }
            None => None,
        };
        self.idle_stop = None;
        self.save();
        if let Some(comment) = restart {
            self.start(st.task.id, &comment).await?;
        }
        Ok(logged)
    }
}
