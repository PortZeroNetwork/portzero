---
id: e1861148-9c64-450a-a6d0-95803340a22c
slug: task-60
status: done
title: Discover PZ_HEALTH_PATH on tunneled processes and containers
relations:
  contains:
  - d05fe04d-bdd7-4eae-9b55-f487c713ad58
created_at: 2026-07-07T22:46:11.439344Z
updated_at: 2026-07-07T23:03:55.439934Z
preferred-model: sonnet
---

Add an optional `PZ_HEALTH_PATH` env var, discovered alongside `PZ_TUNNEL` on
processes and Docker containers. It declares the HTTP path (e.g. `/health`)
that indicates the tunneled endpoint is ready.

Uses:
- `portzero wait <domain> --healthy` polls it (separate ticket).
- Surfaced in `portzero status` and `portzero inspect --json`.
- Carried into production configs by AI agent skills at graduation time
  (the value survives the move to a PaaS; the PZ_* var itself does not).

## Acceptance Criteria

- [x] Discovery reads `PZ_HEALTH_PATH` from process and container env, stored on the route/tunnel record
- [x] Absent var changes nothing (fully optional)
- [x] Shown in `portzero status` output for tunnels that declare it
- [x] Documented in portzero-local/docs alongside PZ_TUNNEL / PZ_TUNNEL_HTTP_PORT
