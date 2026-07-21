---
id: 20a6e935-ce4e-4404-a8c5-8df280b6923d
slug: task-30
status: done
title: Wire autostart enable/disable/status into the CLI (autostart.rs has no caller)
milestones:
- milestone-2
created_at: 2026-06-23T14:16:13.286435Z
updated_at: 2026-06-23T14:16:13.286435Z
---

## Context

Discovered while implementing [[[task-23](../work/task-23.task.md)]]: `client/crates/daemon/src/autostart.rs`
exposes `install_autostart` / `uninstall_autostart` / `is_autostart_installed`,
but **nothing calls them** — the module is only declared in `lib.rs`. So on
every platform (macOS LaunchDaemon, Linux systemd unit, Windows task) there is
no user-facing way to enable autostart. [[[task-23](../work/task-23.task.md)]]'s root-LaunchDaemon work is
correct but unreachable until this lands; the `require_root` sudo hint even
points at a CLI subcommand that doesn't exist yet.

## Approach

- Add CLI subcommands under the tunnel/daemon command group (match the existing
  clap structure in `client/crates/cli`) — e.g. `autostart enable`,
  `autostart disable`, `autostart status` — that call the three `autostart`
  functions.
- Surface the root requirement cleanly on macOS (install/uninstall now bail
  without root); make the help text and any error hints reference the REAL
  subcommand names so [[[task-23](../work/task-23.task.md)]]'s `require_root` message is accurate.
- Update `docs/` so the documented enable flow matches the actual command.

Done when:

- [x] `autostart enable/disable/status` (or equivalent) exist and call into
      `autostart.rs` — added `portzero autostart {enable,disable,status}`
      subcommand group in `client/crates/cli/src/autostart.rs`, wired into
      `main.rs`, calling `install_autostart`/`uninstall_autostart`/`is_autostart_installed`.
- [x] Root requirement is reported with the correct command name — macOS
      `require_root` hint now says `sudo port-zero autostart enable|disable`.
- [x] Help/docs updated; builds clean with `clippy -D warnings` — `privileges.md`
      documents the enable/disable/status flow; `cargo build/clippy -D warnings/test`
      all clean.
- [x] Reconcile the sudo hint added in [[[task-23](../work/task-23.task.md)]] with the real command —
      hint in `autostart.rs::require_root` updated to the real subcommand names.