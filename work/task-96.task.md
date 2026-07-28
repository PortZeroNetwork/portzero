---
id: 0ecdfada-9c04-4e08-b96f-8ea822c1aa1e
slug: task-96
status: todo
title: 'Install: detect an existing owner and fail loudly instead of silently annexing'
created_at: 2026-07-28T00:00:00Z
updated_at: 2026-07-28T00:00:00Z
---

## Context

From [[decision-2]] (multi-user strategy, Option 1). Port Zero is
single-owner-per-machine on macOS/Windows, but nothing says or enforces
that: a second account running the privileged install today silently
overwrites `/Library/LaunchDaemons/tools.devenv.daemon.plist` (or the
Windows scheduled task) and takes over the machine-wide overlay.

## Approach

At privileged install/autostart-install time, detect an existing
installation owned by a **different** user (the plist's pinned `HOME` / the
task's configured user is the marker) and refuse with a clear error that
names the owning account and the explicit override path (uninstall by the
owner or an admin, then reinstall). Same-user reinstall/upgrade stays
idempotent and quiet. Follow `.instructions/user-facing-errors.md`: say what
was found, why the install stopped, and the exact commands to resolve it.

Done when:

- [ ] macOS: second-account install fails with owner name + next steps;
      same-account reinstall unchanged
- [ ] Windows: same behavior for the Administrator service/task
- [ ] Linux: n/a for ownership (per-user service), but starting a second
      account's daemon while another account holds the TUN/resolver produces
      a diagnosable error, not a silent contention loss
- [ ] `portzero doctor` / diagnostics report the machine's current owner
- [ ] Uninstall by the owner (or root) removes the marker cleanly
