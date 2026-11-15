# Changelog

Versions follow [semantic versioning](https://semver.org/). Dates are release dates.

## 0.5.2 (2026-11-15)

- **Updates are found reliably.** The app checks a minute after it starts and then every hour by
  the clock, so also right after the computer wakes from sleep. Before, it counted 6 hours of
  30-second ticks, which don't advance while a laptop sleeps, so an app that was only ever
  suspended could go days without checking. While an update waits, the "Version x is ready"
  notification comes back every 3 hours.

## 0.5.1 (2026-11-15)

- **Meeting / call mode**: *Don't stop while I'm away (2 h)* on the running timer and in the tray
  menu. While it's on, no input doesn't stop the timer, and that time is left out of the activity
  figure. It ends by itself after 2 hours (with a notification); if there has been no input for
  the idle limit by then, the timer stops as usual, at the end of meeting mode at the earliest. The
  minutes go with the stop as the `X-Timer-Meeting` header. Not offered on breaks.
- **A stop refused for length** (a server message such as "one entry can be at most 7 hours" or
  "the limit is 10 hours a day") offers *Stop at HH:MM (the most allowed)* in one click.

## 0.5.0 (2026-11-15)

- **Activity**: every 30 seconds while a timer runs, the app checks whether there was keyboard or
  mouse input in those 30 seconds (only the time since the last input, nothing else). The share of
  the timer's time with input shows on the timer ("Activity: 87%") and is sent with the stop as the
  `X-Timer-Activity` header, for servers that store it. Not measured: breaks, sleep gaps, or the
  first 2 minutes. "What this app records" in Settings says so.
- **Resume**: with no timer running, the window opens on "Continue where you left off" with the
  last task and its note: one click starts it ("Change the note first" to edit). The tray menu has
  "▶ Resume #id task" at the top. A task that needs a note opens the start card instead.

## 0.4.7 (2026-11-15)

- "Check for updates" in Settings and the tray menu, with the answer in words; a failed check or
  install is no longer silent, and a failed install offers the update again.
- A "Version x is ready" banner with an install button on every screen, not only in Settings.
- A timer on the "Breaks" task is never stopped for being away or asleep (being away is what a
  break is). The evening "still running" reminder catches a forgotten one.
- Design: the header (with the running clock) stays in view when scrolling; the task list uses the
  window's height; the start card opens right under the search box; the away question's answers
  have a title and a detail line; "Open in OpenProject" stays on one line.
- Simpler note labels.
- Default work hours end at 17:00.
- Live test: the name-in-note refusal uses the test user's own name, so it works on any server.

## 0.4.6 (2026-11-12)

- Changing the computer's clock (for example fixing it after the clock warning) no longer looks
  like time away: on a gap the app re-measures the clock difference with the server first.
- The window's live counter uses a monotonic clock, so it doesn't jump when the clock changes.
- Start and away times are shown, and a typed "End at" time is read, on the computer's own clock.

## 0.4.5 (2026-11-12)

- All times are measured on the server's clock (from the HTTP `Date` header), so a computer clock
  that is off no longer shows or logs the wrong time.
- Settings warns when the computer's clock is off by a minute or more.
- The tray text refreshes every 5 seconds, in step with the window.
- Plain `http://` server URLs are refused unless the server is on localhost or a private network,
  so the API key is never sent unencrypted over the internet. An existing setup with such a URL
  shows "Not set up" until the URL is changed to `https://` in Settings.

## 0.4.4 (2026-11-12)

- The running time shows seconds and a red RUNNING badge, so a fresh timer visibly runs.
- The tray title starts with "●" while a timer runs.

## 0.4.3 (2026-11-11)

- A timer stopped in the web UI or on another computer no longer causes repeated 403 errors: the
  app says so and changes nothing.
- A request log (`requests.log`: time, method, address, status code only) for troubleshooting.

## 0.4.2 (2026-11-11)

- Only one copy runs; opening it again shows the running one.
- Today list with the day's total; the running task's hours against its estimate.
- Settings has a button that opens OpenProject's API key page; Esc closes the window.

## 0.4.1 (2026-11-11)

- "Break" in the away question, logged on a "Breaks" task when the server has one.
- Clearer away question.

## 0.4.0 (2026-11-10)

- Built-in updater and a release script.
- Offline queue: changes wait with their real times and are sent in order.

## 0.3.0 (2026-11-10)

- Server refusals (HTTP 422) keep the timer running and show the reasons, with a field for a
  reason when a task is over its estimate (posted as a task comment).

## 0.2.0 (2026-11-07)

- The timer stops by itself after a time without input, and asks on return what that time was.
- Note at start, recent tasks, reminders during work hours.

## 0.1.0 (2026-11-05)

- First version: start and stop OpenProject's own timer from the tray.
