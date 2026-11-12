# Contributing

Issues and pull requests are welcome. For anything bigger than a fix, open an issue first so we
can agree on the approach.

## Build

See [Build](README.md#build) in the README. The quickest way to get an Ubuntu `.deb` from any
machine with Docker:

```bash
scripts/build-deb-docker.sh     # -> dist/dwl-timer_<version>_amd64.deb
```

It builds on Ubuntu 22.04 for amd64, so the package installs on 22.04 and 24.04. On an Apple
Silicon Mac, run Docker through Colima with Rosetta (`colima start --vz-rosetta`); QEMU emulation
crashes while linking. The first build takes a while; Rust and npm caches are kept in Docker
volumes.

## Unit tests

```bash
cd src-tauri
cargo test
```

They cover the hours maths, idle bookkeeping, reminders, the offline queue and API parsing. No
server needed. Please run them before sending a pull request.

## Live tests (against a test OpenProject)

`src-tauri/tests/live.rs` drives the same code the app uses against a real server, with fake idle
events. They are `#[ignore]`d and only run when asked:

```bash
OP_URL=http://localhost:8080 OP_API_KEY=... cargo test --test live -- --ignored --test-threads=1
```

**Never point them at a production server**: they start, stop and log time. They need a test
user with at least 3 open work packages assigned. The test user's role may be limited to "Log
own time" (the tests check that finished entries are then read-only); in that case the tests
can't delete what they log, so remove the test user's entries afterwards.

| Variable | Used by | What |
|---|---|---|
| `OP_URL` | all | Server URL |
| `OP_API_KEY` | all | API key of the test user |
| `OP_GENERAL_TASK` | `rules_plugin_refusals` | ID of a task titled "General work" |
| `OP_TINY_TASK` | `rules_plugin_refusals` | ID of a task with an estimate under a minute (e.g. 0.01 h) and no comment from the test user; use a new one each run |

`rules_plugin_refusals` only makes sense on a server with a rules plugin that refuses entries
with HTTP 422 (see "Optional server conventions" in the README).

## UI preview without Rust

```bash
python3 -m http.server 8765
```

Then open <http://127.0.0.1:8765/dev/preview.html>. It loads `ui/` with a mock backend
(`dev/mock-tauri.js`). In the browser console, `mockAway(37)` opens the away question,
`mockOffline()` shows the offline queue with a refused change, and
`mockRefuse(["..."], true)` makes the next stop be refused.

## Debug switches

Read only when the app runs with `DWL_TIMER_DEBUG=1`: `DWL_TIMER_API_KEY` (skip the keyring) and
the `debug_fake_away` command. `DWL_TIMER_IDLE_SECONDS` sets the idle threshold in seconds.

## Style

- Keep pure logic (`tracker.rs`, `remind.rs`, `queue.rs`) free of I/O, with unit tests.
- User-facing text: short, plain sentences.
- Never log notes, hours or API keys.
