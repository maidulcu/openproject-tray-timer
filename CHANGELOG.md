# Changelog

Versions follow [semantic versioning](https://semver.org/). Dates are release dates.

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
