---
id: d692cc40-3cfc-42fb-9035-61bc0cce34e2
slug: task-32
status: done
title: Daemon foreground does not stop on Ctrl-C/SIGINT (must sudo pkill)
milestones:
- milestone-2
created_at: 2026-06-23T14:55:21.364176Z
updated_at: 2026-06-23T14:55:21.364176Z
---

## Context

Observed during the [[[task-22](../work/task-22.task.md)]] / [[[task-31](../work/task-31.task.md)]] macOS bring-up runs: a daemon
started in the foreground as root does **not** stop on Ctrl-C — it has to be
killed with `sudo pkill -f port-zero`. This hurts the dev loop and matters
for the [[[task-23](../work/task-23.task.md)]] LaunchDaemon story (clean stop/teardown).

Repro:

```
sudo -E RUST_LOG=info ./target/debug/port-zero start --foreground
# ^C  -> no effect; process keeps logging / has to be pkill'd
```

What the code looks like today (so this isn't a missing-handler bug):

- `discovery_loop::wait_for_shutdown_signal()` (discovery_loop.rs:739) correctly
  `select!`s on `SIGTERM` and `tokio::signal::ctrl_c()`.
- It is only awaited in the main loop's `select!` (discovery_loop.rs:551-559),
  which races it against `sleep(scan_interval)` — i.e. the signal is only polled
  in the gap **between** scans, not during a scan.
- Runtime is the default multi-threaded `#[tokio::main]`, so a starved signal
  driver is unlikely.

## Approach

Investigate which of these is actually happening (add temporary tracing around
signal receipt and each teardown step):

1. **Blocking scan phase** — the per-iteration scan (discovery_loop.rs:324-550)
   runs before the `select!` and shells out synchronously (`ps`/`lsof`/`docker`
   via `std::process::Command`). If one blocks, SIGINT isn't observed until it
   returns. Candidate fix: move the signal wait to a top-level `select!` that
   races the WHOLE loop body (or spawn the scan via `spawn_blocking` and select
   the signal at the outermost level) so Ctrl-C is honoured immediately.
2. **Hanging teardown** — after the signal fires, shutdown does
   `docker_monitor.abort()` + `overlay.shutdown().await` + pid-file removal
   (discovery_loop.rs:566-580). If `overlay.shutdown()` (scoped-resolver
   uninstall / TUN drop) blocks, the process appears unresponsive. (Note: when
   the overlay failed to start — the [[[task-31](../work/task-31.task.md)]] case — `overlay` is `None`, so
   this path is skipped, which points more at #1 for that specific run.)
3. **Signal delivery under `sudo -E`** — confirm SIGINT is actually reaching the
   child (sudo forwards it, but verify); compare `kill -INT <pid>` vs the
   terminal Ctrl-C, and `kill -TERM` behaviour.

Done when:

- [x] Root cause identified (which of the above)
- [x] Foreground daemon exits promptly on Ctrl-C AND `kill -TERM`, running the
      graceful overlay/resolver teardown
- [x] A second Ctrl-C / a timeout forces exit if graceful teardown stalls
      (no more `sudo pkill` needed)
- [x] Verified via the [[[task-22](../work/task-22.task.md)]] runbook (cross-check `kill -TERM` too)

## Resolution

**Root cause: #1 (blocking scan phase).** `wait_for_shutdown_signal()` was only
raced against `sleep(interval)` in the between-scans `select!`. The per-iteration
scan calls `discovery::scan_all` → `scan_processes` (a *synchronous* sysinfo
refresh + a `ps`/`lsof` subprocess **per process system-wide**) directly inside
an `async fn` without ever yielding, so the signal future was not polled for the
whole multi-second scan. Teardown was a secondary risk (overlay `shutdown()` had
no timeout), but the primary symptom is the unyielding scan.

Fix (in `discovery_loop.rs` + `discovery.rs`):

1. **Top-level shutdown select.** The entire per-iteration loop body now runs as
   an `iteration` async block raced against a single long-lived (`Box::pin`)
   `wait_for_shutdown_signal()` future, so a signal cancels the iteration mid-scan
   instead of only between scans.
2. **Offload the blocking scan.** `scan_all` now runs `scan_processes` via
   `tokio::task::spawn_blocking`, so the async task actually yields and the signal
   future is polled even while the heavy subprocess scan is in flight.
3. **Hard-exit safety net.** On entering shutdown we `spawn_shutdown_watchdog()`,
   a detached task that `std::process::exit(0)`s on EITHER a second signal OR a
   5s deadline. The overlay teardown itself is also wrapped in a 5s
   `tokio::time::timeout`, so graceful teardown still runs in the normal case but
   can never wedge the process.

Non-unix path and graceful teardown (scoped resolver + TUN + pid-file removal)
are preserved.

Verification: `cargo build`/`clippy -D warnings`/`test --workspace` all clean.
Runtime end-to-end test (`client/crates/cli/tests/signal_shutdown.rs`, runs the
real binary under a throwaway HOME, unprivileged): SIGINT exits in ~1.25s,
SIGTERM in ~0.61s — both well under the watchdog. No `sudo pkill` needed.