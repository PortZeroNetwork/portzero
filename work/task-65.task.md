---
id: 2c84fed9-04a6-497c-96c8-659ce2a5dd97
slug: task-65
status: done
title: Publish a GitHub Action that installs the daemon and opens tunnels in a job
relations:
  contains:
  - d05fe04d-bdd7-4eae-9b55-f487c713ad58
created_at: 2026-07-07T22:54:37.736940Z
updated_at: 2026-07-07T23:07:31.829803Z
preferred-model: sonnet
---

A published action (e.g. `portzero/tunnel-action`) so CI integration is one
YAML block: installs the daemon on the runner, brings up tunnels for the job's
compose project / processes, outputs the URL(s) as step outputs, tears down on
job end.

Notes:
- Local tunnels work inside a runner: it is a single machine, which is exactly
  the daemon's scope (TUN needs sudo — available on ubuntu hosted runners;
  container-based jobs are out of scope for v1).
- Cloud tunnels use the OIDC credential exchange (cloud-side backlog task) so
  no long-lived secrets appear in workflows.

## Acceptance Criteria

- [ ] Action published and usable from a public repo
- [x] Outputs tunnel URL(s); works with `portzero wait` for readiness
- [x] Example workflow in portzero-examples covering an integration test against a real HTTPS endpoint
- [x] Documented limitation: container-based jobs unsupported in v1

## Notes (implementation)

- Action: `tunnel-action/action.yml` (composite action) + `tunnel-action/scripts/{start,teardown}.sh`,
  documented in `tunnel-action/README.md`. Ships from this repo (`PortZeroNetwork/portzero-local`)
  at path `tunnel-action/`, not yet a standalone `portzero/tunnel-action` repo.
- Installs via the existing public `linux-install.sh` release asset (same one `README.md`'s Install
  section already points at), so it stays in sync with the daemon's own install/trust/capability
  logic instead of duplicating it.
- Outputs: `urls` (newline-separated, order-matched to the `tunnels` input) and `url` (singular
  convenience output when exactly one tunnel is requested), both populated via `portzero wait` +
  `portzero url` (assumed to exist per task-61/task-62 — not implemented by this ticket).
- Teardown is a second, explicit step (`mode: teardown`, guarded by `if: always()`) rather than an
  automatic composite-action `post:` hook — GitHub Actions composite actions do not support `post:`
  (only JS/Docker actions do). Documented in the README under "Teardown: why it's a second, explicit
  step".
- Example workflow: `portzero-examples/.github/workflows/tunnel-action-integration-test.yml` — uses
  the existing `nodejs-typescript/process` example with `PZ_TUNNEL=...portzero.local:443` (automatic
  HTTPS termination at the tunnel edge) and asserts against the real `https://` URL.
- **Left undone**: "Action published and usable from a public repo" — this requires merging
  `milestone-4-aux` to `develop` (or later mirroring into a dedicated `portzero/tunnel-action` repo)
  and is out of this change's reach; both this repo's own `README.md#usage` and the
  `portzero-examples` workflow's header comment note the ref will resolve once that merge happens.
  No CI run of the example workflow was performed (its `uses:` target isn't merged yet).
