---
id: 4152130d-5ee2-485e-b11d-e15da5d36a8b
slug: task-18
status: done
title: 'Robust TUN lifecycle: clean teardown + recover from leftover deven0 on startup'
milestones:
- milestone-1
created_at: 2026-06-22T15:37:09.457388478Z
updated_at: 2026-06-22T15:37:09.457388478Z
---

## Problem

The overlay's TUN device (`deven0`) is not cleaned up when the daemon stops, so
every subsequent start collides with the leftover device and the overlay
silently degrades to cloud/local-only mode:

```
WARN ... Virtual overlay network not started (continuing in cloud/local-only mode):
  failed to create TUN device (are you root?): Device or resource busy (os error 16)
```

Observed repeatedly during task-15 validation — each time it needed a manual
`sudo ip link delete deven0` to recover. Confirmed `deven0` stays `UP` with **no
daemon process running**.

### Root cause (teardown)

In `net/stack.rs`, `VirtualStack::spawn` does `tun.split()` into reader/writer
halves and moves each into a **detached** `tokio::spawn` (no `JoinHandle` kept).
`VirtualStack::shutdown()` only sends a `StackCommand::Shutdown` to the
`StackEngine`; it does NOT stop those two tasks. The **TUN reader task stays
parked forever in `tun_reader.read().await`**, holding the reader half (and thus
the fd + the `RouteGuard` that lives on the writer half), so the device is never
released on a normal shutdown.

### Why startup resilience is also needed

Even with perfect graceful teardown, an unclean exit (SIGKILL, panic, OOM, power
loss) WILL leave `deven0` behind. The daemon must be able to recover on the next
start rather than bricking its own overlay until a human deletes the device.

## Required fixes

1. **Clean teardown (`net/stack.rs`):** keep the `JoinHandle`s for the TUN
   reader and writer tasks on `VirtualStack`, and `abort()` them in `shutdown()`
   (in addition to the existing `StackCommand::Shutdown`). After shutdown the TUN
   halves must drop so the fd closes, the `RouteGuard` runs, and `deven0`
   disappears. Make the reader task cancellation-safe (its `read().await` is
   fine to abort).

2. **Startup resilience (`net/tun_device.rs`):** when `TunDevice::create` fails
   because the device already exists / is busy (`EBUSY` / "Device or resource
   busy"), delete the stale same-named device and retry once. On Linux: best-effort
   `ip link delete <name>` then re-create. On macOS, `utun` unit numbers are
   kernel-assigned (request `utun`, not a fixed unit) so collisions are unlikely —
   if a specific name was requested and collides, surface a clear error. On
   Windows/wintun, handle analogously or document. Keep it best-effort and logged;
   only delete a device that matches our overlay name (`deven0` / the configured
   name), never an unrelated interface.

## Acceptance Criteria

- [ ] After a normal daemon stop (SIGTERM), `deven0` is gone (no leftover device);
      an immediate restart creates the overlay successfully (no `EBUSY`).
- [ ] After an UNCLEAN exit that leaves `deven0` behind, the next daemon start
      detects the stale device, removes+recreates it, and the overlay starts
      (no manual `ip link delete` needed).
- [ ] The daemon never deletes a network interface other than its own overlay
      device name.
- [ ] `examples/local-overlay/verify.sh` passes twice in a row WITHOUT a manual
      `ip link delete deven0` between runs (human-validated, root).
- [ ] Pure/unit-testable parts (e.g. the "is this an already-exists/busy error"
      classifier, the recreate decision) are unit-tested; no real TUN in tests.

## Notes

Found during task-15 end-to-end validation (the overlay itself works — DNS→VIP→
TUN→smoltcp→backend confirmed; this is purely a device-lifecycle robustness gap).
Relates to task-3 (daemon lifecycle / graceful shutdown) and task-4 (TUN setup +
`RouteGuard`). Part of milestone-1.