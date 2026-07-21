---
id: 2c4637f2-5092-460d-afbb-c8806ee3dd6f
slug: task-33
status: done
title: 'macOS overlay DNS unreachable on gateway IP: bind to loopback + /etc/resolver port'
milestones:
- milestone-2
created_at: 2026-06-23T15:34:23.989825Z
updated_at: 2026-06-23T15:34:23.989825Z
---

## Context

Found by the [[[task-22](../work/task-22.task.md)]] re-run after [[[task-31](../work/task-31.task.md)]] (the overlay now starts on
macOS). DNS resolution of `*.devenv.local` fails — `dig @10.254.0.1
hello.devenv.local` **times out** (server unreachable), and `curl` reports
"Could not resolve host".

Root cause: `net/overlay.rs:44` hardcodes the DNS listen address to the overlay
gateway:

```rust
dns_listen: SocketAddr::new(IpAddr::V4(gateway_ip()), 53),  // 10.254.0.1:53
```

and `net/dns.rs` binds a real `UdpSocket` there. On Linux this is reachable
because the kernel installs a local route for the interface address, so queries
to `10.254.0.1:53` are delivered to the local socket. On **macOS the utun is
point-to-point** and there is NO local route for `10.254.0.1` — the run's
`netstat -rn` showed only `10.254/16 -> utun4` (plus a bogus `10.0.0.255` p2p
peer). So a packet to `10.254.0.1:53` routes OUT the tunnel (into the smoltcp
reader), never reaching the real `UdpSocket`. Hence the timeout.

## Approach

- Make the DNS listen address platform-specific. On **macOS, bind the
  resolver-facing DNS to loopback** — `127.0.0.1:<port>` (a fixed, non-colliding
  high port, e.g. `10053`, kept as a named const so it's easy to change). Linux
  keeps `gateway_ip():53` (works there; don't change it).
- Pass that same `SocketAddr` to `resolver_config::install`, so
  `macos_resolver_file_content` writes `nameserver 127.0.0.1` + `port <port>`
  (the `port` line is already supported). `/etc/resolver/devenv.local` then
  points the system resolver at the reachable loopback server.
- The overlay DATA path is unaffected — service VIPs are `10.254.x.y` (not the
  gateway), so they still route through the tunnel/smoltcp as intended; only the
  embedded DNS server moves off the unreachable gateway IP.

## Verify (needs a privileged re-run by the user — can't be done in CI/agent)

- `dig @127.0.0.1 -p <port> hello.devenv.local` answers (server now reachable).
  - If it returns a `10.254.x.y` VIP → DNS + registration work; remaining issue
    is macOS routing `.local` to our server ([[[task-25](../work/task-25.task.md)]] mDNS/Bonjour).
  - If it returns NXDOMAIN/empty → the server is reachable but the service is not
    registered → chase the registration gap (see note below).
- `dscacheutil -q host -a name hello.devenv.local` / `curl http://...:8080/`
  exercise the full system-resolver path ([[[task-25](../work/task-25.task.md)]]).

## Note: companion symptom to confirm after this fix

The same re-run showed `portzero status` with **no overlay routes** even
though `lsof` proves the service listens on `127.0.0.1:50706` and the daemon saw
its `PZ_TUNNEL`. Could not be isolated while the DNS server was unreachable.
Once this fix lands and `dig @127.0.0.1` works, confirm whether the service
registers (`scan_network_processes` → VIP → DNS record → `overlay.json`). If
still missing, file a dedicated discovery/registration ticket then.

Done when:

- [x] macOS binds the embedded DNS to loopback; `/etc/resolver/devenv.local`
      points at `127.0.0.1:<port>` (`127.0.0.1:10053`,
      `MACOS_DNS_LOOPBACK_PORT`)
- [x] Linux DNS binding unchanged; builds clean with `clippy -D warnings`
- [x] `dig @127.0.0.1 -p <port> <name>` reachable on a privileged re-run
- [x] Registration symptom re-checked — registered after [[[task-34](../work/task-34.task.md)]]; `dig @127.0.0.1 -p 10053` returns `10.254.0.2`