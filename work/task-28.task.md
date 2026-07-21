---
id: 11d9e269-5796-43de-9903-c8d541210572
slug: task-28
status: done
title: 'Windows parity: wintun data path, NRPT DNS, scheduled-task autostart'
milestones:
- milestone-2
created_at: 2026-06-23T12:52:01.023861Z
updated_at: 2026-06-23T12:52:01.023861Z
---

## Context

Windows is scaffolded but genuinely **stubbed and unproven** — further behind
than macOS. The branches exist but none has run:

- **Data path**: `tun` crate needs `wintun.dll` present next to the binary / on
  `PATH`; the connected route is expected to auto-install with the adapter
  address (no explicit route command). Unverified.
- **Scoped DNS**: `resolver_config.rs` shells out to `Add-DnsClientNrptRule`
  (PowerShell), which needs Administrator. Unverified.
- **Discovery**: there is **no Windows branch** in `discover_process_ports` /
  `scan_process_env` — both return empty on Windows, so nothing is discovered.
- **Autostart**: `schtasks /SC ONLOGON` at `LIMITED` run level — but like macOS
  ([[[task-23](../work/task-23.task.md)]]) a non-elevated task can't set up the adapter/DNS, so the overlay
  won't come up from autostart.
- **Privilege model**: same core problem as macOS — needs Administrator; no
  `setcap` equivalent.

## Approach

Bring Windows from "compiles" to "works", mirroring the macOS effort:

- Bundle / locate `wintun.dll` and verify the adapter + connected route carry
  traffic.
- Implement Windows port + env discovery (e.g. `GetExtendedTcpTable` via
  `windows`/`netstat` and `QueryFullProcessImageName` / WMI for env) so services
  are actually found.
- Verify the NRPT rule scopes `devenv.local` to the embedded DNS and tears down.
- Resolve the elevation model so autostart runs with Administrator rights.
- Fold Windows into CI ([[[task-27](../work/task-27.task.md)]]).

Done when:

- [ ] `wintun`-backed overlay carries traffic for `<svc>.devenv.local`
- [ ] Windows process/port + `PZ_TUNNEL` discovery implemented
- [ ] NRPT scoped DNS verified + reversible
- [ ] Autostart runs elevated and brings the overlay up
- [ ] Windows added to the CI matrix