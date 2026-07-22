---
id: cef35c66-b7bb-4385-b870-93436a26d0b1
slug: task-91
status: todo
title: 'daemon: local-tunnel traffic produces no observed_edges/exercised_routes (scoped-resolver bypass?)'
created_at: 2026-07-22T16:49:40.235498961Z
updated_at: 2026-07-22T16:49:40.235498961Z
---

Observed on Linux (systemd-resolved scoped resolver) while building the full-example MCP walkthrough (task-90):

- DemoWeb queried Postgres via db.pzdemo.portzero.local on every request, and curl hit the app by its .portzero.local tunnel name, yet `observed_edges` and `exercised_routes` recorded nothing for either — only cloud-edge traffic appears. The likely cause is the scoped resolver answering with the real address so bytes bypass the daemon's proxy and are never observed. If so, the advertised who-talks-to-whom dependency graph is empty for purely local setups; the MCP server's own instructions ('only traffic addressed via tunnel names is observed') imply named local traffic IS observed.
- The walkthrough's `mcp-db-edge` step asserts the web→db edge and will keep failing until this is fixed or the observability contract is re-worded.

Separate smaller staleness issues seen the same session: `list_services` kept reporting a dead pid's cloud service, and a removed container lingered in a tunnel-name conflict until daemon restart.