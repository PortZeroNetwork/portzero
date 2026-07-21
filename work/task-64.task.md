---
id: 0e9ad524-482e-4461-aa57-59fc81cb80dc
slug: task-64
status: done
title: Resolve {user}, {pr}, {run-id} tokens in PZ_TUNNEL templates
relations:
  contains:
  - d05fe04d-bdd7-4eae-9b55-f487c713ad58
created_at: 2026-07-07T22:50:22.015611Z
updated_at: 2026-07-07T23:07:27.240712Z
preferred-model: sonnet
---

`{branch}` already resolves in PZ_TUNNEL templates. Add:

- `{user}` — local username or portzero account name
- `{pr}` — pull request number (from Actions env when present)
- `{run-id}` — GitHub Actions run id (from GITHUB_RUN_ID)

Convention for the shared domain: names stay single-label; hierarchy is
expressed with `--` inside the label (e.g. `{branch}--myapp.team.tunnel.portzero.cloud`).
`_` is not usable (invalid in hostnames / banned in cert SANs). Segments must
not contain internal `--` so templates stay unambiguous. Dots/nesting are for
wildcard custom domains only (cloud-side ticket).

Server-side validation of resolved names against team naming policies is the
cloud counterpart (portzero-cloud backlog task-028).

## Acceptance Criteria

- [x] Tokens resolve for both process and container discovery
- [x] Unresolvable token (e.g. {pr} outside a PR) fails discovery for that tunnel with a clear diagnostic, not a garbled name
- [x] Segment validation rejects internal `--`
- [x] Docs updated with the token table and the `--` convention
