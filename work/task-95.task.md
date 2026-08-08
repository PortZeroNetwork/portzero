---
id: 8485163b-d13b-4783-b757-4743251ef0a2
slug: task-95
status: done
title: 'tray: PID reuse makes a dead tray look alive, wedging autostart and lying in doctor/version'
created_at: 2026-08-08T13:03:18.587650123Z
updated_at: 2026-08-08T13:33:51.099136566Z
---

## Context

A machine running PortZero 1.1.6 (KDE Plasma / Wayland) had a working daemon,
working tunnels and a working desktop app, but **no tray icon** — while
`portzero version` and `portzero doctor` both reported the tray as running.

Root cause chain:

1. The tray exited at some point without clearing `~/.portzero/tray.pid` or its
   `~/.portzero/components/tray.json` record. Both kept PID **4171**.
2. The desktop session later restarted and `/usr/bin/kaccess` (PID 4162) spawned
   a `QXcbEventQueue` thread that was assigned **TID 4171**.
3. `pid_lookup::pid_is_alive` on Linux was `Path::new("/proc/<pid>").exists()`.
   `/proc/<n>` resolves for *thread* ids too (hidden from `readdir`, but
   stat-able), so this returned `true`.
4. `singleton::existing_owner` therefore saw a live owner and `portzero-tray`
   declined to start — logging at `info` and exiting 0, which at login-time
   autostart goes nowhere. **Every login, permanently.**
5. `versions::read_record` likewise kept the stale record, so every surface
   reported "Tray … running".

Nothing anywhere pointed at the problem: `doctor` had no tray check at all.

## Approach

Identity, not just liveness. A PID from a file only answers "is that component
still running" while it is still running *that component's binary*.

- `pid_lookup::pid_is_alive` (Linux): require `Tgid == pid` so thread ids no
  longer read as processes.
- New `pid_lookup::process_is_alive_named(pid, stem)`: live process **and**
  running the expected binary. Linux reads `/proc/<pid>/exe` (stripping the
  `" (deleted)"` marker an in-place upgrade leaves, so a live pre-upgrade
  component is not mistaken for something else) falling back to `comm`; macOS
  `ps -o comm=`; Windows `QueryFullProcessImageNameW`. Falls back to plain
  liveness where the binary cannot be identified — no worse than before.
- `Component::binary_stem()` maps each component to its binary; used by
  `versions::read_record` and the tray singleton.
- `singleton::release()` on clean exit, so the lock is not left behind at all in
  the common case.
- The refusal now prints to stderr with next steps, instead of exiting 0 silently.
- New `diagnostics/checks_tray.rs` + a `tray` check in `portzero doctor`: finds
  the tray by **process scan** (never by the files that are what go wrong), and
  only warns when tray autostart is installed, so a deliberately-quit tray is
  not nagged.
- Fixed the same thread-vs-process confusion in `check_multiple_instances`,
  where sysinfo enumerating threads could report "multiple daemons" for one
  healthy daemon (threads inherit the name *and* `--foreground` cmdline).

## Verification

Reproduced and fixed against the live wedged machine:

- before: `Tray 1.1.6 running`, `doctor` all green, `portzero-tray` exits 0
- after: `Tray unknown not running`, `doctor` warns with a runnable fix, and the
  tray starts, reclaims the stale lock, and registers
  `org.kde.StatusNotifierItem-<pid>-1` with the KDE StatusNotifierWatcher.

Regression tests cover the thread-id case, the recycled-PID case for both the
singleton and the component record, and lock release/non-release.
