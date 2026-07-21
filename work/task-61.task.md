---
id: fc757857-e263-4eec-a737-58fa44079c55
slug: task-61
status: done
title: Add portzero wait <tunnel-domain> --healthy for CI and test readiness gates
relations:
  contains:
  - d05fe04d-bdd7-4eae-9b55-f487c713ad58
depends_on:
- e1861148-9c64-450a-a6d0-95803340a22c
created_at: 2026-07-07T22:47:59.577469Z
updated_at: 2026-07-07T23:04:18.523104Z
preferred-model: sonnet
---

`portzero wait MYTUNNEL.portzero.local [--healthy] [--timeout <secs>]` blocks
until the tunnel exists; with `--healthy` (or when the backing endpoint declares
PZ_HEALTH_PATH) it also polls the health path until it returns 2xx.

The argument is the tunnel domain — portzero has no service concept and the
tunnel name is the only identity.

Consumers: CD smoke gates in GitHub Actions, Playwright `webServer` blocks
(`command: 'docker compose up -d && portzero wait web.myapp.portzero.local'`),
and the Playwright fixture package.

Design note: a paused tunnel (edge-only pause, cloud-side feature) must be
distinguishable from a dead one; wait should fail on dead and report paused
distinctly.

## Acceptance Criteria

- [x] Exits 0 when tunnel is up (and healthy, if requested); nonzero on timeout with a clear message
- [x] Works for Local and Cloud tunnels
- [x] `--timeout` with a sensible default
- [x] Documented with a Playwright webServer example
