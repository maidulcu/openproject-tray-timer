// Fake Tauri backend for dev/preview.html. Mimics the Rust commands with in-memory state.
(() => {
  const listeners = {};
  const emit = (name, payload) => (listeners[name] || []).forEach((f) => f({ payload }));
  const tasks = [
    { id: 1666, subject: "Write API client", project: "Timer app test" },
    { id: 1667, subject: "Design tray menu", project: "Timer app test" },
    { id: 1668, subject: "Fix idle dialog", project: "Timer app test" },
    { id: 1700, subject: "General work", project: "Timer app test" },
  ];
  let s = { configured: true, max_idle_minutes: 10,
    settings: { server_url: "http://localhost:8080", idle_minutes: 10, work_start: "09:00", work_end: "18:00", reminders: true, start_at_login: true },
    user: { id: 11, name: "Timer Test" }, timer: null, idle_stop: null, stop_problem: null,
    offline: false, waiting: [], refusal: null,
    break_available: true, version: "0.4.2",
    today: [
      { id: 501, task: { id: 1667, subject: "Design tray menu", project: "Timer app test" }, hours: 1.5, comment: "Tray layout" },
      { id: 502, task: { id: 1791, subject: "Breaks", project: "Internal" }, hours: 0.25, comment: "Tea" },
    ],
    today_secs: 5400, task_hours: null,
    recent: [{ task: tasks[3], comment: "Code review and planning" }],
    idle_status: "off (no timer running)", error: null };
  const log = (window.mockLog = []);
  const snap = () => JSON.parse(JSON.stringify(s));
  const comment = (c) => { if ((c || "").trim().length < 3) throw "A note saying what the meeting or call was is required."; };
  let refuse = null; // set by mockRefuse(): the next stop is refused with these messages
  const remember = (task, c) => { s.recent = [{ task, comment: c }, ...s.recent.filter((r) => r.task.id !== task.id)].slice(0, 8); };
  const start = (id, c) => {
    const task = tasks.find((t) => t.id === id);
    s.timer = { entry_id: 900 + log.length, task, comment: c.trim(), started_at: new Date(Date.now() - 42 * 60000).toISOString(),
      worked_secs: 42 * 60, idle_now: false };
    remember(task, c.trim());
    s.task_hours = { task_id: id, spent: 5.5, estimate: 8 };
    s.today_secs = 5400 + 42 * 60;
    s.idle_status = "watching (timer stops after 10 min without input)";
  };
  const cmds = {
    get_state: () => snap(),
    take_view: () => "timer",
    take_pick: () => null,
    hide_window: () => null,
    search_tasks: ({ text }) => tasks.filter((t) => text ? `${t.id} ${t.subject}`.toLowerCase().includes(text.toLowerCase()) : t.id < 1700),
    start_timer: ({ taskId, comment: c }) => { if (s.timer) throw "A timer is already running"; start(taskId, c); },
    retarget_timer: ({ taskId }) => { s.timer.task = tasks.find((t) => t.id === taskId); },
    set_comment: ({ comment: c }) => { s.timer.comment = c.trim(); remember(s.timer.task, c.trim()); },
    stop_timer: ({ input }) => {
      if (input?.comment != null) s.timer.comment = input.comment.trim();
      if (refuse && !(input?.reason && refuse.reason)) {
        s.stop_problem = { messages: refuse.messages, needs_reason: refuse.reason, until: null };
        return false;
      }
      refuse = null; s.stop_problem = null; s.timer = null; return true;
    },
    stop_and_start: ({ taskId, comment: c }) => { s.timer = null; start(taskId, c); return true; },
    resolve_idle_stop: ({ meetingTaskId, meetingComment, restartComment }) => {
      if (meetingTaskId) comment(meetingComment);
      const st = s.idle_stop;
      s.idle_stop = null;
      if (restartComment != null) start(st.task.id, restartComment);
    },
    save_settings: ({ serverUrl, apiKey, new: n }) => {
      if (!apiKey && !s.configured) throw "Enter your API key";
      if (n.idle_minutes < 1 || n.idle_minutes > 10) throw "Stop after: pick 1 to 10 minutes.";
      s.settings = { ...n, server_url: serverUrl };
      return snap();
    },
    refresh: () => null,
    open_in_openproject: ({ path }) => { window.mockOpened = path; },
    retry_refused: ({ note }) => { if (/jonas/i.test(note || "")) throw "The note is shown to the client: remove Jonas."; s.refusal = null; s.waiting = []; s.offline = false; },
    discard_refused: () => { s.refusal = null; },
  };
  // mockAway(37): the timer stopped 37 min ago for no input, and the user is back now.
  window.mockAway = (min) => {
    if (!s.timer) start(1666, "Write the request builder");
    const to = new Date(), from = new Date(to - min * 60000);
    s.idle_stop = { task: s.timer.task, comment: s.timer.comment, from: from.toISOString(), to: to.toISOString(), minutes: min };
    s.timer = null;
    emit("state", snap()); emit("navigate", "away");
  };
  // mockRefuse(["#1666 is over its estimate ..."], true): the next stop is refused.
  window.mockRefuse = (messages, reason = false) => { refuse = { messages, reason }; };
  // mockOffline(): offline with two waiting changes, the first one refused on resend.
  window.mockOffline = () => {
    s.offline = true;
    s.waiting = ["Stop the timer (1:20)", "Start timer on #1667 Design tray menu"];
    s.refusal = { action: "Log 0:45 on #1700 General work", note: "Weekly sync with Jonas", minutes: 45,
      messages: ["The note is shown to the client: remove Jonas. Describe the work only, no names or internal tools."], needs_reason: false };
    emit("state", snap());
  };
  window.__TAURI__ = {
    core: { invoke: async (cmd, args = {}) => {
      log.push([cmd, args]);
      if (!cmds[cmd]) throw `unknown command ${cmd}`;
      const r = cmds[cmd](args);
      if (!["get_state", "search_tasks", "take_view", "take_pick"].includes(cmd)) setTimeout(() => emit("state", snap()));
      return r;
    } },
    event: { listen: async (name, f) => { (listeners[name] ||= []).push(f); } },
  };
})();
