---
id: f1ed2080-9317-4764-a596-c76156310977
slug: task-92
status: todo
title: 'daemon: discovery loop stalls minutes before re-registering a restarted process (stale dead-pid service)'
created_at: 2026-07-22T17:39:53.288166870Z
updated_at: 2026-07-22T17:39:53.288166870Z
---

Reproduced during task-91 verification (2026-07-22): after killing and restarting the demo web process, the discovery loop went ~6 minutes without re-registering it despite logging 'Scanning every 2s'; the dead pid's service and stale backend lingered meanwhile. Also seen earlier the same day as a removed container lingering in a tunnel-name conflict until daemon restart. Browser auto-open is spawn-detached, so the stall is elsewhere in the loop iteration.

Repro: full-example repo, start DemoWeb with PZ_TUNNEL=<name>.portzero.local, kill it, restart it, watch `portzero inspect` / MCP list_services.

Origin: task-91 findings (left open there).