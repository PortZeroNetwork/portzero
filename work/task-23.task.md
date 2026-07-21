---
id: c4bd24a6-7b74-4d87-96bb-3329e7e2b5de
slug: task-23
status: archived
title: 'macOS privilege & autostart model: LaunchDaemon (root) so the overlay actually runs'
relations:
  contains:
  - milestone-2
created_at: 2026-06-23T12:51:58.135867Z
updated_at: 2026-06-23T12:51:58.135867Z
---

## Context

This is the one genuine architectural gap, not just an untested path. On Linux
the daemon runs **unprivileged** because the binary is granted `CAP_NET_ADMIN`
file capabilities (the "file-capability daemon" work). macOS has **no `setcap`
equivalent** — creating `utun`, writing `/etc/resolver`, and editing routes all
need root or a signed Network Extension entitlement.

But `autostart.rs` installs a **LaunchAgent** in `~/Library/LaunchAgents`, which
runs as the logged-in user, **unprivileged**. So on macOS the overlay silently
degrades to cloud/local-only mode every time it autostarts — the `.devenv.local`
data path never comes up. `docs/privileges.md` already concedes the macOS answer
is "run with `sudo`", but nothing makes autostart privileged. Same root issue
will hit Windows ([[[task-28](../work/task-28.task.md)]]) for adapter setup.

## Approach

The elevation model itself is decided in [[[task-29](../work/task-29.task.md)]]; this ticket **implements**
the macOS side of that decision. Expected default (pending [[[task-29](../work/task-29.task.md)]]) is a
**LaunchDaemon in `/Library/LaunchDaemons`** running as root.

Update `autostart.rs` so the macOS branch installs the chosen mechanism instead
of the current user-level LaunchAgent (and `install.sh` / docs explain the
one-time privilege step). Keep graceful degradation when privileges are absent.

Done when:

- [x] Elevation model from [[[task-29](../work/task-29.task.md)]] implemented for macOS autostart
      (root LaunchDaemon in `/Library/LaunchDaemons`, loaded via
      `launchctl bootstrap system`; root-writable logs at `/Library/Logs/devenv`)
- [ ] macOS autostart brings the overlay up **privileged** (utun + resolver +
      route all succeed under the installed service, verified via [[[task-22](../work/task-22.task.md)]])
      — UNVERIFIED: needs a real `sudo` install + boot run, which can't be done
      in CI / from this environment.
- [x] Install/uninstall is clean and idempotent; teardown removes the service
      (install/uninstall require root and fail fast with a `sudo` hint;
      uninstall is a no-op when the plist is absent)
- [x] `docs/privileges.md` updated to match the implemented model
- [x] Unprivileged fallback still degrades gracefully (no hard failure) — the
      daemon's runtime degradation path is unchanged; only autostart
      install/uninstall now requires root.