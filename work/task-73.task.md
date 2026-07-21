---
id: 48afe7f1-d39b-470f-9c9e-730829ff7a7c
slug: task-73
status: todo
title: Overlay startup deadline discards slow-but-successful overlay on Windows
created_at: 2026-07-10T17:00:07.792102Z
updated_at: 2026-07-10T17:00:07.792102Z
---

## Symptom

On Windows, local `.portzero.local` overlay tunnels intermittently fail to come up on first run or on slow/loaded machines. The daemon log shows the overlay reaching "Complete" and then immediately:

```
WARN overlay startup completed after the deadline; shutting it down
INFO Windows: removed NRPT rule for portzero.local
```

## Root cause

`run_discovery_loop` (client/crates/daemon/src/discovery_loop.rs) wrapped overlay startup in a hard 30s deadline (`OVERLAY_START_TIMEOUT`). The deadline existed so overlay startup couldn't block cloud tunnel discovery (the CI staging interop test needs cloud route registration within ~90s). On timeout the daemon continued cloud/local-only AND spawned a task that **tore the overlay down if it later completed**.

But overlay startup time varies widely: first-run wintun *driver install + adapter creation* alone is ~50s on a slow VM (measured 53s), pushing total startup to ~85s — well past 30s. It came up correctly, then got discarded. Even on a fast SSD VM, startup straddles 30s (~25–30s), making local tunnels flaky. Reproduced in the Parallels VM harness ([[parallels-vm-testing]]): local-overlay E2E flips PASS/FAIL depending on whether startup beats 30s.

## Fix

Decouple overlay startup from the main loop entirely: start it on a blocking-pool thread and have the discovery loop **adopt** the overlay whenever its task finishes (fast or slow), via a non-blocking `is_finished()` check at the top of each scan iteration. Cloud discovery runs unblocked the whole time; a slow-but-successful overlay is embraced, never torn down. Removed `OVERLAY_START_TIMEOUT`. Compiles, 283 daemon tests pass, clippy clean. Validating in-VM on slow storage (external Parallels VM, ~85s startup) — must now PASS.

Related: [[the v0.1.0 trust-install hang]] (task-69) — the original reason a deadline was added; that hang should be bounded at the `trust::install` subprocess level instead, which this fix does not itself address.
