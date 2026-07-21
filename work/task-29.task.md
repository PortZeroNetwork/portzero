---
id: aaca088a-a35c-4add-96a8-c2bf2ae38f7a
slug: task-29
status: done
title: 'Decision: macOS (and Windows) daemon elevation model'
milestones:
- milestone-2
created_at: 2026-06-23T13:03:49.537177Z
updated_at: 2026-06-23T13:03:49.537177Z
---

> ADR-style decision ticket. (`.ticketry.toml` reserves a `decision` prefix but
> no `decision` ticket type is configured, so this is a normal task acting as
> the ADR. Implementation lives in [[[task-23](../work/task-23.task.md)]] for macOS and [[[task-28](../work/task-28.task.md)]] for
> Windows.)

## Context

Linux runs the daemon **unprivileged** by granting the binary `CAP_NET_ADMIN`
file capabilities. macOS and Windows have **no `setcap` equivalent**, yet the
overlay needs root/Administrator to create the tunnel device, write scoped DNS,
and edit routes. Today macOS autostart installs a **LaunchAgent** (user-level,
unprivileged) so the overlay silently degrades — see [[[task-23](../work/task-23.task.md)]]. We need a
single decision on how the daemon obtains privileges on macOS (and Windows) that
the autostart + install flow can implement.

## Approach

Choose among the options and record the rationale here, then drive
implementation through [[[task-23](../work/task-23.task.md)]] / [[[task-28](../work/task-28.task.md)]]:

- **A — Root LaunchDaemon / Administrator scheduled task.** Service runs
  privileged at boot; one-time `sudo`/admin step at install. Simplest, no Apple
  signing identity required. Likely the default.
- **B — Network Extension / System Extension** with the
  `com.apple.developer.networking.networkextension` entitlement (macOS): no
  root, but requires an Apple Developer signing identity + notarization. Better
  UX, heavier process; possibly a later milestone.
- **C — Privileged helper** (SMJobBless / Windows service installed by an
  elevated installer): most moving parts.

Consider: install UX, code-signing/notarization cost, how `install.sh` conveys
the privilege step, and keeping graceful unprivileged degradation intact.

## Decision

**Chosen: Option A — Root LaunchDaemon (macOS) / Administrator scheduled task
(Windows).**

Rationale: it is the simplest path to a working privileged overlay and requires
no Apple signing identity or notarization. The daemon runs as root from boot, so
utun creation, scoped `/etc/resolver` writes, and route edits all succeed; the
cost is a one-time `sudo` step at install/uninstall. Network Extensions (B) and
a privileged helper (C) buy nicer UX but add a Developer-ID/signing dependency
and many more moving parts — deferred to a possible later milestone.

- macOS path → [[[task-23](../work/task-23.task.md)]]: autostart now writes a system-domain LaunchDaemon at
  `/Library/LaunchDaemons/tools.devenv.daemon.plist`, loaded via
  `launchctl bootstrap system`. Install/uninstall require root and fail fast
  with a `sudo` hint otherwise.
- Windows path → [[[task-28](../work/task-28.task.md)]]: Administrator-installed scheduled task / service
  (same elevation principle), to be implemented there.

Decision (fill in):

- [x] Option chosen + rationale recorded here
- [x] macOS path handed to [[[task-23](../work/task-23.task.md)]]; Windows path handed to [[[task-28](../work/task-28.task.md)]]
- [x] `docs/privileges.md` updated to reflect the decision