---
id: bea44529-7acf-4375-87ae-b36173cc37ab
slug: task-63
status: done
title: Expose daemon runtime truth via MCP server, plus human-friendly portzero inspect
relations:
  contains:
  - d05fe04d-bdd7-4eae-9b55-f487c713ad58
depends_on:
- e1861148-9c64-450a-a6d0-95803340a22c
created_at: 2026-07-07T22:50:20.172219Z
updated_at: 2026-07-07T23:07:20.037828Z
preferred-model: opus
---

Expose the daemon's observed runtime truth two ways:

1. **MCP server** (for AI coding agents): tools/resources exposing everything
   the daemon knows —
   - Discovered processes/Docker containers with PZ_TUNNEL, their images and listening ports
   - Tunnel domains (local and cloud) and PZ_HEALTH_PATH values
   - Observed connections between tunneled endpoints: the userspace proxy already
     carries all traffic addressed to tunnel names, so it can record who-talks-to-whom
     edges (protocol, last seen, request counts) with no extra instrumentation
   - Exercised HTTP routes per tunnel (path + method + count), useful as a smoke-test
     inventory when graduating to a PaaS
2. **`portzero inspect`** (for humans): the same information formatted for
   human eyes — readable text, not JSON.

The PaaS-agnostic extraction skill (separate ticket) consumes the MCP tools.

Caveat to document: only traffic addressed via tunnel names is observed;
container-to-container traffic over compose-internal DNS bypasses the daemon.

## Acceptance Criteria

- [x] MCP server ships with the daemon and is documented (how agents connect, tool/resource list)
- [x] MCP exposes services, ports, tunnel domains, health paths, observed edges, exercised routes
- [x] `portzero inspect` renders the same data as human-friendly text (no JSON output mode)
- [x] Observability caveat documented
