// DWL timer window. Talks to the Rust side only through invoke() and events.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (sel) => document.querySelector(sel);
const $$ = (sel) => document.querySelectorAll(sel);

let state = null;
let pickedTask = null;     // task chosen in the Timer view
let workTask = null;       // task the away time was work on
let shownStop = null;      // the idle stop the away form was filled for
let view = "timer";

const hm = (secs) => {
  secs = Math.max(0, Math.floor(secs));
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  return `${h}:${String(m).padStart(2, "0")}`;
};
// Running clock with seconds, so a fresh timer visibly counts from the first second.
const hms = (secs) => {
  secs = Math.max(0, Math.floor(secs));
  return hm(secs) + ":" + String(secs % 60).padStart(2, "0");
};
const clock = (iso) => new Date(iso).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
const taskLabel = (t) => `#${t.id} ${t.subject}`;
const recentComment = (id) => state?.recent.find((r) => r.task.id === id)?.comment || "";
// Same list as tracker::NOTE_REQUIRED_TASKS: these tasks' titles say nothing to the client.
const noteRequired = (task) => ["general work"].includes((task?.subject || "").trim().toLowerCase());
const noteLabel = (task) => noteRequired(task)
  ? "Note (required on this task; the client sees it)"
  : "Note (optional; the client sees it, or the task title if empty)";

function workedSecs() {
  // The snapshot's worked_secs is from when it was taken; keep counting locally.
  if (!state?.timer) return 0;
  return state.timer.worked_secs + (Date.now() - state.receivedAt) / 1000;
}

function showError(msg) {
  const el = $("#error");
  el.textContent = msg || "";
  el.hidden = !msg;
  // The meeting's task is over its estimate: ask for the reason with the meeting entry.
  if (msg && /estimate/i.test(msg) && !$("#work-form").hidden) $("#work-reason-box").hidden = false;
}

function setBusy(busy) {
  $$("button").forEach((b) => { if (!b.dataset.view) b.disabled = busy; });
}

async function call(cmd, args) {
  setBusy(true);
  try {
    const res = await invoke(cmd, args);
    showError(null);
    return res;
  } catch (e) {
    showError(String(e));
    throw e;
  } finally {
    setBusy(false);
  }
}

function go(v) {
  view = v;
  $$(".view").forEach((s) => (s.hidden = s.id !== `view-${v}`));
  $$("nav button").forEach((b) => b.classList.toggle("active", b.dataset.view === v));
  render();
  if (v === "timer") {
    (pickedTask ? $("#pick-comment") : $("#search")).focus();
    loadAssigned();
  }
}

function pick(task) {
  pickedTask = task;
  $("#pick-comment").value = recentComment(task.id);
  render();
  $("#pick-comment").focus();
  $("#pick-comment").select();
}

function render() {
  if (!state) return;
  const t = state.timer;

  // Header
  const st = $("#status");
  if (!state.configured) {
    st.innerHTML = "Not set up <span class='sub'>Add your server and API key in Settings.</span>";
  } else if (t) {
    st.innerHTML = "";
    const live = document.createElement("span");
    live.className = "live";
    live.textContent = state.offline ? "Running (offline)" : "Running";
    const clockEl = document.createElement("span");
    clockEl.className = "clock";
    clockEl.textContent = hms(workedSecs());
    st.append(live, clockEl, " ", t.task.subject);
    const sub = document.createElement("span");
    sub.className = "sub";
    sub.textContent = `#${t.task.id} · ${t.task.project} · started ${clock(t.started_at)}`;
    st.append(sub);
  } else {
    st.innerHTML = `No timer running <span class='sub'>${state.user ? "Signed in as " + escapeHtml(state.user.name) : ""}</span>`;
  }
  if (state.error) showError(state.error);

  // Offline / waiting changes
  const waiting = state.waiting || [];
  $("#offline").hidden = !state.offline && !waiting.length;
  const n = waiting.length;
  $("#offline-text").textContent = state.offline
    ? "Offline: the timer keeps working" + (n ? ", " + n + " change(s) will be sent when OpenProject is back." : ".")
    : n + " change(s) waiting to be sent.";
  $("#waiting-box").hidden = !n;
  $("#waiting-list").innerHTML = "";
  for (const w of waiting) {
    const li = document.createElement("li");
    li.textContent = w;
    $("#waiting-list").append(li);
  }

  // A waiting change the server refused
  const q = state.refusal;
  $("#queue-refused").hidden = !q;
  if (q) {
    $("#qr-action").textContent = q.action;
    $("#qr-list").innerHTML = "";
    for (const m of q.messages) {
      const li = document.createElement("li");
      li.textContent = m;
      $("#qr-list").append(li);
    }
    $("#qr-note-box").hidden = q.note == null;
    const key = q.action + q.messages.join();
    if ($("#queue-refused").dataset.key !== key) {
      $("#queue-refused").dataset.key = key;
      $("#qr-note").value = q.note || "";
      $("#qr-reason").value = "";
      $("#qr-minutes").value = q.minutes || "";
    }
    $("#qr-reason-box").hidden = !q.needs_reason;
    $("#qr-minutes-box").hidden = q.minutes == null;
  }

  // Running timer
  $("#running").hidden = !t;
  if (t) {
    const rc = $("#run-comment");
    if (document.activeElement !== rc && rc.dataset.entry !== String(t.entry_id) + t.comment) {
      rc.value = t.comment;
      rc.dataset.entry = String(t.entry_id) + t.comment;
    }
    $("#btn-save-comment").hidden = rc.value.trim() === t.comment.trim();
    $("#run-comment-label").textContent = noteLabel(t.task);
  }
  if (t) {
    const h = state.task_hours;
    const fmt = (x) => hm(x * 3600);
    let txt = "";
    if (h && h.estimate) {
      txt = fmt(h.spent) + " of " + fmt(h.estimate) + " h on this task (logged by everyone)";
      if (h.spent > h.estimate) txt += ": over the estimate, stopping will ask why";
    } else if (h) {
      txt = fmt(h.spent) + " h logged on this task (no estimate)";
    }
    $("#task-hours").textContent = txt;
    $("#task-hours").classList.toggle("warn", !!(h && h.estimate && h.spent > h.estimate));
    $("#btn-open-task").hidden = !t.task.id;
  }

  // Today
  const today = state.today || [];
  $("#today-box").hidden = !state.configured;
  $("#today-sum").textContent = "Today: " + hm(state.today_secs || 0) + " logged" +
    (t ? " (with the running timer)" : "");
  $("#today-list").innerHTML = "";
  for (const e of today) {
    const li = document.createElement("li");
    const isBreak = e.task.subject.trim().toLowerCase() === "breaks";
    li.textContent = hm(e.hours * 3600) + "  #" + e.task.id + " " + e.task.subject + (isBreak ? " (break, not billed)" : "");
    if (e.comment) {
      const n = document.createElement("span");
      n.className = "proj";
      n.textContent = e.comment;
      li.append(n);
    }
    $("#today-list").append(li);
  }
  if (!today.length) $("#today-list").innerHTML = "<li class='hint'>Nothing logged yet today.</li>";

  const p = state.stop_problem;
  $("#refused").hidden = !(t && p);
  if (t && p) {
    const ul = $("#refused-list");
    ul.innerHTML = "";
    for (const m of p.messages) {
      const li = document.createElement("li");
      li.textContent = m;
      ul.append(li);
    }
    $("#reason-box").hidden = !p.needs_reason;
    if (p.until && !$("#stop-until").value && document.activeElement !== $("#stop-until")) {
      $("#stop-until").value = new Date(p.until).toTimeString().slice(0, 5);
    }
    $("#btn-stop").textContent = "Try stopping again";
  } else {
    $("#btn-stop").textContent = "Stop timer";
  }

  // Start / switch
  $("#picked").hidden = !pickedTask;
  if (pickedTask) {
    $("#picked .picked-title").textContent = taskLabel(pickedTask);
    $("#pick-comment-label").textContent = noteLabel(pickedTask).replace("Note", "What will you work on?");
    $("#btn-go").textContent = t ? "Stop the current timer and start this task" : "Start timer";
    $("#pick-running").hidden = !t;
  }

  // Away
  const a = state.idle_stop;
  $("#nav-away").hidden = !(a && a.to);
  $("#away-none").hidden = !!a;
  $("#away-waiting").hidden = !(a && !a.to);
  $("#away-form").hidden = !(a && a.to);
  $("#btn-break-show").hidden = !state.break_available;
  if (a && !a.to) {
    $("#away-waiting").textContent = `Your timer on ${a.task.subject} stopped at ${clock(a.from)} because there was no input. Welcome back when you are.`;
  }
  if (a && a.to) {
    $$(".stop-task").forEach((el) => (el.textContent = a.task.subject));
    $("#away-from").textContent = clock(a.from);
    $("#away-to").textContent = clock(a.to);
    $("#away-min").textContent = a.minutes;
    $$(".away-min").forEach((el) => (el.textContent = a.minutes));
    $("#away-limit").textContent = state.settings.idle_minutes;
    const key = a.from + a.task.id;
    if (shownStop !== key) {
      // A new away period: reset the form.
      shownStop = key;
      workTask = a.task;
      $("#work-form").hidden = true;
      $("#break-form").hidden = true;
      $("#break-note").value = "";
      $("#work-search-box").hidden = true;
      $("#work-comment").value = "";
      $("#work-reason").value = "";
      $("#work-reason-box").hidden = true;
      $("#restart").checked = !t;
      $("#restart-comment").value = a.comment;
    }
    $("#work-task").textContent = taskLabel(workTask);
    $("#restart").disabled = !!t;
    $("#restart-box").hidden = !$("#restart").checked;
  }

  // Settings
  const s = state.settings;
  const keep = (sel, val) => { if (document.activeElement !== $(sel)) $(sel).value = val; };
  keep("#set-url", s.server_url || "");
  keep("#set-idle", s.idle_minutes);
  keep("#set-start", s.work_start);
  keep("#set-end", s.work_end);
  $("#set-idle").max = state.max_idle_minutes;
  if (!settingsDirty) {
    $("#set-remind").checked = s.reminders;
    $("#set-login").checked = s.start_at_login;
  }
  $("#set-key").placeholder = state.configured ? "Saved in the keyring (leave empty to keep)" : "";
  $("#idle-status").textContent = state.idle_status;
  $("#app-version").textContent = state.version || "";
  $("#btn-update").hidden = !state.update_version;
  $("#btn-update").textContent = "Update to " + (state.update_version || "") + ": install and restart";
}
let settingsDirty = false;
$("#set-remind").onchange = $("#set-login").onchange = () => (settingsDirty = true);

function escapeHtml(s) {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

function setState(s) {
  state = { ...s, receivedAt: Date.now() };
  render();
  if (view === "timer" && !$("#search").value && document.activeElement !== $("#search")) showDefaultList();
}

// ---- task lists ----
function fillList(list, groups, onPick) {
  list.innerHTML = "";
  for (const [title, tasks] of groups) {
    if (!tasks.length) continue;
    if (title) {
      const h = document.createElement("li");
      h.className = "group";
      h.textContent = title;
      list.append(h);
    }
    for (const task of tasks) {
      const li = document.createElement("li");
      li.textContent = taskLabel(task);
      const proj = document.createElement("span");
      proj.className = "proj";
      proj.textContent = task.project;
      li.append(proj);
      li.onclick = () => {
        list.querySelectorAll("li").forEach((x) => x.classList.remove("sel"));
        li.classList.add("sel");
        onPick(task);
      };
      list.append(li);
    }
  }
  if (!list.children.length) list.innerHTML = "<li class='hint'>No open tasks match.</li>";
}

let assigned = [];
async function loadAssigned() {
  if (!state?.configured) return;
  try { assigned = await invoke("search_tasks", { text: "" }); } catch (_) { return; }
  if (!$("#search").value) showDefaultList();
}
function showDefaultList() {
  // Empty search: recent tasks first, then open tasks assigned to me.
  const running = state?.timer?.task.id;
  const recent = (state?.recent || []).map((r) => r.task).filter((t) => t.id !== running);
  const ids = new Set(recent.map((t) => t.id));
  fillList($("#results"), [["Recent", recent], ["Assigned to me", assigned.filter((t) => !ids.has(t.id) && t.id !== running)]], pick);
}

function wireSearch(input, list, onPick, onEmpty) {
  let timer = null;
  let seq = 0;
  const run = async () => {
    const mine = ++seq;
    try {
      const tasks = await invoke("search_tasks", { text: input.value });
      if (mine !== seq) return;
      if (!input.value.trim() && onEmpty) onEmpty(tasks);
      else fillList(list, [["", tasks]], onPick);
    } catch (e) {
      showError(String(e));
    }
  };
  input.addEventListener("input", () => { clearTimeout(timer); timer = setTimeout(run, 250); });
  input.addEventListener("focus", () => { if (!list.children.length || !input.value) run(); });
}

wireSearch($("#search"), $("#results"), pick, (tasks) => { assigned = tasks; showDefaultList(); });
wireSearch($("#work-search"), $("#work-results"), (task) => {
  workTask = task;
  $("#work-search-box").hidden = true;
  render();
  $("#work-comment").focus();
});

// Enter submits (Shift+Enter for a new line).
$$("textarea[data-submit]").forEach((ta) =>
  ta.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      $("#" + ta.dataset.submit).click();
    }
  }));

// ---- actions ----
async function act(cmd, args, after) {
  try {
    await call(cmd, args);
    setState(await invoke("get_state"));
    after?.();
  } catch (_) { /* shown */ }
}

$("#run-comment").addEventListener("input", render);
$("#btn-save-comment").onclick = () => act("set_comment", { comment: $("#run-comment").value });
// The end time typed in "End at", as an ISO time today (or yesterday if later than now).
function untilIso() {
  const v = $("#stop-until").value;
  if (!v) return null;
  const [h, m] = v.split(":").map(Number);
  const d = new Date();
  d.setHours(h, m, 0, 0);
  if (d > new Date()) d.setDate(d.getDate() - 1);
  return d.toISOString();
}
function stopInput() {
  return { comment: $("#run-comment").value, reason: $("#stop-reason").value || null, until: untilIso() };
}
function clearStopForm() {
  $("#stop-reason").value = "";
  $("#stop-until").value = "";
}
async function stopAct(cmd, args, after) {
  try {
    const stopped = await call(cmd, args);
    setState(await invoke("get_state"));
    if (stopped) { clearStopForm(); after?.(); }
  } catch (_) { /* shown */ }
}
$("#btn-stop").onclick = () => stopAct("stop_timer", { input: stopInput() }, () => invoke("hide_window"));

function donePicking() {
  pickedTask = null;
  $("#pick-comment").value = "";
  $("#search").value = "";
  invoke("hide_window");
}
$("#btn-go").onclick = () => {
  const args = { taskId: pickedTask.id, comment: $("#pick-comment").value };
  if (state?.timer) stopAct("stop_and_start", { ...args, input: stopInput() }, donePicking);
  else act("start_timer", args, donePicking);
};
$("#btn-retarget").onclick = () => act("retarget_timer", { taskId: pickedTask.id }, donePicking);

function restartComment() {
  return $("#restart").checked && !$("#restart").disabled ? $("#restart-comment").value : null;
}
function afterAway() {
  if (!state.idle_stop) invoke("hide_window");
}
$("#restart").onchange = render;
$("#btn-not-work").onclick = () =>
  act("resolve_idle_stop", { meetingTaskId: null, meetingComment: null, restartComment: restartComment(), reason: null }, afterAway);
$("#btn-work-show").onclick = () => {
  $("#break-form").hidden = true;
  $("#work-form").hidden = false;
  $("#work-comment").focus();
};
$("#btn-break-show").onclick = () => {
  $("#work-form").hidden = true;
  $("#break-form").hidden = false;
  $("#break-note").focus();
};
$("#btn-break").onclick = () =>
  act("resolve_idle_break", { note: $("#break-note").value, restartComment: restartComment() }, afterAway);
$("#break-note").addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); $("#btn-break").click(); } });
$("#btn-work-change").onclick = () => { $("#work-search-box").hidden = false; $("#work-search").focus(); };
$("#btn-work").onclick = () =>
  act("resolve_idle_stop", {
    meetingTaskId: workTask.id,
    meetingComment: $("#work-comment").value,
    restartComment: restartComment(),
    reason: $("#work-reason").value || null,
  }, afterAway);

$("#btn-save").onclick = async () => {
  $("#set-msg").textContent = "Checking…";
  try {
    const s = await call("save_settings", {
      serverUrl: $("#set-url").value,
      apiKey: $("#set-key").value,
      new: {
        idle_minutes: Number($("#set-idle").value),
        work_start: $("#set-start").value,
        work_end: $("#set-end").value,
        reminders: $("#set-remind").checked,
        start_at_login: $("#set-login").checked,
      },
    });
    $("#set-key").value = "";
    settingsDirty = false;
    setState(s);
    $("#set-msg").textContent = `Connected as ${s.user?.name}.`;
  } catch (e) {
    $("#set-msg").textContent = "";
  }
};

$("#btn-qr-retry").onclick = () => {
  const q = state.refusal;
  const minutes = Number($("#qr-minutes").value) || null;
  act("retry_refused", {
    note: q.note == null ? null : $("#qr-note").value,
    reason: $("#qr-reason").value || null,
    minutes: q.minutes != null && minutes && minutes < q.minutes ? minutes : null,
  });
};
$("#btn-qr-discard").onclick = () => {
  if (confirm("Discard this change? It won't be sent to OpenProject, and time in it is not logged.")) act("discard_refused", {});
};

$("#btn-update").onclick = () => {
  $("#set-msg").textContent = "Installing… Ubuntu may ask for your password.";
  act("install_update", {});
};

$("#btn-open-task").onclick = () => {
  if (state?.timer) invoke("open_in_openproject", { path: "/work_packages/" + state.timer.task.id }).catch((e) => showError(String(e)));
};
$("#btn-key-page").onclick = () =>
  invoke("open_in_openproject", { path: "/my/access_tokens" }).catch((e) => showError(String(e)));
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && !document.querySelector("dialog[open]")) invoke("hide_window");
});

$$("nav button").forEach((b) => (b.onclick = () => go(b.dataset.view)));

// ---- events from Rust ----
listen("state", (e) => setState(e.payload));
listen("navigate", (e) => go(e.payload));
listen("pick", (e) => { go("timer"); pick(e.payload); });
setInterval(render, 15000);
// Tick the clock every second without redrawing the whole window.
setInterval(() => {
  const c = document.querySelector("#status .clock");
  if (c && state?.timer) c.textContent = hms(workedSecs());
}, 1000);

(async () => {
  setState(await invoke("get_state"));
  go(await invoke("take_view"));
  const p = await invoke("take_pick");
  if (p) pick(p);
})();
