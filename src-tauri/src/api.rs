//! OpenProject API v3 client: just the calls the timer needs.
//!
//! Tested against OpenProject 17.9. Notes from those tests:
//! - Starting a timer needs `spentOn`; only one timer per user can run.
//! - Stopping needs `hours`: OpenProject does not compute it, the client must send it.
//! - A server can refuse a save with HTTP 422 and say why (one `message`, or several in
//!   `_embedded.errors` for `MultipleErrors`). A refused stop leaves the timer running.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("Not set up: add the server URL and API key in Settings")]
    NotConfigured,
    #[error("Cannot reach the server: {0}")]
    Network(String),
    #[error("The API key was refused (401). Check it in Settings.")]
    Unauthorized,
    #[error("OpenProject said: {0}")]
    Server(String),
    /// Not allowed (HTTP 403) or not found (404): for a timer, it was stopped or deleted
    /// elsewhere (members can't change a stopped entry).
    #[error("OpenProject didn't allow it: {0}")]
    NotAllowed(String),
    /// The server refused the change (HTTP 422) for these reasons, in its own words.
    #[error("{}", .0.join("\n"))]
    Refused(Vec<String>),
    #[error("Unexpected answer from the server: {0}")]
    Parse(String),
}

pub type Result<T> = std::result::Result<T, ApiError>;

/// Technical log of requests (time, method, path, status; never bodies, notes or keys), for
/// finding out what happened on a tester's computer. Set once by the app; tests leave it unset.
pub static REQUEST_LOG: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

fn log_request(method: &str, url: &str, status: &str) {
    let Some(path) = REQUEST_LOG.get() else { return };
    // Keep it small: start over past 1 MB.
    if std::fs::metadata(path).map(|m| m.len() > 1_000_000).unwrap_or(false) {
        let _ = std::fs::rename(path, path.with_extension("log.old"));
    }
    let path_only = url.split('?').next().unwrap_or(url);
    let path_only = path_only.find("/api/").map_or(path_only, |i| &path_only[i..]);
    let line = format!("{} {} {} {}\n", chrono::Utc::now().to_rfc3339(), method, path_only, status);
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

impl ApiError {
    /// The server could not be reached (offline, DNS, timeout, server down): worth queueing.
    pub fn is_offline(&self) -> bool {
        matches!(self, ApiError::Network(_))
    }

    pub fn is_not_allowed(&self) -> bool {
        matches!(self, ApiError::NotAllowed(_))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct User {
    pub id: u64,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Task {
    pub id: u64,
    pub subject: String,
    pub project: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimeEntry {
    pub id: u64,
    pub ongoing: bool,
    pub task: Task,
    pub spent_on: String,
    /// When the timer started: `startTime` if the server tracks start times, else `createdAt`.
    pub started_at: DateTime<Utc>,
    pub comment: String,
}

#[derive(Clone)]
pub struct Client {
    base: String,
    api_key: String,
    http: reqwest::Client,
}

/// ISO 8601 duration in whole minutes, as OpenProject expects for `hours` (e.g. `PT1H7M`).
pub fn iso_minutes(minutes: i64) -> String {
    let m = minutes.max(0);
    match (m / 60, m % 60) {
        (0, mm) => format!("PT{mm}M"),
        (h, 0) => format!("PT{h}H"),
        (h, mm) => format!("PT{h}H{mm}M"),
    }
}

/// Normalise a user-typed server URL: trims, adds https:// if no scheme, drops trailing slashes.
pub fn normalize_base_url(raw: &str) -> std::result::Result<String, String> {
    let raw = raw.trim().trim_end_matches('/');
    if raw.is_empty() {
        return Err("Server URL is empty".into());
    }
    let with_scheme = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    };
    let parsed = url::Url::parse(&with_scheme).map_err(|e| format!("Invalid server URL: {e}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("Server URL must start with http:// or https://".into());
    }
    if parsed.scheme() == "http" && !is_local_host(&parsed) {
        return Err("Use https:// for this server: over plain http:// your API key would be sent unencrypted. \
                    (http:// is allowed only for localhost and private network addresses.)"
            .into());
    }
    Ok(with_scheme)
}

/// Loopback, private network addresses and local-only names (.local, .lan, .internal, .localhost),
/// where plain http doesn't cross the internet.
fn is_local_host(u: &url::Url) -> bool {
    match u.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback() || (ip.segments()[0] & 0xfe00) == 0xfc00,
        Some(url::Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            d == "localhost" || [".localhost", ".local", ".lan", ".internal", ".home.arpa"].iter().any(|s| d.ends_with(s))
        }
        None => false,
    }
}

fn filters(f: Value) -> String {
    f.to_string()
}

fn link_id(v: &Value) -> Option<u64> {
    v.get("href")?.as_str()?.rsplit('/').next()?.parse().ok()
}

fn parse_task(e: &Value) -> Result<Task> {
    Ok(Task {
        id: e["id"].as_u64().ok_or_else(|| ApiError::Parse("work package without id".into()))?,
        subject: e["subject"].as_str().unwrap_or_default().to_string(),
        project: e["_links"]["project"]["title"].as_str().unwrap_or_default().to_string(),
    })
}

fn parse_time_entry(e: &Value) -> Result<TimeEntry> {
    let id = e["id"].as_u64().ok_or_else(|| ApiError::Parse("time entry without id".into()))?;
    let links = &e["_links"];
    // OpenProject 17 links the logged item as `entity` (older versions: `workPackage`).
    let entity = if links["entity"].is_object() { &links["entity"] } else { &links["workPackage"] };
    let task = Task {
        id: link_id(entity).ok_or_else(|| ApiError::Parse("time entry without work package".into()))?,
        subject: entity["title"].as_str().unwrap_or_default().to_string(),
        project: links["project"]["title"].as_str().unwrap_or_default().to_string(),
    };
    let started = e["startTime"]
        .as_str()
        .or_else(|| e["createdAt"].as_str())
        .ok_or_else(|| ApiError::Parse("time entry without createdAt".into()))?;
    let started_at = DateTime::parse_from_rfc3339(started)
        .map_err(|err| ApiError::Parse(format!("bad timestamp {started}: {err}")))?
        .with_timezone(&Utc);
    Ok(TimeEntry {
        id,
        ongoing: e["ongoing"].as_bool().unwrap_or(false),
        task,
        spent_on: e["spentOn"].as_str().unwrap_or_default().to_string(),
        started_at,
        comment: e["comment"]["raw"].as_str().unwrap_or_default().to_string(),
    })
}

/// The reasons in a 422 answer: all of `_embedded.errors[].message` when there are several,
/// otherwise the top-level `message`.
fn refusal_messages(body: &Value) -> Vec<String> {
    let several: Vec<String> = body["_embedded"]["errors"]
        .as_array()
        .map(|a| a.iter().filter_map(|e| e["message"].as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    if !several.is_empty() {
        return several;
    }
    vec![body["message"].as_str().unwrap_or("The server refused the change.").to_string()]
}

/// Hours in an ISO 8601 duration as OpenProject sends them ("PT1H30M", "P1DT2H", "PT0.5H").
pub fn iso_hours(d: &str) -> Option<f64> {
    let d = d.strip_prefix('P')?;
    let (days, time) = match d.split_once('T') {
        Some((a, b)) => (a, b),
        None => (d, ""),
    };
    let mut hours = 0.0;
    let mut num = String::new();
    for (part, units) in [(days, "D"), (time, "HMS")] {
        for c in part.chars() {
            if c.is_ascii_digit() || c == '.' {
                num.push(c);
                continue;
            }
            let n: f64 = num.parse().ok()?;
            num.clear();
            hours += match (units, c) {
                ("D", 'D') => n * 24.0,
                ("HMS", 'H') => n,
                ("HMS", 'M') => n / 60.0,
                ("HMS", 'S') => n / 3600.0,
                _ => return None,
            };
        }
    }
    num.is_empty().then_some(hours)
}

/// A finished time entry for the "Today" list.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Logged {
    pub id: u64,
    pub task: Task,
    pub hours: f64,
    pub comment: String,
}

/// Time on a task: everyone's logged hours and its estimate (a parent's own or derived one).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TaskHours {
    pub task_id: u64,
    pub spent: f64,
    pub estimate: Option<f64>,
}

fn wp_href(id: u64) -> String {
    format!("/api/v3/work_packages/{id}")
}

impl Client {
    pub fn new(base_url: &str, api_key: &str) -> Result<Self> {
        if api_key.trim().is_empty() {
            return Err(ApiError::NotConfigured);
        }
        let base = normalize_base_url(base_url).map_err(|_| ApiError::NotConfigured)?;
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .user_agent(concat!("dwl-timer/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| ApiError::Network(e.to_string()))?;
        Ok(Self { base, api_key: api_key.trim().to_string(), http })
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<Value> {
        let req = req
            .basic_auth("apikey", Some(&self.api_key))
            .header("Accept", "application/hal+json")
            .build()
            .map_err(|e| ApiError::Network(e.to_string()))?;
        let (method, url) = (req.method().to_string(), req.url().to_string());
        let resp = match self.http.execute(req).await {
            Ok(r) => r,
            Err(e) => {
                log_request(&method, &url, "network error");
                return Err(ApiError::Network(e.to_string()));
            }
        };
        let status = resp.status();
        log_request(&method, &url, status.as_str());
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized);
        }
        if matches!(status.as_u16(), 502..=504) {
            return Err(ApiError::Network(format!("server unavailable (HTTP {status})")));
        }
        if status == reqwest::StatusCode::NO_CONTENT {
            return Ok(Value::Null);
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| ApiError::Parse(format!("HTTP {status}: {e}")))?;
        if matches!(status.as_u16(), 403 | 404) {
            let msg = body["message"].as_str().unwrap_or("not allowed").to_string();
            return Err(ApiError::NotAllowed(msg));
        }
        if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
            return Err(ApiError::Refused(refusal_messages(&body)));
        }
        if !status.is_success() {
            let msg = body["message"].as_str().unwrap_or("request failed").to_string();
            return Err(ApiError::Server(format!("{msg} (HTTP {})", status.as_u16())));
        }
        Ok(body)
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v3{path}", self.base)
    }

    pub async fn me(&self) -> Result<User> {
        let v = self.send(self.http.get(self.url("/users/me"))).await?;
        Ok(User {
            id: v["id"].as_u64().ok_or_else(|| ApiError::Parse("user without id".into()))?,
            name: v["name"].as_str().unwrap_or_default().to_string(),
        })
    }

    /// Open work packages: assigned to me when `text` is empty, otherwise every open work
    /// package I can see whose subject or ID matches (so shared tasks such as a project's
    /// general task are found too).
    pub async fn search_tasks(&self, text: &str) -> Result<Vec<Task>> {
        let mut f = vec![json!({"status": {"operator": "o", "values": []}})];
        if text.trim().is_empty() {
            f.push(json!({"assignee": {"operator": "=", "values": ["me"]}}));
        } else {
            f.push(json!({"typeahead": {"operator": "**", "values": [text.trim()]}}));
        }
        let req = self.http.get(self.url("/work_packages")).query(&[
            ("filters", filters(Value::Array(f))),
            ("sortBy", r#"[["updatedAt","desc"]]"#.to_string()),
            ("pageSize", "25".to_string()),
        ]);
        let v = self.send(req).await?;
        v["_embedded"]["elements"]
            .as_array()
            .map(|a| a.iter().map(parse_task).collect())
            .unwrap_or_else(|| Ok(vec![]))
    }

    /// My running timer, if any (also finds one started in the web UI).
    pub async fn running_timer(&self) -> Result<Option<TimeEntry>> {
        let f = json!([
            {"ongoing": {"operator": "=", "values": ["t"]}},
            {"user": {"operator": "=", "values": ["me"]}}
        ]);
        let req = self.http.get(self.url("/time_entries")).query(&[("filters", filters(f))]);
        let v = self.send(req).await?;
        match v["_embedded"]["elements"].as_array().and_then(|a| a.first()) {
            Some(e) => Ok(Some(parse_time_entry(e)?)),
            None => Ok(None),
        }
    }

    pub async fn task(&self, id: u64) -> Result<Task> {
        let v = self.send(self.http.get(self.url(&format!("/work_packages/{id}")))).await?;
        parse_task(&v)
    }

    /// My finished entries on `day`, newest first.
    pub async fn my_entries_on(&self, day: NaiveDate) -> Result<Vec<Logged>> {
        let f = json!([
            {"user": {"operator": "=", "values": ["me"]}},
            {"spent_on": {"operator": "=d", "values": [day.to_string()]}}
        ]);
        let req = self.http.get(self.url("/time_entries")).query(&[
            ("filters", filters(f)),
            ("pageSize", "100".to_string()),
            ("sortBy", r#"[["id","desc"]]"#.to_string()),
        ]);
        let v = self.send(req).await?;
        let mut out = vec![];
        for e in v["_embedded"]["elements"].as_array().into_iter().flatten() {
            if e["ongoing"].as_bool() == Some(true) {
                continue;
            }
            let t = parse_time_entry(e)?;
            let hours = e["hours"].as_str().and_then(iso_hours).unwrap_or(0.0);
            out.push(Logged { id: t.id, task: t.task, hours, comment: t.comment });
        }
        Ok(out)
    }

    pub async fn task_hours(&self, id: u64) -> Result<TaskHours> {
        let v = self.send(self.http.get(self.url(&format!("/work_packages/{id}")))).await?;
        let h = |k: &str| v[k].as_str().and_then(iso_hours);
        Ok(TaskHours {
            task_id: id,
            spent: h("spentTime").unwrap_or(0.0),
            estimate: h("derivedEstimatedTime").filter(|x| *x > 0.0).or(h("estimatedTime")).filter(|x| *x > 0.0),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub async fn time_entry(&self, id: u64) -> Result<TimeEntry> {
        let v = self.send(self.http.get(self.url(&format!("/time_entries/{id}")))).await?;
        parse_time_entry(&v)
    }

    pub async fn start_timer(&self, task_id: u64, spent_on: NaiveDate, comment: &str) -> Result<TimeEntry> {
        let body = json!({
            "ongoing": true,
            "spentOn": spent_on.to_string(),
            "comment": {"raw": comment},
            "_links": {"entity": {"href": wp_href(task_id)}}
        });
        let v = self.send(self.http.post(self.url("/time_entries")).json(&body)).await?;
        parse_time_entry(&v)
    }

    /// Point the running timer at another task; the timer keeps running.
    pub async fn retarget_timer(&self, entry_id: u64, task_id: u64) -> Result<TimeEntry> {
        let body = json!({"_links": {"entity": {"href": wp_href(task_id)}}});
        let v = self
            .send(self.http.patch(self.url(&format!("/time_entries/{entry_id}"))).json(&body))
            .await?;
        parse_time_entry(&v)
    }

    /// Change the comment of the running timer (allowed with "log own time" alone).
    pub async fn update_comment(&self, entry_id: u64, comment: &str) -> Result<TimeEntry> {
        let body = json!({"comment": {"raw": comment}});
        let v = self
            .send(self.http.patch(self.url(&format!("/time_entries/{entry_id}"))).json(&body))
            .await?;
        parse_time_entry(&v)
    }

    pub async fn stop_timer(&self, entry_id: u64, minutes: i64, comment: &str) -> Result<TimeEntry> {
        let body = json!({
            "ongoing": false,
            "hours": iso_minutes(minutes),
            "comment": {"raw": comment}
        });
        let v = self
            .send(self.http.patch(self.url(&format!("/time_entries/{entry_id}"))).json(&body))
            .await?;
        parse_time_entry(&v)
    }

    /// A finished entry, e.g. a meeting during an away period.
    pub async fn log_time(
        &self,
        task_id: u64,
        spent_on: NaiveDate,
        minutes: i64,
        comment: &str,
    ) -> Result<TimeEntry> {
        let body = json!({
            "spentOn": spent_on.to_string(),
            "hours": iso_minutes(minutes),
            "comment": {"raw": comment},
            "_links": {"entity": {"href": wp_href(task_id)}}
        });
        let v = self.send(self.http.post(self.url("/time_entries")).json(&body)).await?;
        parse_time_entry(&v)
    }

    /// A comment on the task itself (its activity), e.g. why it needs more time than estimated.
    pub async fn comment_on_task(&self, task_id: u64, text: &str) -> Result<()> {
        let body = json!({"comment": {"raw": text}});
        self.send(self.http.post(self.url(&format!("/work_packages/{task_id}/activities"))).json(&body))
            .await?;
        Ok(())
    }

    pub async fn delete_time_entry(&self, id: u64) -> Result<()> {
        self.send(self.http.delete(self.url(&format!("/time_entries/{id}")))).await?;
        Ok(())
    }

    /// Raw entry, for tests that check what was stored.
    pub async fn time_entry_raw(&self, id: u64) -> Result<Value> {
        self.send(self.http.get(self.url(&format!("/time_entries/{id}")))).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso() {
        assert_eq!(iso_minutes(0), "PT0M");
        assert_eq!(iso_minutes(25), "PT25M");
        assert_eq!(iso_minutes(60), "PT1H");
        assert_eq!(iso_minutes(67), "PT1H7M");
        assert_eq!(iso_minutes(-5), "PT0M");
    }

    #[test]
    fn iso_durations() {
        assert_eq!(iso_hours("PT1H30M"), Some(1.5));
        assert_eq!(iso_hours("PT0.5H"), Some(0.5));
        assert_eq!(iso_hours("P1DT2H"), Some(26.0));
        assert_eq!(iso_hours("PT45M"), Some(0.75));
        assert_eq!(iso_hours("PT0S"), Some(0.0));
        assert_eq!(iso_hours("1H"), None);
        assert_eq!(iso_hours("PT1X"), None);
    }

    #[test]
    fn urls() {
        assert_eq!(normalize_base_url("op.example.com/").unwrap(), "https://op.example.com");
        assert_eq!(normalize_base_url(" http://localhost:8080 ").unwrap(), "http://localhost:8080");
        assert!(normalize_base_url("").is_err());
        assert!(normalize_base_url("ftp://x").is_err());
        // Plain http only where it doesn't cross the internet.
        assert!(normalize_base_url("http://op.example.com").unwrap_err().contains("https"));
        for ok in ["http://127.0.0.1:8080", "http://192.168.1.20", "http://10.0.0.5", "http://[::1]:8080",
                   "http://openproject.lan", "http://op.local", "http://op.localhost"] {
            assert_eq!(normalize_base_url(ok).unwrap(), ok);
        }
    }

    #[test]
    fn refusals_single_and_multiple() {
        let one: Value = serde_json::from_str(
            r#"{"errorIdentifier":"urn:openproject-org:api:v3:errors:PropertyConstraintViolation","message":"One time entry can be at most 7 hours."}"#,
        )
        .unwrap();
        assert_eq!(refusal_messages(&one), vec!["One time entry can be at most 7 hours."]);
        let many: Value = serde_json::from_str(
            r#"{"errorIdentifier":"urn:openproject-org:api:v3:errors:MultipleErrors","message":"Multiple field constraints have been violated.",
            "_embedded":{"errors":[{"message":"A"},{"message":"B"}]}}"#,
        )
        .unwrap();
        assert_eq!(refusal_messages(&many), vec!["A", "B"]);
    }

    #[test]
    fn parses_time_entry_from_op17() {
        let v: Value = serde_json::from_str(
            r#"{"_type":"TimeEntry","id":284,"ongoing":true,"comment":{"raw":""},
            "spentOn":"2026-11-05","hours":null,"createdAt":"2026-11-05T05:03:35.452Z",
            "_links":{"project":{"href":"/api/v3/projects/106","title":"Timer app test"},
            "entity":{"href":"/api/v3/work_packages/1666","title":"Write API client"}}}"#,
        )
        .unwrap();
        let e = parse_time_entry(&v).unwrap();
        assert_eq!(e.id, 284);
        assert!(e.ongoing);
        assert_eq!(e.task, Task { id: 1666, subject: "Write API client".into(), project: "Timer app test".into() });
        assert_eq!(e.started_at.to_rfc3339(), "2026-11-05T05:03:35.452+00:00");
    }
}
