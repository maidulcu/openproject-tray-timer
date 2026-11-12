//! DWL timer: a tray timer for OpenProject with idle detection.
//!
//! Tauri glue: tray menu, the one window (timer / away / settings views), the idle source,
//! reminders and the background refresh. The behaviour itself is in `core`.

pub mod api;
pub mod core;
pub mod idle;
pub mod queue;
pub mod remind;
pub mod settings;
pub mod tracker;

use crate::api::{Client, Logged, Task, TaskHours, User};
use crate::core::{Core, Stop, StopInput};
use crate::idle::{IdleEvent, IdleHandle};
use crate::remind::{Nudge, Reminders};
use crate::settings::{Recent, Settings, MAX_IDLE_MINUTES};
use chrono::{DateTime, Local, Utc};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

const TRAY_ID: &str = "main";
const WINDOW: &str = "main";
/// Recent tasks shown in the tray menu.
const TRAY_RECENT: usize = 5;
const TICK_SECS: i64 = 30;

struct IdleRuntime {
    handle: Option<IdleHandle>,
    threshold_secs: i64,
    status: String,
}

struct App {
    core: tokio::sync::Mutex<Core>,
    settings: Mutex<Settings>,
    recent: Mutex<Vec<Recent>>,
    reminders: Mutex<Reminders>,
    config_dir: PathBuf,
    idle_tx: UnboundedSender<IdleEvent>,
    idle: Mutex<IdleRuntime>,
    last_error: Mutex<Option<String>>,
    /// View the window should show next, and a task to pre-select (read by the page).
    view: Mutex<String>,
    pick: Mutex<Option<Task>>,
    /// A newer version found by the updater, ready to install.
    update: tokio::sync::Mutex<Option<tauri_plugin_updater::Update>>,
    update_version: Mutex<Option<String>>,
    /// Last tray title and tooltip set.
    tray_text: Mutex<(String, String)>,
    /// Today's finished entries, and the running task's hours against its estimate.
    today: Mutex<Vec<Logged>>,
    task_hours: Mutex<Option<TaskHours>>,
}

#[derive(Serialize)]
struct TimerView {
    entry_id: u64,
    task: Task,
    comment: String,
    started_at: DateTime<Utc>,
    worked_secs: i64,
    idle_now: bool,
}

#[derive(Serialize)]
struct IdleStopView {
    task: Task,
    comment: String,
    from: DateTime<Utc>,
    to: Option<DateTime<Utc>>,
    minutes: Option<i64>,
}

#[derive(Serialize)]
struct StopProblemView {
    messages: Vec<String>,
    needs_reason: bool,
    until: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
struct RefusalView {
    action: String,
    note: Option<String>,
    minutes: Option<i64>,
    messages: Vec<String>,
    needs_reason: bool,
}

#[derive(Serialize)]
struct Snapshot {
    /// Seconds this computer's clock is behind the server's (negative = ahead).
    clock_offset_secs: i64,
    configured: bool,
    settings: Settings,
    max_idle_minutes: u32,
    user: Option<User>,
    timer: Option<TimerView>,
    idle_stop: Option<IdleStopView>,
    stop_problem: Option<StopProblemView>,
    offline: bool,
    /// Actions waiting for the server, described for people.
    waiting: Vec<String>,
    refusal: Option<RefusalView>,
    update_version: Option<String>,
    version: String,
    /// The server has a "Breaks" task, so the away question offers "Break".
    break_available: bool,
    today: Vec<Logged>,
    /// Today's finished entries plus the running timer, breaks left out (seconds).
    today_secs: i64,
    task_hours: Option<TaskHours>,
    recent: Vec<Recent>,
    idle_status: String,
    error: Option<String>,
}

/// Idle threshold in seconds. `DWL_TIMER_IDLE_SECONDS` overrides it for testing.
fn idle_threshold_secs(s: &Settings) -> i64 {
    std::env::var("DWL_TIMER_IDLE_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(s.idle_minutes.clamp(1, MAX_IDLE_MINUTES) as i64 * 60)
}

fn fmt_hm(secs: i64) -> String {
    format!("{}:{:02}", secs / 3600, (secs % 3600) / 60)
}

fn short(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    }
}

async fn snapshot(app: &AppHandle) -> Snapshot {
    let st = app.state::<App>();
    let settings = st.settings.lock().unwrap().clone();
    let recent = st.recent.lock().unwrap().clone();
    let core = st.core.lock().await;
    let now = crate::api::now();
    let timer = match (&core.timer, &core.session) {
        (Some(t), Some(s)) => Some(TimerView {
            entry_id: t.id,
            task: t.task.clone(),
            comment: t.comment.clone(),
            started_at: t.started_at,
            worked_secs: s.worked_secs(now),
            idle_now: s.idle_since.is_some(),
        }),
        _ => None,
    };
    let idle_stop = core.idle_stop.as_ref().map(|s| IdleStopView {
        task: s.task.clone(),
        comment: s.comment.clone(),
        from: s.from,
        to: s.to,
        minutes: s.minutes(),
    });
    let stop_problem = core.stop_problem.as_ref().map(|p| StopProblemView {
        messages: p.messages.clone(),
        needs_reason: p.needs_reason(),
        until: p.until,
    });
    let refusal = core.refusal.as_ref().map(|r| RefusalView {
        action: r.action.describe(),
        note: r.action.note().map(str::to_string),
        minutes: r.action.minutes(),
        messages: r.messages.clone(),
        needs_reason: core::needs_reason(&r.messages),
    });
    let idle_status = st.idle.lock().unwrap().status.clone();
    let error = st.last_error.lock().unwrap().clone();
    let update_version = st.update_version.lock().unwrap().clone();
    let today = st.today.lock().unwrap().clone();
    let logged: f64 = today.iter().filter(|l| !tracker::is_break_task(&l.task.subject)).map(|l| l.hours).sum();
    let today_secs = (logged * 3600.0) as i64 + timer.as_ref().map_or(0, |t| t.worked_secs);
    let task_hours = st
        .task_hours
        .lock()
        .unwrap()
        .clone()
        .filter(|h| timer.as_ref().is_some_and(|t| t.task.id == h.task_id));
    Snapshot {
        clock_offset_secs: crate::api::clock_offset_secs(),
        configured: core.client.is_some(),
        settings,
        max_idle_minutes: MAX_IDLE_MINUTES,
        user: core.user.clone(),
        timer,
        idle_stop,
        stop_problem,
        offline: core.offline,
        waiting: core.queue.iter().map(|a| a.describe()).collect(),
        refusal,
        update_version,
        version: app.package_info().version.to_string(),
        break_available: core.break_task.is_some(),
        today,
        today_secs,
        task_hours,
        recent,
        idle_status,
        error,
    }
}

/// Idle checks run only while a timer runs, or until the user is back after an idle stop.
fn sync_idle(app: &AppHandle, watch: bool) {
    let st = app.state::<App>();
    let threshold = idle_threshold_secs(&st.settings.lock().unwrap());
    let mut rt = st.idle.lock().unwrap();
    if !watch {
        rt.handle = None;
        rt.status = "off (no timer running)".into();
    } else if rt.handle.is_none() || rt.threshold_secs != threshold {
        rt.handle = Some(idle::spawn(
            std::time::Duration::from_secs(threshold as u64),
            st.idle_tx.clone(),
        ));
        rt.threshold_secs = threshold;
        rt.status = format!("watching (timer stops after {} min without input)", (threshold + 59) / 60);
    }
}

fn build_menu(app: &AppHandle, snap: &Snapshot) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::new(app)?;
    let status_text = match (&snap.timer, snap.configured) {
        (_, false) => "Not set up".to_string(),
        (Some(t), _) => format!("● {} — {}", short(&t.task.subject, 40), fmt_hm(t.worked_secs)),
        (None, _) => "No timer running".to_string(),
    };
    menu.append(&MenuItem::with_id(app, "status", status_text, false, None::<&str>)?)?;
    if snap.configured {
        let today = format!("Today: {} logged", fmt_hm(snap.today_secs));
        menu.append(&MenuItem::with_id(app, "today", today, false, None::<&str>)?)?;
    }
    if snap.offline || !snap.waiting.is_empty() {
        let text = match (snap.offline, snap.waiting.len()) {
            (true, 0) => "Offline: the timer keeps working".to_string(),
            (true, n) => format!("Offline: {n} change(s) waiting to be sent"),
            (false, n) => format!("{n} change(s) waiting to be sent"),
        };
        menu.append(&MenuItem::with_id(app, "offline", text, false, None::<&str>)?)?;
    }
    if snap.refusal.is_some() {
        menu.append(&MenuItem::with_id(app, "fix", "OpenProject refused a waiting change: fix…", true, None::<&str>)?)?;
    }
    if snap.stop_problem.is_some() {
        menu.append(&MenuItem::with_id(app, "fix", "The server refused to stop the timer: fix…", true, None::<&str>)?)?;
    }
    if let Some(st) = snap.idle_stop.as_ref().filter(|s| s.to.is_some()) {
        let text = format!("Timer stopped while you were away {} min: answer…", st.minutes.unwrap_or(0));
        menu.append(&MenuItem::with_id(app, "away", text, true, None::<&str>)?)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    if snap.timer.is_some() {
        menu.append(&MenuItem::with_id(app, "stop", "Stop timer", true, None::<&str>)?)?;
        menu.append(&MenuItem::with_id(app, "open", "Switch task or edit comment…", true, None::<&str>)?)?;
    } else {
        menu.append(&MenuItem::with_id(app, "open", "Start timer…", snap.configured, None::<&str>)?)?;
    }
    let running_id = snap.timer.as_ref().map(|t| t.task.id);
    let recent: Vec<_> = snap.recent.iter().filter(|r| Some(r.task.id) != running_id).take(TRAY_RECENT).collect();
    if snap.configured && !recent.is_empty() {
        menu.append(&PredefinedMenuItem::separator(app)?)?;
        let head = if snap.timer.is_some() { "Switch to:" } else { "Start on:" };
        menu.append(&MenuItem::with_id(app, "recent-head", head, false, None::<&str>)?)?;
        for r in recent {
            let text = format!("#{} {}", r.task.id, short(&r.task.subject, 45));
            menu.append(&MenuItem::with_id(app, format!("recent:{}", r.task.id), text, true, None::<&str>)?)?;
        }
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    if let Some(v) = &snap.update_version {
        let text = format!("Update to {v}: install and restart");
        menu.append(&MenuItem::with_id(app, "update", text, true, None::<&str>)?)?;
    }
    menu.append(&MenuItem::with_id(app, "refresh", "Refresh from server", snap.configured, None::<&str>)?)?;
    menu.append(&MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?)?;
    menu.append(&MenuItem::with_id(app, "quit", "Quit (a running timer keeps running)", true, None::<&str>)?)?;
    Ok(menu)
}

/// The tray's title and tooltip. Set only when they change, so it can run every few seconds.
fn set_tray_text(app: &AppHandle, tray: &tauri::tray::TrayIcon, timer: Option<(String, i64)>, configured: bool, offline: bool) {
    let mut title = match (&timer, configured) {
        (Some((_, secs)), _) => format!("● {}", fmt_hm(*secs)),
        (None, true) => "no timer".into(),
        (None, false) => String::new(),
    };
    if offline {
        title.push_str(" (offline)");
    }
    let tip = match &timer {
        Some((subject, secs)) => format!("DWL timer: {} ({})", subject, fmt_hm(*secs)),
        None => "DWL timer: no timer running".into(),
    };
    let st = app.state::<App>();
    let mut last = st.tray_text.lock().unwrap();
    if *last != (title.clone(), tip.clone()) {
        let _ = tray.set_title(Some(title.clone()));
        let _ = tray.set_tooltip(Some(tip.clone()));
        *last = (title, tip);
    }
}

/// Keep the tray's minutes in step with the window's clock (the full refresh runs every 30 s).
async fn refresh_tray_text(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else { return };
    let (timer, configured, offline) = {
        let st = app.state::<App>();
        let core = st.core.lock().await;
        let timer = core.timer.as_ref().zip(core.session.as_ref())
            .map(|(t, s)| (t.task.subject.clone(), s.worked_secs(crate::api::now())));
        (timer, core.client.is_some(), core.offline)
    };
    set_tray_text(app, &tray, timer, configured, offline);
}

/// Push the new state to the tray and the window.
async fn publish(app: &AppHandle) {
    let snap = snapshot(app).await;
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        if let Ok(menu) = build_menu(app, &snap) {
            let _ = tray.set_menu(Some(menu));
        }
        let timer = snap.timer.as_ref().map(|t| (t.task.subject.clone(), t.worked_secs));
        set_tray_text(app, &tray, timer, snap.configured, snap.offline);
    }
    let _ = app.emit_to(WINDOW, "state", &snap);
}

/// After any change: remember the running task as recent, watch idle if needed, publish.
async fn after_change(app: &AppHandle) {
    let st = app.state::<App>();
    let (running, watch) = {
        let mut core = st.core.lock().await;
        if let Some(n) = core.notice.take() {
            set_error(app, Some(n));
        }
        let waiting = core.idle_stop.as_ref().is_some_and(|s| s.to.is_none());
        (core.timer.as_ref().map(|t| (t.task.clone(), t.comment.clone())), core.timer.is_some() || waiting)
    };
    if let Some((task, comment)) = running {
        let mut recent = st.recent.lock().unwrap();
        let before = recent.clone();
        settings::push_recent(&mut recent, &task, &comment);
        if *recent != before {
            settings::save_recent(&st.config_dir, &recent);
        }
    }
    sync_idle(app, watch);
    publish(app).await;
}

fn show_view(app: &AppHandle, view: &str) {
    *app.state::<App>().view.lock().unwrap() = view.to_string();
    if let Some(w) = app.get_webview_window(WINDOW) {
        let _ = w.set_always_on_top(view == "away");
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
    let _ = app.emit_to(WINDOW, "navigate", view);
}

fn show_pick(app: &AppHandle, task: Task) {
    *app.state::<App>().pick.lock().unwrap() = Some(task.clone());
    let _ = app.emit_to(WINDOW, "pick", &task);
    show_view(app, "timer");
}

fn set_error(app: &AppHandle, err: Option<String>) {
    if let Some(e) = &err {
        eprintln!("dwl-timer: {e}");
    }
    *app.state::<App>().last_error.lock().unwrap() = err;
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        log::warn!("notification failed: {e}");
    }
}

/// Start at login only in installed (release) builds, so development runs don't register.
fn sync_autostart(app: &AppHandle, on: bool) {
    if cfg!(debug_assertions) {
        return;
    }
    use tauri_plugin_autostart::ManagerExt;
    let al = app.autolaunch();
    let res = match (on, al.is_enabled().unwrap_or(false)) {
        (true, false) => al.enable(),
        (false, true) => al.disable(),
        _ => Ok(()),
    };
    if let Err(e) = res {
        log::warn!("start at login: {e}");
    }
}

/// Reload today's entries and the running task's hours (quietly skipped when offline).
async fn refresh_today(app: &AppHandle) {
    let st = app.state::<App>();
    let (client, task) = {
        let core = st.core.lock().await;
        (core.client.clone(), core.timer.as_ref().map(|t| t.task.id).filter(|id| *id != queue::PENDING))
    };
    let Some(client) = client else { return };
    if let Ok(entries) = client.my_entries_on(Local::now().date_naive()).await {
        *st.today.lock().unwrap() = entries;
    }
    let hours = match task {
        Some(id) => client.task_hours(id).await.ok(),
        None => None,
    };
    *st.task_hours.lock().unwrap() = hours;
    publish(app).await;
}

/// Open a page of the OpenProject server in the browser (only that server: `path` starts with /).
#[tauri::command]
async fn open_in_openproject(app: AppHandle, path: String) -> Result<(), String> {
    if !path.starts_with('/') || path.starts_with("//") {
        return Err("Not an OpenProject page.".into());
    }
    let base = app.state::<App>().settings.lock().unwrap().server_url.clone();
    if base.is_empty() {
        return Err("Set the server URL first.".into());
    }
    use tauri_plugin_opener::OpenerExt;
    app.opener().open_url(format!("{base}{path}"), None::<&str>).map_err(|e| e.to_string())
}

/// Ask the update server for a newer version (installed builds only).
async fn check_update(app: &AppHandle) {
    if cfg!(debug_assertions) {
        return;
    }
    use tauri_plugin_updater::UpdaterExt;
    let updater = match app.updater() {
        Ok(u) => u,
        Err(e) => return log::warn!("updater: {e}"),
    };
    match updater.check().await {
        Ok(Some(update)) => {
            let st = app.state::<App>();
            let v = update.version.clone();
            let first = st.update_version.lock().unwrap().replace(v.clone()).as_deref() != Some(v.as_str());
            *st.update.lock().await = Some(update);
            if first {
                notify(app, "DWL timer update", &format!("Version {v} is ready. Install it from the tray menu."));
            }
            publish(app).await;
        }
        Ok(None) => {}
        Err(e) => log::warn!("update check failed: {e}"),
    }
}

// ---- commands (called from ui/main.js) ----

/// Download and install the waiting update (Ubuntu asks for the password), then restart.
/// A running timer keeps running on the server and waiting changes are saved.
#[tauri::command]
async fn install_update(app: AppHandle) -> Result<(), String> {
    let update = app.state::<App>().update.lock().await.take().ok_or("No update is waiting.")?;
    if let Err(e) = update.download_and_install(|_, _| {}, || {}).await {
        let msg = format!("The update could not be installed: {e}");
        set_error(&app, Some(msg.clone()));
        publish(&app).await;
        return Err(msg);
    }
    app.restart();
}

#[tauri::command]
async fn get_state(app: AppHandle) -> Snapshot {
    snapshot(&app).await
}

#[tauri::command]
fn take_view(state: State<'_, App>) -> String {
    std::mem::replace(&mut *state.view.lock().unwrap(), "timer".into())
}

#[tauri::command]
fn take_pick(state: State<'_, App>) -> Option<Task> {
    state.pick.lock().unwrap().take()
}

#[tauri::command]
fn hide_window(app: AppHandle) {
    if let Some(w) = app.get_webview_window(WINDOW) {
        let _ = w.set_always_on_top(false);
        let _ = w.hide();
    }
}

#[tauri::command]
async fn save_settings(app: AppHandle, server_url: String, api_key: String, new: Settings) -> Result<Snapshot, String> {
    let st = app.state::<App>();
    let url = api::normalize_base_url(&server_url)?;
    if !(1..=MAX_IDLE_MINUTES).contains(&new.idle_minutes) {
        return Err(format!("Stop after: pick 1 to {MAX_IDLE_MINUTES} minutes."));
    }
    let (start, end) = (settings::parse_hm(&new.work_start)?, settings::parse_hm(&new.work_end)?);
    if start >= end {
        return Err("Work hours: the end must be after the start.".into());
    }
    let old = st.settings.lock().unwrap().clone();
    let key = if api_key.trim().is_empty() {
        settings::load_api_key(&url).ok_or("Enter your API key (My account > Access tokens).")?
    } else {
        api_key.trim().to_string()
    };
    let client = Client::new(&url, &key).map_err(|e| e.to_string())?;
    // Check the key before saving anything.
    client.me().await.map_err(|e| e.to_string())?;
    settings::save_api_key(&url, &key)?;
    if !old.server_url.is_empty() && old.server_url != url {
        let _ = settings::delete_api_key(&old.server_url);
    }
    let new = Settings {
        server_url: url,
        work_start: start.format("%H:%M").to_string(),
        work_end: end.format("%H:%M").to_string(),
        ..new
    };
    new.save(&st.config_dir)?;
    *st.settings.lock().unwrap() = new.clone();
    sync_autostart(&app, new.start_at_login);
    {
        let mut core = st.core.lock().await;
        core.idle_threshold_secs = idle_threshold_secs(&new);
        core.connect(client).await?;
    }
    set_error(&app, None);
    after_change(&app).await;
    Ok(snapshot(&app).await)
}

/// Search the server; offline, search the tasks the app has seen (recent, assigned, found).
#[tauri::command]
async fn search_tasks(state: State<'_, App>, text: String) -> Result<Vec<Task>, String> {
    let client = state.core.lock().await.client()?.clone();
    match client.search_tasks(&text).await {
        Ok(tasks) => {
            state.core.lock().await.remember_tasks(&tasks);
            Ok(tasks)
        }
        Err(e) if e.is_offline() => {
            let mut core = state.core.lock().await;
            core.offline = true;
            let recent: Vec<Task> = state.recent.lock().unwrap().iter().map(|r| r.task.clone()).collect();
            core.remember_tasks(&recent);
            Ok(core.cached_tasks(&text))
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Run a core action, then refresh the tray/window; errors also go to the snapshot.
macro_rules! act {
    ($app:expr, |$core:ident| $body:expr) => {{
        let res = {
            let st = $app.state::<App>();
            let mut $core = st.core.lock().await;
            $body.await
        };
        set_error(&$app, res.as_ref().err().cloned());
        after_change(&$app).await;
        refresh_today(&$app).await;
        res.map(|_| ())
    }};
}

#[tauri::command]
async fn retry_refused(app: AppHandle, note: Option<String>, reason: Option<String>, minutes: Option<i64>) -> Result<(), String> {
    act!(app, |core| core.retry_refused(note, reason, minutes))
}

#[tauri::command]
async fn discard_refused(app: AppHandle) -> Result<(), String> {
    let res = app.state::<App>().core.lock().await.discard_refused();
    set_error(&app, res.as_ref().err().cloned());
    after_change(&app).await;
    res
}

#[tauri::command]
async fn refresh(app: AppHandle) -> Result<(), String> {
    act!(app, |core| core.refresh())
}

#[tauri::command]
async fn start_timer(app: AppHandle, task_id: u64, comment: String) -> Result<(), String> {
    act!(app, |core| core.start(task_id, &comment))
}

#[tauri::command]
async fn retarget_timer(app: AppHandle, task_id: u64) -> Result<(), String> {
    act!(app, |core| core.retarget(task_id))
}

#[tauri::command]
async fn set_comment(app: AppHandle, comment: String) -> Result<(), String> {
    act!(app, |core| core.set_comment(&comment))
}

/// Run a stop; returns whether the timer stopped (a refusal is in the snapshot's `stop_problem`).
async fn run_stop(app: &AppHandle, res: Result<Stop, String>) -> Result<bool, String> {
    set_error(app, res.as_ref().err().cloned());
    after_change(app).await;
    refresh_today(app).await;
    res.map(|s| matches!(s, Stop::Done))
}

#[tauri::command]
async fn stop_timer(app: AppHandle, input: Option<StopInput>) -> Result<bool, String> {
    let res = app.state::<App>().core.lock().await.stop(crate::api::now(), input.unwrap_or_default()).await;
    run_stop(&app, res).await
}

#[tauri::command]
async fn stop_and_start(app: AppHandle, task_id: u64, comment: String, input: Option<StopInput>) -> Result<bool, String> {
    let res = {
        let st = app.state::<App>();
        let mut core = st.core.lock().await;
        core.stop_and_start(task_id, &comment, crate::api::now(), input.unwrap_or_default()).await
    };
    run_stop(&app, res).await
}

#[tauri::command]
async fn resolve_idle_stop(
    app: AppHandle,
    meeting_task_id: Option<u64>,
    meeting_comment: Option<String>,
    restart_comment: Option<String>,
    reason: Option<String>,
) -> Result<(), String> {
    let meeting = meeting_task_id.map(|id| (id, meeting_comment.unwrap_or_default()));
    act!(app, |core| core.resolve_idle_stop(meeting, restart_comment, reason))
}

#[tauri::command]
async fn resolve_idle_break(app: AppHandle, note: String, restart_comment: Option<String>) -> Result<(), String> {
    act!(app, |core| core.resolve_idle_as_break(&note, restart_comment))?;
    if app.state::<App>().core.lock().await.needs_answer() {
        show_view(&app, "away");
    }
    Ok(())
}

/// Development aid: pretend the user was away for `minutes`, ending now.
/// Only works when the app runs with `DWL_TIMER_DEBUG=1`.
#[tauri::command]
fn debug_fake_away(state: State<'_, App>, minutes: i64) -> Result<(), String> {
    if std::env::var("DWL_TIMER_DEBUG").as_deref() != Ok("1") {
        return Err("debug commands are off".into());
    }
    let now = crate::api::now();
    let tx = &state.idle_tx;
    tx.send(IdleEvent::Idle { since: now - chrono::Duration::minutes(minutes) }).map_err(|e| e.to_string())?;
    tx.send(IdleEvent::Active { at: now }).map_err(|e| e.to_string())?;
    Ok(())
}

// ---- tray menu ----

fn on_menu(app: &AppHandle, id: &str) {
    match id {
        "open" | "fix" => show_view(app, "timer"),
        "update" => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                if install_update(app.clone()).await.is_err() {
                    show_view(&app, "settings");
                }
            });
        }
        "away" => show_view(app, "away"),
        "settings" => show_view(app, "settings"),
        "quit" => app.exit(0),
        "stop" | "refresh" => {
            let app = app.clone();
            let id = id.to_string();
            tauri::async_runtime::spawn(async move {
                if id == "stop" {
                    // Refused or failed: open the window, which shows why.
                    if !matches!(stop_timer(app.clone(), None).await, Ok(true)) {
                        show_view(&app, "timer");
                    }
                } else {
                    let _ = refresh(app.clone()).await;
                }
            });
        }
        _ => {
            let Some(task_id) = id.strip_prefix("recent:").and_then(|s| s.parse::<u64>().ok()) else { return };
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let st = app.state::<App>();
                let Some(r) = st.recent.lock().unwrap().iter().find(|r| r.task.id == task_id).cloned() else { return };
                let running = st.core.lock().await.timer.is_some();
                // One click starts with the comment used last time on that task; switching
                // (or no remembered comment) opens the window to confirm.
                if running || tracker::clean_comment(&r.comment, &r.task.subject).is_err() {
                    show_pick(&app, r.task);
                } else if start_timer(app.clone(), r.task.id, r.comment).await.is_err() {
                    show_view(&app, "timer");
                }
            });
        }
    }
}

// ---- background tasks ----

fn spawn_idle_consumer(app: AppHandle, mut rx: tokio::sync::mpsc::UnboundedReceiver<IdleEvent>) {
    tauri::async_runtime::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if let IdleEvent::Unavailable(msg) = &ev {
                log::warn!("idle detection unavailable: {msg}");
                app.state::<App>().idle.lock().unwrap().status = format!("unavailable: {msg}");
                publish(&app).await;
                continue;
            }
            let res = app.state::<App>().core.lock().await.on_idle_event(&ev).await;
            handle_due(&app, res).await;
        }
    });
}

/// After an idle event or tick: show the away question if due, report errors, publish.
async fn handle_due(app: &AppHandle, res: Result<bool, String>) {
    match res {
        Ok(true) => {
            set_error(app, None);
            let st = app.state::<App>();
            let refused = {
                let core = st.core.lock().await;
                core.stop_problem.is_some() || core.refusal.is_some()
            };
            show_view(app, if refused { "timer" } else { "away" });
        }
        Ok(false) => {}
        Err(e) => set_error(app, Some(e)),
    }
    after_change(app).await;
}

async fn remind(app: &AppHandle) {
    let st = app.state::<App>();
    let s = st.settings.lock().unwrap().clone();
    let (configured, timer) = {
        let core = st.core.lock().await;
        let timer = core.timer.as_ref().zip(core.session.as_ref()).map(|(t, s)| (t.task.subject.clone(), s.worked_secs(crate::api::now())));
        (core.client.is_some(), timer)
    };
    if !configured {
        return;
    }
    let now = Local::now();
    // Idle time is read only for the "no timer" reminder: work hours, no timer running.
    let idle = if s.reminders && timer.is_none() && remind::in_work_hours(now, &s) {
        idle::idle_seconds().await
    } else {
        None
    };
    let nudge = st.reminders.lock().unwrap().tick(now, TICK_SECS, &s, timer.is_some(), idle);
    match (nudge, timer) {
        (Some(Nudge::NoTimer), _) => notify(app, "No timer running", "You're at the computer without a timer. Start one from the tray icon."),
        (Some(Nudge::StillRunning), Some((task, secs))) => notify(
            app,
            "Timer still running",
            &format!("{task} ({}). Stop it from the tray icon if you're done for the day.", fmt_hm(secs)),
        ),
        _ => {}
    }
}

fn spawn_tickers(app: AppHandle) {
    let tray_app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            refresh_tray_text(&tray_app).await;
        }
    });
    tauri::async_runtime::spawn(async move {
        let mut n: u64 = 0;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(TICK_SECS as u64)).await;
            n += 1;
            let has_client = app.state::<App>().core.lock().await.client.is_some();
            if !has_client {
                publish(&app).await;
                continue;
            }
            // Every 2 minutes pick up timers started or stopped in the web UI; every 30 s while
            // offline or while changes wait to be sent.
            let st = app.state::<App>();
            let busy = {
                let core = st.core.lock().await;
                core.offline || !core.queue.is_empty()
            };
            if n % 4 == 0 || busy {
                let _ = refresh(app.clone()).await;
            }
            // Sleep / restart gaps and stops that failed while offline.
            let res = app.state::<App>().core.lock().await.tick(crate::api::now()).await;
            handle_due(&app, res).await;
            remind(&app).await;
            // Updates: a minute after start, then every 6 hours.
            if n == 2 || n % 720 == 0 {
                check_update(&app).await;
            }
        }
    });
}

async fn connect_on_start(app: AppHandle) {
    let st = app.state::<App>();
    let s = st.settings.lock().unwrap().clone();
    sync_autostart(&app, s.start_at_login);
    // Development only: DWL_TIMER_DEBUG=1 + DWL_TIMER_API_KEY skips the keyring.
    let key = std::env::var("DWL_TIMER_API_KEY")
        .ok()
        .filter(|_| std::env::var("DWL_TIMER_DEBUG").as_deref() == Ok("1"))
        .or_else(|| settings::load_api_key(&s.server_url));
    let result = match key {
        Some(k) if !s.server_url.is_empty() => match Client::new(&s.server_url, &k) {
            Ok(c) => st.core.lock().await.connect(c).await,
            Err(e) => Err(e.to_string()),
        },
        _ => {
            eprintln!("dwl-timer: not set up (no server URL or no API key in the keyring)");
            show_view(&app, "settings");
            Ok(())
        }
    };
    set_error(&app, result.err());
    if st.core.lock().await.client.is_some() {
        // Time since the app last ran (shutdown, crash) counts as away; the user is here now.
        let now = crate::api::now();
        let mut core = st.core.lock().await;
        let gap = core.tick(now).await;
        let back = core.on_idle_event(&IdleEvent::Active { at: now }).await;
        drop(core);
        let due = match (gap, back) {
            (Err(e), _) | (_, Err(e)) => Err(e),
            (Ok(a), Ok(b)) => Ok(a || b),
        };
        handle_due(&app, due).await;
    } else {
        after_change(&app).await;
    }
    refresh_today(&app).await;
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // One copy only: opening the app again (menu, autostart, double click) shows the running
        // one instead of starting a second tray icon with its own timer.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_view(app, "timer")))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, None))
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let config_dir = app.path().app_config_dir()?;
            let data_dir = app.path().app_data_dir()?;
            let settings = Settings::load(&config_dir);
            let core = Core::new(Some(data_dir.join("state.json")), idle_threshold_secs(&settings));
            let _ = std::fs::create_dir_all(&data_dir);
            let _ = api::REQUEST_LOG.set(data_dir.join("requests.log"));
            let (idle_tx, idle_rx) = unbounded_channel();
            app.manage(App {
                core: tokio::sync::Mutex::new(core),
                settings: Mutex::new(settings),
                recent: Mutex::new(settings::load_recent(&config_dir)),
                reminders: Mutex::new(Reminders::default()),
                config_dir,
                idle_tx,
                idle: Mutex::new(IdleRuntime { handle: None, threshold_secs: 0, status: "off".into() }),
                last_error: Mutex::new(None),
                view: Mutex::new("timer".into()),
                pick: Mutex::new(None),
                update: tokio::sync::Mutex::new(None),
                update_version: Mutex::new(None),
                tray_text: Mutex::new(Default::default()),
                today: Mutex::new(vec![]),
                task_hours: Mutex::new(None),
            });

            let handle = app.handle().clone();
            TrayIconBuilder::with_id(TRAY_ID)
                .icon(app.default_window_icon().cloned().ok_or("no app icon")?)
                .tooltip("DWL timer")
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| on_menu(app, event.id.as_ref()))
                .build(app)?;

            spawn_idle_consumer(handle.clone(), idle_rx);
            spawn_tickers(handle.clone());
            tauri::async_runtime::spawn(connect_on_start(handle));
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window only hides it; the app lives in the tray.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.set_always_on_top(false);
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            take_view,
            take_pick,
            hide_window,
            save_settings,
            search_tasks,
            refresh,
            start_timer,
            retarget_timer,
            set_comment,
            stop_timer,
            stop_and_start,
            resolve_idle_stop,
            resolve_idle_break,
            retry_refused,
            discard_refused,
            install_update,
            open_in_openproject,
            debug_fake_away
        ])
        .build(tauri::generate_context!())
        .expect("error while building DWL timer")
        .run(|_app, event| {
            // Keep running in the tray when the window is closed.
            if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}
