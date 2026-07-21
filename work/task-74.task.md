---
id: 17b96ab9-576c-421a-8c17-8567e8807db5
slug: task-74
status: done
title: macOS 15.7+ hides process env from ps -E, breaking daemon service discovery
created_at: 2026-07-10T19:26:10.069650Z
updated_at: 2026-07-10T19:26:10.069650Z
---

## Symptom

On macOS 15.7.7 (Sequoia), the daemon discovers NO `PZ_TUNNEL`-tagged services, so neither local `.portzero.local` overlay tunnels nor cloud tunnels register. Reproduced in the Parallels macOS VM ([[parallels-vm-testing]]): both E2E flows (local-overlay, staging-cloud) fail — the daemon connects to the cloud edge fine but never registers a route, and the overlay comes up (DNS queries arrive) but never resolves a service.

## Root cause

macOS service discovery reads `PZ_TUNNEL` from process environments via `ps -wwwE`:
- `scan_process_env_macos` → `ps -p <pid> -wwwE -o command=`
- `scan_network_processes_sync_macos` → `ps -axo pid=,command= -wwwE`

On macOS 15.7.7, `ps -E` no longer exposes process environments — verified airtight: a process with `PZ_MARKER=zzq` in its ENV (not argv), queried as ROOT with `sudo ps -p <pid> -Eww -o command=`, returns just `sleep 30` with no env. So `ps -E` yields no `PZ_TUNNEL` and discovery finds nothing. Apple appears to have hardened `ps -E` in a 15.x update — GitHub's `macos-15` CI runner (where the interop test passes) must predate this.

## Impact

portzero's env-tag discovery is broken for users on macOS 15.7+. Windows (ReadProcessMemory) and Linux (`/proc/<pid>/environ`) are unaffected and both E2E flows pass there.

## Fix direction (needs design + verification)

Replace `ps -E` on macOS with a direct env read via `sysctl` `KERN_PROCARGS2` (same-user/root can still read argv+env this way — this is how `ps` used to source it). Verify it isn't also restricted on 15.7. If procargs2 is restricted too, env-based discovery may be unworkable on recent macOS and an alternative tagging mechanism is needed.

---

## Update 2026-07-10 — corrected root cause (the `ps -E` diagnosis was confounded)

Verified empirically on a real macOS 15.7.7 host (24G720), not a VM:

- **`KERN_PROCARGS2` and `ps -E` are equivalent.** Both read the env of ordinary
  user binaries; both are blocked from reading the env of a **SIP-protected
  system binary** (`/usr/bin/perl`, `/bin/sleep`, system python, …) — and this
  is true **even as root** (tested: root reading a root-owned system-`perl`
  gets `env_count=0` from both). The env-hiding is a kernel check in the shared
  `sysctl_procargs` path keyed on the target's protected/restricted status, not
  a `ps`-specific hardening.

- **Why the original repro looked airtight:** it tested with `sleep 30`, i.e.
  `/bin/sleep`, a SIP binary. Its env is hidden from everything — so `ps -E`
  (and procargs2) correctly showed nothing, but that generalized the wrong
  conclusion. For a non-SIP user binary, `ps -E` on 15.7.7 exposes env fine.

- **Why the VM E2E actually failed:** the tagged service is launched as
  `sudo env PZ_TUNNEL=… perl "$LIB" …` = system `/usr/bin/perl` (SIP-protected),
  whose env no process can read at any privilege. Windows/Linux pass because
  `/proc/<pid>/environ` and `ReadProcessMemory` don't have this restriction.

### Fixes landed

1. **Daemon** (`discovery/process.rs`): macOS env reading switched to
   `KERN_PROCARGS2` (old `ps -E` kept behind a single commented line at each
   site, with a two-approach explanation). This is **not** what unblocks the
   E2E — it is equivalent to `ps -E` for correctness — but it removes the
   per-scan `ps` subprocess spawn that caused slow scans / route flapping, so
   it is kept on those merits. New unit test `test_parse_procargs2_env`.

2. **E2E harness** (`e2e-local-overlay.sh`, `e2e-staging-tunnel.sh`): on macOS,
   run the tagged perl service from a **copy** of perl at an unrestricted path
   (`$WORK/perl`), which is not SIP-protected and whose env is readable.
   Verified on-host: copied perl serves `http-echo.pl` and its `PZ_TUNNEL` env
   reads back via both procargs2 and `ps -E`.

### Product implication (not yet actioned)

Real users who tag a dev server run under a **SIP/system interpreter**
(`/usr/bin/python3`, system `perl`, `/bin/sh`) will **not** be discovered on
macOS 15.7+, regardless of the daemon's read method. Users on homebrew/nvm/
rbenv/user-compiled runtimes are unaffected. Consider detecting this case and
surfacing a clear diagnostic (e.g. "tunnel tag on a macOS system binary can't
be read; run under a non-system interpreter").
