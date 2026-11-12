//! End-to-end test against a real OpenProject, with fake idle events.
//!
//! Never point this at production: it starts and stops timers and logs time.
//! Needs a test user with at least 3 open work packages assigned. The user's role may be
//! locked down to "log own time" without "edit own time entries" (the test checks that
//! finished entries are then read-only). The test can't delete what it logs under such a
//! role, so remove the test user's entries afterwards.
//!
//!   OP_URL=http://localhost:8080 OP_API_KEY=... cargo test --test live -- --ignored --test-threads=1

use chrono::{Duration, Utc};
use dwl_timer_lib::api::Client;
use dwl_timer_lib::core::{Core, Stop, StopInput};
use dwl_timer_lib::idle::IdleEvent;

/// A client for a closed port: every call fails as "offline".
fn dead_client() -> Client {
    Client::new("http://127.0.0.1:9", &std::env::var("OP_API_KEY").unwrap()).unwrap()
}

fn client() -> Client {
    let url = std::env::var("OP_URL").expect("set OP_URL");
    let key = std::env::var("OP_API_KEY").expect("set OP_API_KEY");
    Client::new(&url, &key).unwrap()
}

/// Delete any running timer of the test user so each test starts clean (a running timer can
/// be deleted with "log own time" alone; stopping it might be refused).
async fn clean(c: &Client) {
    if let Some(t) = c.running_timer().await.unwrap() {
        c.delete_time_entry(t.id).await.unwrap();
    }
}

async fn stored(c: &Client, id: u64) -> (String, String, bool) {
    let v = c.time_entry_raw(id).await.unwrap();
    (
        v["hours"].as_str().unwrap_or_default().to_string(),
        v["comment"]["raw"].as_str().unwrap_or_default().to_string(),
        v["ongoing"].as_bool().unwrap_or(false),
    )
}

#[tokio::test]
#[ignore]
async fn search() {
    let c = client();
    let mine = c.search_tasks("").await.unwrap();
    assert!(mine.len() >= 3, "need at least 3 open tasks assigned to the test user");
    let first = &mine[0];
    let by_id = c.search_tasks(&first.id.to_string()).await.unwrap();
    assert!(by_id.iter().any(|t| t.id == first.id), "search by ID");
    let word = first.subject.split_whitespace().last().unwrap();
    assert!(c.search_tasks(word).await.unwrap().iter().any(|t| t.id == first.id), "search by subject word");
    assert!(c.search_tasks("zzqqxx-no-such-task").await.unwrap().is_empty());
}

#[tokio::test]
#[ignore]
async fn full_day_flow() {
    let c = client();
    clean(&c).await;
    // Ordinary tasks only (not the ones the rules test needs: General work, a tiny estimate).
    let tasks: Vec<_> = c.search_tasks("").await.unwrap().into_iter()
        .filter(|t| !t.subject.contains("General work") && !t.subject.contains("Tiny") && t.subject != "Breaks")
        .collect();
    let (a, b, other) = (&tasks[0], &tasks[1], &tasks[2]);

    let mut core = Core::new(None, 10 * 60);
    core.connect(c.clone()).await.unwrap();
    assert!(core.timer.is_none());

    // One timer per user.
    core.start(a.id, "Build the login form").await.unwrap();
    let first = core.timer.as_ref().unwrap().id;
    assert_eq!(stored(&c, first).await.1, "Build the login form");
    assert!(core.start(b.id, "Something else").await.unwrap_err().contains("already running"));

    // Adopt: a fresh Core (app restart) finds the same running timer and its comment.
    let mut restarted = Core::new(None, 10 * 60);
    restarted.connect(c.clone()).await.unwrap();
    assert_eq!(restarted.timer.as_ref().unwrap().id, first);
    assert_eq!(restarted.timer.as_ref().unwrap().comment, "Build the login form");

    // Edit the comment while running, and fix a wrong task: same entry keeps running.
    core.set_comment("Build and style the login form").await.unwrap();
    core.retarget(b.id).await.unwrap();
    assert_eq!(core.timer.as_ref().unwrap().id, first);
    assert_eq!(core.timer.as_ref().unwrap().task.id, b.id);

    // Idle: input stops 12 minutes after the start, so the timer is stopped with 12 minutes.
    let start = core.session.as_ref().unwrap().started_at;
    let m = |n| start + Duration::minutes(n);
    assert!(!core.on_idle_event(&IdleEvent::Idle { since: m(12) }).await.unwrap());
    assert!(core.timer.is_none());
    assert!(c.running_timer().await.unwrap().is_none(), "stopped on the server");
    assert_eq!(stored(&c, first).await, ("PT12M".into(), "Build and style the login form".into(), false));

    // Finished entries are read-only when the role lacks "edit own time entries".
    let locked = c.update_comment(first, "edited later").await.is_err();
    println!("finished entries read-only for this user: {locked}");

    // Back 25 minutes later: it was a meeting on another task; start again on B.
    assert!(core.on_idle_event(&IdleEvent::Active { at: m(37) }).await.unwrap());
    assert_eq!(core.idle_stop.as_ref().unwrap().minutes(), Some(25));
    assert!(core.resolve_idle_stop(Some((other.id, "x".into())), None, None).await.unwrap_err().contains("required"));
    let meeting = core
        .resolve_idle_stop(Some((other.id, "Planning call with the client".into())), Some("Login form tests".into()), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored(&c, meeting.id).await, ("PT25M".into(), "Planning call with the client".into(), false));
    assert!(core.idle_stop.is_none());
    let second = core.timer.as_ref().unwrap();
    assert_eq!((second.task.id, second.comment.as_str()), (b.id, "Login form tests"));
    let second = second.id;

    // One-click stop uses the comment the timer was started with.
    assert!(matches!(core.stop(Utc::now(), StopInput::default()).await.unwrap(), Stop::Done));
    assert_eq!(stored(&c, second).await, ("PT1M".into(), "Login form tests".into(), false));

    // Switch: stop A (its own comment), start B with a new one.
    core.start(a.id, "Fix the menu").await.unwrap();
    let third = core.timer.as_ref().unwrap().id;
    core.stop_and_start(b.id, "Review the PR", Utc::now(), StopInput::default()).await.unwrap();
    assert_eq!(stored(&c, third).await.1, "Fix the menu");
    let fourth = core.timer.as_ref().unwrap().id;

    // Sleep: a heartbeat, then the next one an hour later. The timer stops at the first beat.
    let beat = core.session.as_ref().unwrap().started_at + Duration::minutes(3);
    assert!(!core.tick(beat).await.unwrap());
    assert!(core.tick(beat + Duration::minutes(60)).await.unwrap(), "away question after the gap");
    assert!(core.timer.is_none());
    assert_eq!(stored(&c, fourth).await, ("PT3M".into(), "Review the PR".into(), false));
    assert_eq!(core.idle_stop.as_ref().unwrap().minutes(), Some(60));
    // Not work: nothing logged, nothing restarted.
    assert!(core.resolve_idle_stop(None, None, None).await.unwrap().is_none());
    assert!(core.timer.is_none() && core.idle_stop.is_none());

    // A timer stopped in the web UI is noticed on refresh.
    core.start(a.id, "Last task").await.unwrap();
    let fifth = core.timer.as_ref().unwrap().id;
    c.stop_timer(fifth, 2, "stopped elsewhere").await.unwrap();
    core.refresh().await.unwrap();
    assert!(core.timer.is_none());
}

/// Refusals from a server with a server-side rules plugin (HTTP 422), as a normal member. Needs
/// OP_GENERAL_TASK (a task titled "General work") and OP_TINY_TASK (a task with an estimate
/// below one minute, e.g. 0.01 h, and no comment from the test user) assigned to the test user.
#[tokio::test]
#[ignore]
async fn rules_plugin_refusals() {
    let c = client();
    clean(&c).await;
    let id = |v: &str| std::env::var(v).unwrap_or_else(|_| panic!("set {v}")).parse::<u64>().unwrap();
    let (general, tiny) = (id("OP_GENERAL_TASK"), id("OP_TINY_TASK"));
    let mut core = Core::new(None, 10 * 60);
    core.connect(c.clone()).await.unwrap();

    // General work: no note refused before anything is sent; a note is fine.
    assert!(core.start(general, "").await.unwrap_err().contains("needs a note"));
    // A timer started elsewhere without a note: the stop is refused, the timer keeps running.
    let raw = c.start_timer(general, chrono::Local::now().date_naive(), "").await.unwrap();
    core.refresh().await.unwrap();
    assert!(matches!(core.stop(Utc::now(), StopInput::default()).await.unwrap(), Stop::Refused(_)));
    assert!(core.timer.is_some() && core.stop_problem.is_some());
    // A team member's name in the note: the server refuses, the timer still runs.
    let named = StopInput { comment: Some("Jonas fixed the menu".into()), ..StopInput::default() };
    let Stop::Refused(msgs) = core.stop(Utc::now(), named).await.unwrap() else { panic!("not refused") };
    println!("server said: {msgs:?}");
    assert!(c.running_timer().await.unwrap().is_some_and(|t| t.id == raw.id));
    let fixed = StopInput { comment: Some("Fixed the menu".into()), ..StopInput::default() };
    assert!(matches!(core.stop(Utc::now(), fixed).await.unwrap(), Stop::Done));
    assert!(core.stop_problem.is_none());

    // Over the estimate while away: the idle stop is refused, so the timer keeps running and
    // the user is asked when back; with a reason (a task comment) it stops at the idle start.
    core.start(tiny, "Small fix").await.unwrap();
    let entry = core.timer.as_ref().unwrap().id;
    let start = core.session.as_ref().unwrap().started_at;
    let away = start + chrono::Duration::seconds(90);
    core.on_idle_event(&IdleEvent::Idle { since: away }).await.unwrap();
    let problem = core.stop_problem.clone().expect("refused");
    assert!(problem.needs_reason(), "{problem:?}");
    assert_eq!(problem.until, Some(away), "the stop stays at the idle start");
    assert!(core.timer.is_some());
    assert!(core.on_idle_event(&IdleEvent::Active { at: away + chrono::Duration::minutes(20) }).await.unwrap());
    let with_reason = StopInput { reason: Some("The fix also needs a migration, about 1 more hour".into()), ..StopInput::default() };
    assert!(matches!(core.stop(Utc::now(), with_reason).await.unwrap(), Stop::Done));
    assert_eq!(stored(&c, entry).await, ("PT2M".into(), "Small fix".into(), false));
    let back = core.idle_stop.clone().unwrap();
    assert_eq!((back.from, back.minutes()), (away, Some(20)));
    core.resolve_idle_stop(None, None, None).await.unwrap();

    // Refused while being sent from the queue: sending pauses, the note is fixed, it goes up.
    core.client = Some(dead_client());
    core.offline = true;
    core.start(general, "Weekly sync with Jonas").await.unwrap();
    assert!(matches!(core.stop(Utc::now(), StopInput::default()).await.unwrap(), Stop::Done));
    core.client = Some(c.clone());
    core.refresh().await.unwrap();
    let refusal = core.refusal.clone().expect("refused on resend");
    println!("queued action refused: {:?}", refusal.messages);
    assert!(core.queue.is_empty());
    core.retry_refused(Some("Weekly sync".into()), None, None).await.unwrap();
    assert!(core.refusal.is_none() && core.queue.is_empty());
}

/// Offline: actions queue with their real times and go up in order when the server answers.
#[tokio::test]
#[ignore]
async fn offline_queue() {
    let c = client();
    clean(&c).await;
    let tasks: Vec<_> = c.search_tasks("").await.unwrap().into_iter()
        .filter(|t| !t.subject.contains("General work") && !t.subject.contains("Tiny") && t.subject != "Breaks")
        .collect();
    let (a, b) = (&tasks[0], &tasks[1]);
    let mut core = Core::new(None, 10 * 60);
    core.connect(c.clone()).await.unwrap();
    core.remember_tasks(&tasks);

    // Network gone: start, edit the note, stop. Nothing reaches the server; the timer runs locally.
    core.client = Some(dead_client());
    core.start(a.id, "Offline work").await.unwrap();
    assert!(core.offline);
    assert_eq!(core.timer.as_ref().unwrap().id, 0);
    core.set_comment("Offline work, part 2").await.unwrap();
    assert_eq!(core.queue.len(), 2);
    assert!(matches!(core.stop(Utc::now(), StopInput::default()).await.unwrap(), Stop::Done));
    // Started and stopped offline: one finished entry waits.
    assert_eq!(core.queue.len(), 1);
    // Start another task while still offline; it keeps running locally.
    core.start(b.id, "Second offline task").await.unwrap();
    assert_eq!(core.queue.len(), 2);
    assert!(c.running_timer().await.unwrap().is_none(), "nothing sent yet");

    // Back online: the queue goes up in order.
    core.client = Some(c.clone());
    core.refresh().await.unwrap();
    assert!(core.queue.is_empty() && core.refusal.is_none() && !core.offline);
    let running = c.running_timer().await.unwrap().expect("second timer started on the server");
    assert_eq!((running.task.id, running.comment.as_str()), (b.id, "Second offline task"));
    assert_eq!(core.timer.as_ref().unwrap().id, running.id);
    let me = core.user.as_ref().unwrap().id;
    let logged = c.time_entry_raw(running.id - 1).await.unwrap();
    assert_eq!(logged["comment"]["raw"], "Offline work, part 2");
    assert_eq!(logged["_links"]["user"]["href"], format!("/api/v3/users/{me}"));

    // A timer the server knows, stopped while offline: the stop waits with its hours.
    core.client = Some(dead_client());
    core.offline = true;
    assert!(matches!(core.stop(Utc::now(), StopInput::default()).await.unwrap(), Stop::Done));
    assert!(core.timer.is_none());
    assert!(c.running_timer().await.unwrap().is_some(), "still running on the server");
    core.client = Some(c.clone());
    core.refresh().await.unwrap();
    assert!(c.running_timer().await.unwrap().is_none(), "stopped once back online");
    assert_eq!(stored(&c, running.id).await, ("PT1M".into(), "Second offline task".into(), false));
}

/// "Break" on return: the away time goes to the task titled "Breaks" with the note, and the
/// timer can start again. Needs an open task titled "Breaks" the test user can log time on.
#[tokio::test]
#[ignore]
async fn break_answer() {
    let c = client();
    clean(&c).await;
    let task = c.search_tasks("").await.unwrap().into_iter()
        .find(|t| !t.subject.contains("General work") && !t.subject.contains("Tiny") && t.subject != "Breaks")
        .unwrap();
    let mut core = Core::new(None, 10 * 60);
    core.connect(c.clone()).await.unwrap();
    let breaks = core.break_task.clone().expect("a task titled Breaks");

    core.start(task.id, "Login form").await.unwrap();
    let start = core.session.as_ref().unwrap().started_at;
    let m = |n| start + chrono::Duration::minutes(n);
    core.on_idle_event(&IdleEvent::Idle { since: m(30) }).await.unwrap();
    assert!(core.on_idle_event(&IdleEvent::Active { at: m(45) }).await.unwrap());
    core.resolve_idle_as_break("Doing Salah", Some("Login form".into())).await.unwrap();
    assert!(core.idle_stop.is_none());
    assert_eq!(core.timer.as_ref().unwrap().task.id, task.id, "started again");

    // The break entry was logged just before the restarted timer.
    let ids: Vec<u64> = (1..=3).map(|i| core.timer.as_ref().unwrap().id - i).collect();
    let mut found = false;
    for id in ids {
        if let Ok(v) = c.time_entry_raw(id).await {
            if v["comment"]["raw"] == "Doing Salah" {
                assert_eq!(v["hours"], "PT15M");
                assert!(v["_links"]["entity"]["href"].as_str().unwrap().ends_with(&format!("/{}", breaks.id)));
                found = true;
            }
        }
    }
    assert!(found, "break entry on the Breaks task");

    // The "Today" list and the task's hours.
    let today = c.my_entries_on(chrono::Local::now().date_naive()).await.unwrap();
    let brk = today.iter().find(|l| l.comment == "Doing Salah").expect("break in today's list");
    assert_eq!((brk.task.id, (brk.hours * 60.0).round() as i64), (breaks.id, 15));
    assert!(today.iter().all(|l| l.id != core.timer.as_ref().unwrap().id), "running timer not listed");
    let h = c.task_hours(task.id).await.unwrap();
    assert!(h.spent >= 0.5, "the 30 minutes before the break: {h:?}");
    clean(&c).await;
}

/// A timer stopped elsewhere (the browser): the app says so and changes nothing, now or from
/// the offline queue. Members get 403 when changing a stopped entry; the app must not loop.
#[tokio::test]
#[ignore]
async fn stopped_elsewhere() {
    let c = client();
    clean(&c).await;
    let task = c.search_tasks("").await.unwrap().into_iter()
        .find(|t| !t.subject.contains("General work") && !t.subject.contains("Tiny") && t.subject != "Breaks")
        .unwrap();
    let mut core = Core::new(None, 10 * 60);
    core.connect(c.clone()).await.unwrap();

    // Stopped in the browser, then "Stop" in the app before its next refresh.
    core.start(task.id, "Web stop").await.unwrap();
    let id = core.timer.as_ref().unwrap().id;
    c.stop_timer(id, 7, "Web stop").await.unwrap();
    let err = core.stop(Utc::now(), StopInput::default()).await.unwrap_err();
    assert!(err.contains("already stopped"), "{err}");
    assert!(core.timer.is_none());
    assert_eq!(stored(&c, id).await.0, "PT7M", "the browser's stop is kept");
    // Same for a note edit.
    core.start(task.id, "Second").await.unwrap();
    let id2 = core.timer.as_ref().unwrap().id;
    c.stop_timer(id2, 3, "Second").await.unwrap();
    assert!(core.set_comment("Changed").await.unwrap_err().contains("already stopped"));

    // A stop queued offline for a timer that is stopped elsewhere meanwhile: dropped, no refusal.
    core.start(task.id, "Third").await.unwrap();
    let id3 = core.timer.as_ref().unwrap().id;
    core.client = Some(dead_client());
    core.offline = true;
    assert!(matches!(core.stop(Utc::now(), StopInput::default()).await.unwrap(), Stop::Done));
    c.stop_timer(id3, 4, "Third").await.unwrap();
    core.client = Some(c.clone());
    core.refresh().await.unwrap();
    assert!(core.queue.is_empty() && core.refusal.is_none(), "dropped, not refused");
    assert!(core.notice.as_deref().unwrap_or("").contains("already stopped"));
    assert_eq!(stored(&c, id3).await.0, "PT4M");
}
