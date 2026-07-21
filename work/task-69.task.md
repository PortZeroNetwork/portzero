---
id: 3f96d0ff-6b88-445b-a388-aa17c0231809
slug: task-69
status: in-progress
title: Windows trust::install hangs indefinitely (root cause of v0.1.0 release blocker)
created_at: 2026-07-09T19:13:47.961666Z
updated_at: 2026-07-09T19:13:47.961666Z
---

## Context

The v0.1.0 Windows client interop failures were caused by daemon startup wedging inside `OverlayNetwork::start`. The mitigation (30s startup deadline + per-step logging, commit 781181b) made the daemon resilient and CI green, and the step logging from Release run 29042846603 pinned the exact wedge:

```
19:11:04.839 overlay startup step: LoadingLocalCa
19:11:04.842 overlay startup step: InstallingTrust
19:11:34.844 WARN Virtual overlay network startup did not complete within 30s
```

`trust::install` (client/crates/daemon/src/tls/trust.rs, Windows `install_impl` → `install_windows_root_store` / `install_nss_dbs_windows`) hangs indefinitely on GitHub's windows-latest runner (reproduced 3/3 before the deadline was added).

## Impact after mitigation

On any Windows machine where this hang reproduces, daemon startup is delayed 30s, the overlay never comes up (cloud tunnels still work), and the local CA is never installed into the trust store, so `.portzero.local` HTTPS shows cert warnings.

## Investigation leads

- certutil -addstore on the machine Root store may be waiting on something (elevation UI? CryptoAPI network retrieval?) even when run as Administrator in a non-interactive session.
- `install_nss_dbs_windows` scans Firefox profiles and runs NSS certutil; windows-latest ships Firefox, and NSS certutil can block on a locked cert9.db.
- Child-process calls in trust.rs have no timeouts; whatever the cause, each external command invocation there should get a hard timeout so one stuck tool can't consume the whole 30s overlay budget.
- CI's e2e job (real TUN overlay) passes on windows-latest — compare what it does differently (it skips trust install: see task-54 "skip trust install in real TUN e2e", which suggests this hang may have been encountered before).

## Progress (defensive fix landed, root cause still unconfirmed)

Implemented the ticket's most-actionable lead: every external child process the
trust installer spawns now runs under a hard watchdog (`run_command_capture` /
`TRUST_COMMAND_TIMEOUT`, currently 10s) in `client/crates/daemon/src/tls/trust.rs`.
A wedged child is killed and the failure is logged with the command/phase name
instead of consuming the whole 30s overlay budget. This covers every confirmed
and suspected hang site:

- `is_mozilla_certutil_windows` — the `certutil.exe -H` probe run over every
  non-System32 `certutil.exe` on PATH (the site the vmtest scripts reproduce).
- `run_certutil` / `run_certutil_output` — NSS `certutil` add/delete/list/init,
  which can block on a locked `cert9.db`.
- `run_command` (Windows + Linux/macOS) — any other external tool.

Composition with the existing skip: `OverlayConfig.install_trust = false` (task-54)
already skips the whole `trust::install`; the watchdog lives *inside* it, so the
two compose without duplication.

Cross-platform unit tests cover the watchdog (kills an overrunning child, returns
output for a fast one, reports spawn failure). `install_windows_root_store` uses
the CryptoAPI directly (no child process), so it is not a subprocess-hang site.

### Left to do before this closes the v0.1.0 blocker
- The OS-level root cause (why `certutil`/CryptoAPI/NSS wedges under a
  non-interactive Administrator/SYSTEM session on windows-latest) is still
  UNKNOWN and was not reproducible from the Linux dev sandbox.
- Validate on a real Windows box / the next windows-latest Release run that the
  daemon now completes startup within 30s (overlay comes up; trust step logs a
  timeout-and-skip rather than wedging). Only then downgrade the v0.1.0 risk.
- Consider whether the 10s per-command timeout is right given multiple NSS DBs
  could still sum toward the 30s budget; a real repro should confirm the value.
- Kept `status: in-progress` deliberately — the code fix is complete but the
  underlying blocker is not verifiably resolved without a Windows repro.
