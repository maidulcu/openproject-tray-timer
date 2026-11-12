//! Idle detection: tells the app when the user stops and starts using mouse/keyboard.
//!
//! Only "idle since" and "active again at" timestamps come out of here. No keystrokes, no
//! window titles, no screenshots: the sources below only ask the OS how long it has been since
//! the last input event.
//!
//! Sources:
//! - Linux GNOME (Wayland and X11): D-Bus `org.gnome.Mutter.IdleMonitor`, event-driven
//!   (`AddIdleWatch` + `AddUserActiveWatch`).
//! - macOS: polls `CGEventSourceSecondsSinceLastEventType` (used for development).
//! - Anything else: reports itself unavailable; the timer still works without idle checks.
//! - Tests: send `IdleEvent`s into the same channel (see `Core::on_idle_event`).

use chrono::{DateTime, Utc};
use std::time::Duration;
use tokio::sync::{mpsc::UnboundedSender, oneshot};

#[derive(Debug, Clone, PartialEq)]
pub enum IdleEvent {
    Idle { since: DateTime<Utc> },
    Active { at: DateTime<Utc> },
    Unavailable(String),
}

/// Stops the watch when dropped.
pub struct IdleHandle {
    _stop: oneshot::Sender<()>,
}

/// Seconds since the last mouse/keyboard input, read once (`None` where unsupported).
/// Used for the "no timer running" reminder; nothing is stored.
pub async fn idle_seconds() -> Option<u64> {
    platform::idle_seconds().await
}

/// Start watching for idle periods of at least `threshold`.
pub fn spawn(threshold: Duration, tx: UnboundedSender<IdleEvent>) -> IdleHandle {
    let (stop_tx, stop_rx) = oneshot::channel();
    platform::spawn(threshold, tx, stop_rx);
    IdleHandle { _stop: stop_tx }
}

fn ago(d: Duration) -> DateTime<Utc> {
    crate::api::now() - chrono::Duration::from_std(d).unwrap_or_default()
}

/// Generic polling loop over "how long since the last input".
#[allow(dead_code)]
async fn poll_loop<F>(
    threshold: Duration,
    tx: UnboundedSender<IdleEvent>,
    mut stop: oneshot::Receiver<()>,
    idle_time: F,
) where
    F: Fn() -> Option<Duration>,
{
    let mut idle = false;
    let mut last = Duration::ZERO;
    loop {
        tokio::select! {
            _ = &mut stop => return,
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
        let Some(now_idle) = idle_time() else {
            let _ = tx.send(IdleEvent::Unavailable("cannot read idle time".into()));
            return;
        };
        if !idle && now_idle >= threshold {
            idle = true;
            let _ = tx.send(IdleEvent::Idle { since: ago(now_idle) });
        } else if idle && now_idle < last {
            idle = false;
            let _ = tx.send(IdleEvent::Active { at: ago(now_idle) });
        }
        last = now_idle;
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use futures_util::StreamExt;

    #[zbus::proxy(
        interface = "org.gnome.Mutter.IdleMonitor",
        default_service = "org.gnome.Mutter.IdleMonitor",
        default_path = "/org/gnome/Mutter/IdleMonitor/Core"
    )]
    trait IdleMonitor {
        fn get_idletime(&self) -> zbus::Result<u64>;
        fn add_idle_watch(&self, interval: u64) -> zbus::Result<u32>;
        fn add_user_active_watch(&self) -> zbus::Result<u32>;
        fn remove_watch(&self, id: u32) -> zbus::Result<()>;
        #[zbus(signal)]
        fn watch_fired(&self, id: u32) -> zbus::Result<()>;
    }

    pub async fn idle_seconds() -> Option<u64> {
        let conn = zbus::Connection::session().await.ok()?;
        let proxy = IdleMonitorProxy::new(&conn).await.ok()?;
        proxy.get_idletime().await.ok().map(|ms| ms / 1000)
    }

    pub fn spawn(threshold: Duration, tx: UnboundedSender<IdleEvent>, stop: oneshot::Receiver<()>) {
        tauri::async_runtime::spawn(async move {
            if let Err(e) = run(threshold, &tx, stop).await {
                let _ = tx.send(IdleEvent::Unavailable(format!(
                    "GNOME idle monitor not available ({e}). Idle checks need GNOME."
                )));
            }
        });
    }

    async fn run(
        threshold: Duration,
        tx: &UnboundedSender<IdleEvent>,
        mut stop: oneshot::Receiver<()>,
    ) -> zbus::Result<()> {
        let conn = zbus::Connection::session().await?;
        let proxy = IdleMonitorProxy::new(&conn).await?;
        let mut fired = proxy.receive_watch_fired().await?;
        let idle_id = proxy.add_idle_watch(threshold.as_millis() as u64).await?;
        let mut active_id: Option<u32> = None;
        loop {
            tokio::select! {
                _ = &mut stop => break,
                sig = fired.next() => {
                    let Some(sig) = sig else { break };
                    let id = sig.args()?.id;
                    if id == idle_id {
                        let ms = proxy.get_idletime().await.unwrap_or(threshold.as_millis() as u64);
                        let _ = tx.send(IdleEvent::Idle { since: ago(Duration::from_millis(ms)) });
                        active_id = Some(proxy.add_user_active_watch().await?);
                    } else if Some(id) == active_id {
                        active_id = None;
                        let _ = tx.send(IdleEvent::Active { at: crate::api::now() });
                    }
                }
            }
        }
        let _ = proxy.remove_watch(idle_id).await;
        if let Some(id) = active_id {
            let _ = proxy.remove_watch(id).await;
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
    }
    const COMBINED_SESSION_STATE: i32 = 0;
    const ANY_INPUT_EVENT: u32 = u32::MAX;

    fn idle_time() -> Option<Duration> {
        // SAFETY: plain C call with constant arguments, no pointers.
        let secs = unsafe { CGEventSourceSecondsSinceLastEventType(COMBINED_SESSION_STATE, ANY_INPUT_EVENT) };
        (secs.is_finite() && secs >= 0.0).then(|| Duration::from_secs_f64(secs))
    }

    pub async fn idle_seconds() -> Option<u64> {
        idle_time().map(|d| d.as_secs())
    }

    pub fn spawn(threshold: Duration, tx: UnboundedSender<IdleEvent>, stop: oneshot::Receiver<()>) {
        tauri::async_runtime::spawn(poll_loop(threshold, tx, stop, idle_time));
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use super::*;

    pub async fn idle_seconds() -> Option<u64> {
        None
    }

    pub fn spawn(_threshold: Duration, tx: UnboundedSender<IdleEvent>, _stop: oneshot::Receiver<()>) {
        let _ = tx.send(IdleEvent::Unavailable("idle checks are not supported on this system yet".into()));
    }
}
