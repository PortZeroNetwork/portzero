---
id: e77844ff-b30b-46c5-a91d-99cb31c2306c
slug: task-62
status: done
title: Add portzero url and portzero env for exporting tunnel URLs to test configs
relations:
  contains:
  - d05fe04d-bdd7-4eae-9b55-f487c713ad58
created_at: 2026-07-07T22:48:01.321648Z
updated_at: 2026-07-07T23:06:39.025360Z
preferred-model: haiku
---

Plumbing so a test process can learn a tunnel's URL:

- `portzero url MYTUNNEL.portzero.local` prints the resolved URL (scheme, host, port).
- `portzero env` prints export lines for all discovered tunnels;
  `portzero env --github` appends to `$GITHUB_ENV` in Actions.

This is the escape hatch for non-Playwright consumers; the Playwright fixture
package talks to the daemon directly and is the preferred integration.

## Acceptance Criteria

- [x] `portzero url` prints exactly the URL on stdout (script-safe)
- [x] `portzero env --github` works in a GitHub Actions job
- [x] Errors clearly when the tunnel does not exist
