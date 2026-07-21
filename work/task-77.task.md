---
id: 2cbe7ddb-9fe2-427a-9d7f-1815ad389ffe
slug: task-77
status: done
title: Align CI/release workflows with the cross-product release conventions
created_at: 2026-07-14T00:00:00Z
updated_at: 2026-07-14T00:00:00Z
---

Make portzero-local and portzero-cloud consistent, copyable exemplars of the
staging → promote → tag release model:

- Split the gated stable promote out of `release.yml` into
  `.github/workflows/promote-production.yml` (**Promote to Production**), with
  the canonical `ref` + `bump` (patch/minor/major/none) dispatch inputs — the
  same workflow name, job name (`promote`), and inputs as portzero-cloud.
- Gate it on the `production` GitHub Environment (renamed from `release`;
  requires a manual Settings change — see PR).
- `release.yml`'s dispatch channel becomes `edge | stable`: `edge` stays the
  ungated unsigned prerelease; `stable` re-publishes the vX.Y.Z tag the run is
  dispatched at (the artifact-product rollback path, driven by promote's
  `bump: none`).
- Document the whole model in `docs/release-conventions.md` (kept in sync with
  the identical file in portzero-cloud), including the trigger-safe tag push
  snippet and a new-product setup checklist.

See decision-1 for the ADR.
