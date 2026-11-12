//! Settings: server URL, idle threshold, work hours and reminders in a JSON file, the API key
//! in the system keyring (GNOME Keyring / KWallet via Secret Service on Linux, Keychain on
//! macOS). Also the recent tasks list.

use crate::api::Task;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const KEYRING_SERVICE: &str = "dwl-timer";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub server_url: String,
    /// Minutes without mouse/keyboard input before the timer stops (at most `MAX_IDLE_MINUTES`).
    pub idle_minutes: u32,
    /// Work hours, local time, Monday to Friday ("09:00", "18:00").
    pub work_start: String,
    pub work_end: String,
    /// Remind me when I use the computer during work hours with no timer running, and when a
    /// timer still runs after work hours.
    pub reminders: bool,
    pub start_at_login: bool,
}

/// The longest the idle threshold may be: away time beyond this always stops the timer.
pub const MAX_IDLE_MINUTES: u32 = 10;

impl Default for Settings {
    fn default() -> Self {
        Self {
            server_url: String::new(),
            idle_minutes: MAX_IDLE_MINUTES,
            work_start: "09:00".into(),
            work_end: "18:00".into(),
            reminders: true,
            start_at_login: true,
        }
    }
}

/// Parse "HH:MM".
pub fn parse_hm(s: &str) -> Result<chrono::NaiveTime, String> {
    chrono::NaiveTime::parse_from_str(s.trim(), "%H:%M").map_err(|_| format!("\"{s}\" is not a time like 09:00"))
}

pub fn settings_path(config_dir: &Path) -> PathBuf {
    config_dir.join("settings.json")
}

impl Settings {
    pub fn load(config_dir: &Path) -> Self {
        let mut s: Self = std::fs::read_to_string(settings_path(config_dir))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        s.idle_minutes = s.idle_minutes.clamp(1, MAX_IDLE_MINUTES);
        s
    }

    pub fn save(&self, config_dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(config_dir).map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(settings_path(config_dir), json).map_err(|e| e.to_string())
    }
}

/// The key is stored per server URL, so switching servers doesn't reuse the wrong key.
fn entry(server_url: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYRING_SERVICE, server_url).map_err(|e| format!("Keyring: {e}"))
}

pub fn load_api_key(server_url: &str) -> Option<String> {
    if server_url.is_empty() {
        return None;
    }
    entry(server_url).ok()?.get_password().ok()
}

pub fn save_api_key(server_url: &str, key: &str) -> Result<(), String> {
    entry(server_url)?.set_password(key).map_err(|e| format!("Keyring: {e}"))
}

pub fn delete_api_key(server_url: &str) -> Result<(), String> {
    match entry(server_url)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("Keyring: {e}")),
    }
}

/// A task used recently and the comment last used on it (pre-fills the next start).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Recent {
    pub task: Task,
    pub comment: String,
}

const MAX_RECENT: usize = 8;

pub fn load_recent(config_dir: &Path) -> Vec<Recent> {
    std::fs::read_to_string(config_dir.join("recent.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_recent(config_dir: &Path, recent: &[Recent]) {
    let res = std::fs::create_dir_all(config_dir)
        .and_then(|_| std::fs::write(config_dir.join("recent.json"), serde_json::to_string_pretty(recent).unwrap_or_default()));
    if let Err(e) = res {
        log::warn!("could not save recent tasks: {e}");
    }
}

/// Put `task` first. An empty comment keeps the one remembered for that task.
pub fn push_recent(recent: &mut Vec<Recent>, task: &Task, comment: &str) {
    let old = recent.iter().position(|r| r.task.id == task.id).map(|i| recent.remove(i));
    let comment = if comment.trim().is_empty() {
        old.map(|r| r.comment).unwrap_or_default()
    } else {
        comment.trim().to_string()
    };
    recent.insert(0, Recent { task: task.clone(), comment });
    recent.truncate(MAX_RECENT);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: u64) -> Task {
        Task { id, subject: format!("T{id}"), project: "P".into() }
    }

    #[test]
    fn recent_moves_to_front_keeps_comment_and_is_capped() {
        let mut r = vec![];
        for i in 0..10 {
            push_recent(&mut r, &task(i), &format!("c{i}"));
        }
        assert_eq!(r.len(), MAX_RECENT);
        assert_eq!(r[0].task.id, 9);
        push_recent(&mut r, &task(5), "");
        assert_eq!(r[0], Recent { task: task(5), comment: "c5".into() });
        assert_eq!(r.iter().filter(|x| x.task.id == 5).count(), 1);
    }

    #[test]
    fn old_settings_files_get_the_new_defaults_and_the_cap() {
        let dir = std::env::temp_dir().join(format!("dwl-timer-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(settings_path(&dir), r#"{"server_url":"https://x","idle_minutes":30}"#).unwrap();
        let s = Settings::load(&dir);
        assert_eq!(s.idle_minutes, MAX_IDLE_MINUTES);
        assert_eq!(s.work_start, "09:00");
        assert!(s.reminders && s.start_at_login);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn times() {
        assert!(parse_hm("09:00").is_ok());
        assert!(parse_hm("9am").is_err());
    }
}
