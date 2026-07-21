---
id: 2e55ad9c-36e6-4799-a631-a5c143761fb8
slug: task-16
status: done
title: Explicit canonical port in PZ_TUNNEL value (name:port) for the overlay
milestones:
- milestone-1
created_at: 2026-06-22T11:10:29.655256618Z
updated_at: 2026-06-22T11:10:29.655256618Z
---

## Problem

An overlay service binds **port 0** (ephemeral) and is reached at
`<name>.devenv.local`, but the overlay currently exposes it on the *discovered
ephemeral port*, so there is no stable/clean port to address it by
(`db.devenv.local:49152`). The `examples/local-overlay/verify.sh` CHECK 2 has to
curl that ephemeral port for exactly this reason.

Key insight: because each overlay service gets its OWN virtual IP from
`10.254.0.0/16`, there are **no port conflicts** across services even on
identical ports. So a service can be exposed on its NATURAL port (`5432`, `80`,
…) — `db.devenv.local:5432` and `cache.devenv.local:6379` coexist fine. We just
need a way to declare that canonical port.

## Design (decided)

Carry the canonical port in the `PZ_TUNNEL` value as a trailing `:port`:

```
PZ_TUNNEL=db.devenv.local:5432
PZ_TUNNEL=web-{branch}.devenv.local:8080
```

The overlay then listens on `VIP:<port>` and proxies to the real ephemeral
backend. When no `:port` is given, behavior is unchanged today (discovered
ephemeral port) — later superseded by protocol detection (see **[task-17](../work/task-17.task.md)**, which
treats explicit `:port` as the override).

## Parsing rules

- Strip the optional trailing `:<port>` off the value **before** template
  resolution and suffix classification. `<port>` must be an integer 1–65535;
  the remaining part is the domain (still a full domain; suffix decides overlay
  vs cloud).
- Templates: `web-{branch}.devenv.local:8080` → strip `:8080`, resolve
  `{branch}` on `web-{branch}.devenv.local`, then `port = 8080`.
- Invalid / out-of-range port → log a clear warning and IGNORE the port (fall
  back to current behavior); never break discovery.
- Scope the canonical-port behavior to the **overlay path**. For cloud
  (`.tunnel.portzero.cloud`) the edge assigns the URL, so a declared port is not
  meaningful there — document that it is ignored on the cloud path (or reserve
  for later).

## Implementation notes (verify in design)

- `PZ_TUNNEL` is read/validated in `discovery.rs` (`scan_network_services` /
  `scan_processes`) and resolved via the `domain` crate. The `(domain, Option<port>)`
  split likely belongs in the `domain` crate next to template/suffix logic.
- Plumb the canonical port through `DiscoveredNetworkService.service_port` (today
  this is the ephemeral port for plain processes) so the overlay's
  `ServiceTable::register` uses it as the listen port, with the real ephemeral
  address as the proxy target.

## Acceptance Criteria

- [ ] `PZ_TUNNEL=<name>.devenv.local:<port>` exposes the overlay service on
      `VIP:<port>`, proxying to the real ephemeral backend.
- [ ] Suffix-decides-target still works (parser strips `:port` before classification).
- [ ] Works with `{branch}`/`{worktree}` templates in the name part.
- [ ] No `:port` → unchanged current behavior.
- [ ] Invalid/out-of-range port → warning + graceful fallback, no crash.
- [ ] Pure parser unit-tested: value → `(domain, Option<port>)` incl. templates,
      no-port, trailing/invalid/out-of-range port; no system mutation.
- [ ] `examples/local-overlay/verify.sh` updated (or a follow-up noted) to use a
      fixed canonical port so CHECK 2 curls a clean `:port`.

## Relations

Part of [milestone-1](../work/milestone-1.task.md). Builds on the overlay ([task-1](../work/task-1.task.md) stack, [task-3](../work/task-3.task.md) wiring).
**[task-17](../work/task-17.task.md)** (HTTP/TLS protocol detection) depends on this and uses explicit
`:port` as its override.
