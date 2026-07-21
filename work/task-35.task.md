---
id: a7a05fac-4706-41d3-9680-c53af65fd97e
slug: task-35
status: done
title: Legacy-port monitor floods warnings for system services on macOS (e.g. sshd:22)
milestones:
- milestone-2
created_at: 2026-06-23T19:08:14.581933Z
updated_at: 2026-06-23T19:08:14.581933Z
---

## Context

Observed during the [[[task-22](../work/task-22.task.md)]] macOS runs: the legacy-port monitor floods the
daemon log and `portzero status` with warnings about **system services**:

```
WARN ... Port 22 is served directly (not via port-zero) by pid 606 (unknown
dir) — Set PZ_TUNNEL on this process ... Until then this service bypasses
the tunnel.
```

Port 22 is `sshd` — a system daemon the user will never tunnel. The legacy
monitor (`legacy_monitor.rs` + `enumerate_system_listeners` in `discovery.rs`)
enumerates ALL listeners system-wide and flags any on a "managed/common" port.
On macOS this is far noisier than on Linux because the daemon runs as **root**
(per [[[task-23](../work/task-23.task.md)]]) and therefore sees every process's listeners — including
system services (`sshd`, `mDNSResponder`, Parallels, ollama, JetBrains, etc.).

This is cosmetic (does not affect the overlay) but it's a real UX problem:
actionable warnings are drowned out.

UPDATE: the primary cause is the lsof `-a` bug ([[[task-36](../work/task-36.task.md)]]) — without `-a`,
`enumerate_system_listeners` attributes the WHOLE system's listeners to every
pid, so every process looked like it served 22/8080/etc. The [[[task-36](../work/task-36.task.md)]] fix
should eliminate most of this flood; re-verify whether any genuine scoping work
remains here afterward (the heuristic may still want a system-service denylist).

## Approach

- Scope the legacy-port heuristic so it doesn't flag system/well-known services.
  Options (pick after a quick look at the current heuristic):
  - Only flag listeners whose owning process is in the user's project context
    (has a discoverable cwd under a dev dir, or runs as the invoking user — not
    root/system uids), mirroring how the monitor already uses `cwd` /
    `has_port_zero`.
  - Exclude a denylist of well-known system ports (22, 53, 5353, 631, …) and/or
    well-known system process names.
  - Only warn for ports that actually collide with a discovered `.devenv.local`
    service, not every direct listener.
- Make sure the change is cross-platform (don't just suppress on macOS) and keep
  the genuinely-useful "your dev server bypasses the tunnel" case.

Done when:

- [x] System services (sshd:22, etc.) no longer generate legacy warnings
- [x] A real user dev server bypassing the tunnel is still flagged
- [x] `status` + daemon log are quiet on a normal macOS box
- [x] Cross-platform (Linux behaviour still sensible)

## Resolution

Superseded by a broader decision (a user reported 12 duplicate `LegacyListener`
entries in one `portzero status`/desktop-app view) rather than the scoped
denylist this ticket originally proposed: `LegacyListener` is no longer
surfaced as a user-facing diagnostic/problem at all. It is still detected
(`legacy_monitor::scan_legacy_listeners`) and still deduped per-scan on
`(port, pid)`, but `notify::collect_problems` now filters it out of the
`Problem` list the tray/desktop-app/`/status.json` render from, and
`discovery_loop::overlay::publish_issues` only ever logs it at `info` level
(never `warn`, never a desktop notification). `portzero status`'s
`print_issues` was updated to match.

This sidesteps the system-service-denylist approach entirely — sshd:22 and a
real bypassed dev server are now treated the same way (logged, not flagged),
which is coarser than the original "keep the genuinely useful case" goal but
correctly addresses the actual UX complaint (duplicate ERROR-level entries in
the desktop app) without maintaining a denylist. If a future need re-emerges
for surfacing *some* legacy listeners as actionable (e.g. only those on a
port a managed service also uses), reopen with the denylist/scoping approach
this ticket describes.