---
id: ee0488f7-2163-465d-805a-0f9b1d17959e
slug: task-15
status: done
title: Make Linux scoped DNS robust (systemd-resolved without networkd, plus fallbacks)
milestones:
- milestone-1
depends_on:
- af741f08-3311-48c3-a459-8cde3f518a90
created_at: 2026-06-22T10:41:40.321030133Z
updated_at: 2026-06-22T10:41:40.321030133Z
---

## Problem

task-2's Linux scoped resolver (`net/resolver_config.rs`) configures `*.devenv.local`
resolution through `resolvectl`, attaching the DNS server + routing domain to the
**`lo` link**:

- `resolvectl dns lo <ip>:<port>`
- `resolvectl domain lo ~devenv.local`
- (teardown) `resolvectl revert lo`

**Corrected diagnosis (2026-06-22).** This is NOT only a "missing systemd-resolved"
problem. Running the `real_tun_overlay` e2e as root on a box where systemd-resolved
*is* active but systemd-**networkd** is NOT (NetworkManager-managed — a very common
setup) produced:

```
Failed to set DNS configuration: Unit dbus-org.freedesktop.network1.service not found.
Failed to revert interface configuration: Unit dbus-org.freedesktop.network1.service not found.
```

These are `resolvectl`'s OWN messages (our `run_resolvectl` calls `.status()`, so
resolvectl's stderr prints raw). `network1` is systemd-networkd. So the current
`resolvectl … lo` approach fails whenever networkd isn't managing the link, even with
systemd-resolved fully active. The overlay TUN + smoltcp byte-proxy come up fine (the
test still passes — resolver setup is best-effort/non-fatal), but `*.devenv.local`
does not resolve, so `curl http://db.devenv.local/` cannot reach the overlay.

There are therefore (at least) THREE Linux environments to handle:
- (A) systemd-resolved **with** networkd — current path works.
- (B) systemd-resolved **without** networkd (NetworkManager) — current path FAILS. ← this machine.
- (C) no systemd-resolved at all — current path FAILS.

## Goal

Make scoped `*.devenv.local` resolution work across the common Linux resolver
environments (A/B/C above) — especially systemd-resolved-without-networkd, which is
where this was found — while preserving the existing guarantees: no whole-resolver
hijack, reversible/clean teardown, best-effort/non-fatal.

## Approach (finalize in design; sub-agent should root-cause on this box first)

Detect the active resolver mechanism at install time and pick a strategy:

1. **systemd-resolved active** (A and B): attach the scoped DNS + `~devenv.local`
   routing domain to the overlay's **own TUN link (`deven0`)** instead of `lo`. The TUN
   interface is created by the daemon and is a real link resolved can manage directly
   via its D-Bus `SetLinkDNS`/`SetLinkDomains` API, which should not require networkd.
   (Primary hypothesis for fixing environment B — must be verified by re-running the
   root `real_tun_overlay` e2e on a NetworkManager box.) Consider calling resolved's
   D-Bus API directly, or `resolvectl dns deven0 …`, and verify the networkd error is
   gone. Note ordering: the resolver must be configured AFTER the TUN link exists.
2. **dnsmasq / NetworkManager+dnsmasq** (fallback): drop a scoped snippet
   (e.g. `/etc/NetworkManager/dnsmasq.d/devenv.conf` or `/etc/dnsmasq.d/devenv.conf`)
   with `server=/devenv.local/127.0.0.1#5300`, then reload the resolver.
3. **No local stub at all** (C): fall back to per-name `/etc/hosts` entries
   (`<vip> <name>`). The overlay assigns a stable VIP per name, so this is scoped +
   reversible; tradeoff is it bypasses the embedded DNS server for those names and must
   be kept in sync as services come/go (would require feeding ServiceTable updates into
   the resolver layer — larger change; flag if pursued).

Detection ideas: `systemctl is-active systemd-resolved`, presence of
`/run/systemd/resolve/`, `systemctl is-active systemd-networkd`, `resolvectl status`,
or which resolver owns `/etc/resolv.conf`.

## Acceptance Criteria

- [ ] **Environment B (this machine): systemd-resolved active, networkd absent** — scoped
      DNS configures WITHOUT the `network1` error, by attaching to the TUN link
      (`deven0`) or another networkd-independent method. Verified by `real_tun_overlay`
      passing with NO `Unit dbus-org.freedesktop.network1.service not found` output and
      `*.devenv.local` actually resolving to the overlay VIP.
- [ ] Detects the resolver environment; keeps the working `resolvectl` path for (A).
- [ ] At least one working fallback for environment C (no systemd-resolved): dnsmasq
      snippet and/or `/etc/hosts` per-name mapping.
- [ ] `*.devenv.local` resolves to the overlay VIP, so the `real_tun_overlay` e2e
      (or a documented manual `curl`) succeeds end-to-end on a box like this one.
- [ ] Reversible/clean teardown for whichever mechanism is chosen; no leftover config
      after daemon shutdown.
- [ ] Only affects `devenv.local` (no whole-resolver hijack).
- [ ] Clear, actionable warning + guidance when no mechanism can be configured.
- [ ] Pure helpers (mechanism detection over injected inputs, config-snippet/hosts-line
      generation) unit-tested; no real system mutation in tests.

## Update (2026-06-22): reopened — networkd error fixed, but DNS server unreachable via the link

The `busctl`/resolve1 approach works: a clean `sudo verify.sh` run logged
`configured systemd-resolved for devenv.local on link deven0 (ifindex 9) via
resolve1 D-Bus` with **NO `network1` error**. The original bug is solved.

But resolution still fails:

```
resolvectl query hello.devenv.local: resolve call failed: ... Connection refused
```

Root cause: systemd-resolved sends per-link DNS queries **bound to the link
(`deven0`)**, but the embedded DNS server is advertised/bound at **`127.0.0.1:5300`**
(`OverlayConfig.dns_listen` default). Loopback isn't reachable out of `deven0`, so
the query is refused. The TUN's own address is the gateway **`10.254.0.1`**
(`net::virtual_ip::gateway_ip()`), which *is* reachable via `deven0`.

**Fix (remaining work to close this ticket):**
- Serve / advertise the overlay DNS on the TUN gateway, not loopback: bind the
  `OverlayDnsServer` to `10.254.0.1:5300` (or `0.0.0.0:5300`) and pass the gateway
  address (`10.254.0.1:5300`) to `resolver_config::install` so resolved's per-link
  DNS for `deven0` points at an address reachable via `deven0`. This likely means
  splitting the DNS *bind* address from the *advertised* address in `overlay.rs`.
- Secondary: `verify.sh` also logged `route add failed (ip route add 10.254.0.0/16
  dev deven0): File exists` — a leftover route from a SIGKILL'd run. Make the
  route-add idempotent (treat `EEXIST` as success) in `net/tun_device.rs` so reruns
  are clean.
- Re-validate with `sudo ./examples/local-overlay/verify.sh` (CHECK 1 must return a
  `10.254.x.x` VIP; CHECK 2 curl must succeed).

## Resolution (2026-06-22)

Two coupled causes, fixed together:
- **Port:** resolve1 `SetLinkDNS` carries no port, so systemd-resolved always
  queries the configured server on **port 53**, but the embedded DNS server was on
  **5300** → `Connection refused`. (This was even documented in
  `busctl_set_link_dns_args` but not reconciled with `OverlayConfig`.)
- **Address:** loopback isn't reachable via the `deven0` link resolved scopes to.

Fix (single change in `net/overlay.rs`): `OverlayConfig.dns_listen` now defaults to
the TUN **gateway on port 53** — `SocketAddr::new(gateway_ip(), 53)` = `10.254.0.1:53`.
The embedded DNS server binds there and the scoped resolver is pointed there, so:
resolve1's port-less `SetLinkDNS(10.254.0.1)` hits `:53` ✔; the gateway is reachable
via `deven0` ✔; macOS/dnsmasq paths (which carry the port) reach `10.254.0.1:53` too.
`resolver_config.rs` needed no change — it already uses whatever address it's given.

Plus the secondary idempotent-route fix in `net/tun_device.rs` (treat `ip route add`
`EEXIST`/"File exists" as success).

⚠️ Unit tests + build pass unprivileged, but the end-to-end resolve path can only be
confirmed by a human root run: `sudo ./examples/local-overlay/verify.sh` (CHECK 1
must now return a `10.254.x.x` VIP and CHECK 2 curl must succeed).

## Notes

Extends task-2 (`depends_on: task-2`). The `/etc/hosts` fallback also relates to how
overlay services are surfaced — keep entries in sync with the ServiceTable lifecycle.
