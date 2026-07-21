---
id: 90c60514-3fb9-4866-b2e0-1932ad263477
slug: task-31
status: done
title: 'macOS TUN never created: pass None for kernel-assigned utun (bare "utun" name panics tun crate)'
milestones:
- milestone-2
created_at: 2026-06-23T14:41:22.788764Z
updated_at: 2026-06-23T14:41:22.788764Z
---

## Context

Found by the [[[task-22](../work/task-22.task.md)]] privileged run on macOS. The overlay never starts —
even as root — because **TUN device creation fails** at step 1:

```
failed to create TUN device (are you root?): cannot parse integer from empty string
```

Root cause confirmed in `tun-0.6.1/src/platform/macos/device.rs`:

```rust
let id = if let Some(name) = config.name.as_ref() {
    if !name.starts_with("utun") { /* err */ }
    name[4..].parse::<u32>()? + 1u32   // "utun"[4..] == "" -> parse error
} else {
    0u32                                // None -> kernel auto-assigns next free utunN
};
```

`net/tun_device.rs::create` always sets the name via
`config.name.clone().unwrap_or_else(|| default_device_name())`, and
`default_device_name()` returns the bare `"utun"` on macOS. So we pass
`Some("utun")` → `"utun"[4..]` is empty → parse error. The crate's `None` branch
is exactly the "let the kernel pick the next free utunN" behaviour the existing
(wrong) code comment claims the bare prefix gives.

Because the overlay aborts here, the route, `/etc/resolver/devenv.local`, and
the whole data path never run — which is why [[[task-24](../work/task-24.task.md)]] and [[[task-25](../work/task-25.task.md)]] could
not be assessed.

## Approach

- On macOS, when no explicit device name is requested, **do not call
  `tuncfg.name(...)`** (leave it `None`) so the crate uses `id = 0` and the
  kernel assigns the next free `utunN`. Keep Linux (`deven0`) unchanged.
- After creation, `actual_name` already comes from `dev.get_ref().name()` (the
  crate queries the real `utunN` via getsockopt), so route install/teardown keep
  working with the kernel-assigned name.
- Do NOT bump the `tun` crate (0.7.x changes the packet-header handling and would
  entangle [[[task-24](../work/task-24.task.md)]]). Keep this a minimal, macOS-scoped fix.
- Fix the misleading `default_device_name()` / `TunConfig` doc comment that says
  the bare `utun` prefix lets the kernel choose.

Done when:

- [x] macOS `OverlayNetwork::start` creates a `utunN` device as root (no parse
      error) — verified by a re-run of the [[[task-22](../work/task-22.task.md)]] runbook
- [x] `10.254.0.0/16` route + `/etc/resolver/devenv.local` then install
      (unblocks assessment of [[[task-24](../work/task-24.task.md)]] / [[[task-25](../work/task-25.task.md)]])
- [x] Linux device creation (`deven0`) unchanged; builds clean with
      `clippy -D warnings`
- [x] Misleading comments corrected

## Verified (run 2026-06-23, macOS, as root)

Re-run of the [[[task-22](../work/task-22.task.md)]] runbook with the fix (`adf3b39`):

```
TUN device created: name=utun4 address=10.254.0.1/16 mtu=1500
route add succeeded: route -n add -net 10.254.0.0/16 -interface utun4
overlay DNS listening on 10.254.0.1:53
macOS: wrote scoped resolver /etc/resolver/devenv.local
Virtual overlay network started (.devenv.local)
```

`ifconfig` shows `utun4` with `inet 10.254.0.1`, `netstat -rn` shows
`10.254/16 -> utun4`, `/etc/resolver/devenv.local` written. The overlay now
starts. Remaining DNS-resolution + data-path issues are downstream
([[[task-25](../work/task-25.task.md)]] / [[[task-24](../work/task-24.task.md)]]).

## Implementation notes

Fixed in `client/crates/daemon/src/net/tun_device.rs`. Extracted the
name-selection decision into a pure `select_device_name(Option<&str>)` helper
returning a `DeviceNameSelection { set_name, requested_name }`:

- **macOS + `None`** (the daemon's default path): `set_name = None`, so
  `create()` does NOT call `tuncfg.name(...)`. The `tun` crate then uses utun
  unit `id = 0` and the kernel assigns the next free `utunN`. The non-empty
  placeholder `"utun"` is kept only for logging if the post-create name query
  fails.
- **macOS + `Some("utunN")`**: honoured verbatim (caller requested a specific
  unit).
- **Linux/Windows**: always sets a concrete valid name (caller's or `deven0`) —
  byte-for-byte unchanged.

`actual_name` is still derived from `dev.get_ref().name()` after creation, so
route install/teardown use the real kernel-assigned name. The stale
delete/retry path is unaffected on macOS (`delete_device_command` still returns
`None` there). Doc comments on `default_device_name()` and `TunConfig::name`
corrected.

Verified: `cargo build --workspace` (clean, no warnings),
`cargo clippy --workspace --all-targets -- -D warnings` (passes),
`cargo test --workspace` (passes; added unprivileged unit tests for
`select_device_name`). Did NOT bump the `tun` crate.

The first two boxes need a privileged (root) re-run of the [task-22](../work/task-22.task.md) runbook to
confirm a `utunN` actually comes up — left unchecked. Runtime unverified.