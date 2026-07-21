---
id: 2a8cf42b-859e-449a-802b-6f8ffa24caf0
slug: task-22
status: done
title: 'macOS bring-up: first privileged run + runtime triage'
milestones:
- milestone-2
created_at: 2026-06-23T12:50:48.796935Z
updated_at: 2026-06-23T12:50:48.796935Z
---

## Context

The workspace already compiles cleanly on macOS and most platform branches are
implemented (utun in `net/tun_device.rs`, `/etc/resolver` in
`net/resolver_config.rs`, `lsof`/`ps` discovery in `discovery.rs`, LaunchAgent
in `autostart.rs`). What has **never executed** is the privileged macOS data
path. This ticket is the entry point: do a real privileged run, observe what
actually works end-to-end, and triage findings into the dependent tickets
([[[task-24](../work/task-24.task.md)]] utun header, [[[task-25](../work/task-25.task.md)]] DNS/.local, [[[task-26](../work/task-26.task.md)]] discovery).

## Approach

Run the daemon privileged and watch the overlay come up:

```
cargo build
sudo -E ./target/debug/port-zero start --foreground
```

Then, from a second shell, start an example `.devenv.local` service (see
`examples/local-overlay/`) and exercise the full path:

- Does a `utunN` interface appear and get `10.254.0.1/16`?
- Does the `10.254.0.0/16` route install (`netstat -rn | grep 10.254`)?
- Does `/etc/resolver/devenv.local` get written, and does
  `dscacheutil -q host -a name <svc>.devenv.local` resolve to the VIP?
- Does `curl http://<svc>.devenv.local/` actually carry traffic, or does it
  resolve but hang? (A hang points straight at [[[task-24](../work/task-24.task.md)]].)
- Does discovery attribute the listener to the right process via `lsof`/`ps`?

Capture the daemon log and record which privileged operations succeed vs.
silently degrade ("continuing in cloud/local-only mode").

Done when:

- [x] Privileged foreground run documented with the daemon log captured
- [x] Each overlay step (utun create, route, scoped resolver) marked
      works / broken / degraded
- [x] End-to-end `curl http://<svc>.devenv.local/` result recorded (carries
      traffic vs. resolves-but-hangs vs. no-resolve)
- [x] Findings triaged into the dependent tickets

## Findings (run 2026-06-23, macOS, as root)

The overlay **never starts** — TUN creation fails at step 1 even as root:

```
WARN ... Virtual overlay network not started (continuing in cloud/local-only
mode): failed to create TUN device (are you root?): cannot parse integer from
empty string.
```

Per-step result:

- **utun create — BROKEN.** Bare `"utun"` name → `tun` crate parse error. Root
  cause + fix tracked in [[[task-31](../work/task-31.task.md)]] (the real macOS blocker). Everything below
  is downstream of this.
- **route `10.254.0.0/16` — never ran** (`netstat -rn` empty). Blocked on
  [[[task-31](../work/task-31.task.md)]].
- **scoped resolver — never ran** (`/etc/resolver/devenv.local` absent,
  `dscacheutil` empty). Blocked on [[[task-31](../work/task-31.task.md)]]; [[[task-25](../work/task-25.task.md)]] re-pointed at it.
- **data path / `curl` — no-resolve** (overlay down). [[[task-24](../work/task-24.task.md)]]'s utun-header
  question is still UNASSESSED — we never reached the data path; re-pointed at
  [[[task-31](../work/task-31.task.md)]].
- **discovery — WORKS.** Log: `PZ_TUNNEL value ends in .local — routing to
  overlay path … domain="hello.devenv.local"`. The macOS `ps`-based env scan
  found the service. [[[task-26](../work/task-26.task.md)]] is an optimisation, not a fix.

Orthogonal (not parity bugs): auth token expired (`AuthFailed` — needs
`portzero login`); `examples/local-overlay/verify.sh` is Linux-only
(`resolvectl`) and needs a macOS DNS-check path.
