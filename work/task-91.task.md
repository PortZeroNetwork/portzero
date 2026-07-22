---
id: cef35c66-b7bb-4385-b870-93436a26d0b1
slug: task-91
status: done
title: 'daemon: local-tunnel traffic produces no observed_edges/exercised_routes (scoped-resolver bypass?)'
created_at: 2026-07-22T16:49:40.235498961Z
updated_at: 2026-07-22T16:49:40.235498961Z
---

Observed on Linux (systemd-resolved scoped resolver) while building the full-example MCP walkthrough (task-90):

- DemoWeb queried Postgres via db.pzdemo.portzero.local on every request, and curl hit the app by its .portzero.local tunnel name, yet `observed_edges` and `exercised_routes` recorded nothing for either — only cloud-edge traffic appears. The likely cause is the scoped resolver answering with the real address so bytes bypass the daemon's proxy and are never observed. If so, the advertised who-talks-to-whom dependency graph is empty for purely local setups; the MCP server's own instructions ('only traffic addressed via tunnel names is observed') imply named local traffic IS observed.
- The walkthrough's `mcp-db-edge` step asserts the web→db edge and will keep failing until this is fixed or the observability contract is re-worded.

Separate smaller staleness issues seen the same session: `list_services` kept reporting a dead pid's cloud service, and a removed container lingered in a tunnel-name conflict until daemon restart.
## Findings & fix (2026-07-22)

**Root cause was NOT the scoped resolver.** DNS answers `*.portzero.local`
with the overlay VIP (10.254.x.x) and the bytes DO traverse the daemon's
smoltcp stack — but only the cloud connector ever recorded into the
`ObservationStore`. The local overlay proxy (`net/stack/engine.rs`) shuttled
opaque bytes with zero instrumentation, so purely local setups had an empty
graph despite carrying every named connection.

**Fix** (merged to staging, commits 3242cd8 + 1417766 + 3882c15):

- `observations.rs`: `record_connection` (TCP-level edges) and `HttpTap`
  (best-effort plaintext request-head sniffer → exercised routes +
  Referer/Origin edges + X-PZ-Test attribution).
- `net/stack/engine.rs`: on every accepted overlay connection, record a
  who-talks-to-whom edge, attributing the caller via client source port →
  PID (`management/pid_lookup`) → discovered service (so `web → db` appears
  for opaque protocols like Postgres, protocol "tcp"). Unattributed callers
  are recorded as `(external)` for tcp services only; HTTP keeps the cloud
  path's referer-edge semantics. Runs on the blocking pool, never the stack
  loop. Management tunnels (portzero dashboard/API) excluded to avoid noise.
- `tls/stack.rs`: the TLS-termination path taps the decrypted client→backend
  plaintext for HTTPS routes.
- Plumbing: `OverlayConfig.observations` (daemon passes the same store the
  cloud connector uses; tests/benches leave it None).

**Verified on this machine**: DemoWeb under `pzweb91.portzero.local` +
`db.pzdemo.portzero.local` (compose Postgres). `observed_edges` now shows
`pzweb91.portzero.local -> db.pzdemo.portzero.local [tcp]` and
`exercised_routes` shows GET/POST `/api/guestbook`, `/healthz` etc. with
X-PZ-Test attribution, over both `portzero inspect` and `portzero mcp`
tools/call.

**Known limitations (left open)**:
- PID attribution races: a connection made before its owning process is
  discovered (e.g. an app's startup DB pool connect right after `dotnet run`,
  or right after a daemon restart) records as `(external)`; the next fresh
  connection after discovery attributes correctly. Refused-backend retries
  also often miss attribution (client socket gone before the /proc scan).
  `RUST_LOG=portzero_daemon::net::stack::engine=debug` logs each decision.
- Secondary staleness issues NOT fixed here (pre-existing, reproduced during
  verification): after killing/restarting the demo web process the discovery
  loop went ~6 minutes without re-registering it (stale dead-pid service +
  stale backend addr, `Scanning every 2s` notwithstanding), correlating with
  auto-open events at 17:20/17:26 in the log; browser open itself is spawn-
  detached so the block is elsewhere in the loop iteration. Deserves its own
  ticket.
- HttpTap only parses request heads that start at a chunk boundary
  (overwhelmingly the common case); heads split mid-token are missed and
  bodies starting a chunk with a method token could over-count. Best-effort
  telemetry by design.
