# Which address the daemon dials

Discovery finds a port. Forwarding needs an *address*. This page records how
one becomes the other, because getting it wrong produces the least helpful
failure the product has: the tunnel connects, then returns nothing.

## The rule

Every listener discovery finds is recorded as a `ListeningPort { port, addr }`
(`discovery.rs`) — the address exactly as the OS reports it. `dial_addr()`
turns that into something connectable:

| Bound to | Dialed |
|----------|--------|
| `0.0.0.0` (IPv4 wildcard) | `127.0.0.1` |
| `::` (IPv6 wildcard) | `::1` |
| `127.0.0.1` / `::1` | itself |
| a specific address (`192.168.1.5`) | itself |

A wildcard address is not connectable, so it maps to loopback **of its own
family**. `::` maps to `::1` rather than `127.0.0.1` because a `::` socket
accepts `::1` whether or not `IPV6_V6ONLY` is set, while a v6-only socket
accepts nothing on `127.0.0.1`.

## Why the family, not just loopback-vs-public

`127.0.0.1` and `::1` are both loopback and are **not** interchangeable. A
process listening on `[::1]:5173` is unreachable from `127.0.0.1`; the connect
either fails or, worse, lands on some other process.

This is easy to hit by accident. Node ≥ 17 stopped reordering DNS results, so
on a dual-stack Linux host `localhost` resolves to `::1` first and a dev server
told to bind `"localhost"` (Vite's default) ends up IPv6-only without anyone
choosing that. The daemon used to hardcode `Ipv4Addr::LOCALHOST` at every dial
site, so those services were discovered correctly — right domain, right port,
right PID — and then answered every request with an empty reply.

The corollary is worth stating too: **loopback-only is fine.** Binding
`127.0.0.1` works through a tunnel. Telling users to bind `0.0.0.0` to "fix"
a tunnel exposes their dev server to the LAN to solve a problem they do not
have.

## Where the address comes from, per platform

| Source | Reads | Note |
|--------|-------|------|
| Linux | `/proc/<pid>/net/tcp` + `tcp6` | Hex, each 32-bit word in **host** byte order: `0100007F` is `127.0.0.1`, `…01000000` is `::1`. IPv4-mapped (`::ffff:…`) is normalized to IPv4. |
| macOS | `lsof -a -iTCP -sTCP:LISTEN -nP -p <pid>` | Prints a wildcard as a bare `*` for **both** families, so the family comes from the `IPv4`/`IPv6` TYPE column. Zone indices (`%lo0`) are stripped. |
| Windows | `Get-NetTCPConnection`, `netstat -ano` | Self-describing (`::`, `[::1]:…`); no hint needed. |
| Docker | `NetworkSettings.Ports[].HostIp` | The published host address. A port published on both families prefers IPv4. |

## Where it flows

- **Local tunnels** — `DiscoveredNetworkService::real_addr` is a `SocketAddr`
  carrying the address; the overlay proxy (`net/stack/engine.rs`,
  `tls/stack.rs`) has always just `connect`ed to it, so nothing there needed
  changing.
- **Cloud tunnels** — `DiscoveredService::host` → `Route::host` (persisted in
  `routes.json`) → `DomainRouter` (which maps a domain to a `SocketAddr`, not a
  port) → `forwarder::forward_request`. An unparseable persisted `host` falls
  back to `127.0.0.1`, the historical value.

## How it is tested

Three layers, because each one can only see so much:

- **Unit** — the platform parsers against real `/proc`, `lsof`, `netstat`, and
  Docker output lines, plus the `dial_addr` mapping table.
- **In-crate integration** — real bytes to a live `[::1]` listener: a cloud
  forward (`forwarder::tests`) and an overlay byte-proxy across the smoltcp
  stack (`tests/overlay_unprivileged.rs`).
- **VM system test** — `PHASE=local-tunnel-ipv6` in the combined flavor
  (`vmtest/scripts/combined-{linux,macos,windows}`) serves a second tagged
  service bound to `::1` alone, alongside the normal IPv4 one, and requires it
  to answer through its real tunnel on a real OS. This is the only layer that
  exercises the actual platform enumeration path — the unit tests feed those
  parsers synthetic lines; here the kernel writes them. A guest with no IPv6
  loopback SKIPs the leg rather than failing it.

## What this is not

This is IPv6 on the *backend* side only. The overlay itself is IPv4: VIPs are
`Ipv4Address`, the TUN gateway is `10.254.0.1`, and `*.portzero.local` resolves
to A records. Serving the overlay over IPv6 is a separate, much larger change.
