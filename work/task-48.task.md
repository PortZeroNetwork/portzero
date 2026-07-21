---
id: ecd36120-da7f-462b-9331-c3d5b59717e7
slug: task-48
status: done
title: Publish generated OpenAPI spec as a GitHub release asset
relations:
  contains:
  - e0776fa1-3fc9-4807-bcd7-12e412360163
depends_on:
- e1b214ee-2cac-469c-b7bf-db70a406b34a
created_at: 2026-07-01T23:40:04.831533901Z
updated_at: 2026-07-01T23:40:04.831533901Z
---

Attach the generated `api/management-v1.yaml` to GitHub releases so
consumers can pull the spec without cloning the repo.

- In `.github/workflows/release.yml`, add a step that regenerates the
  spec (via the `just openapi` recipe from openapi-gen) and uploads it
  as a release asset (e.g. with `softprops/action-gh-release` or
  `gh release upload`).
- Verify the asset shows up on a real release run (or a draft/dry-run
  if the release workflow supports one).
- Cross-check against [task-45](../work/task-45.task.md) (lefthook + just recipes for pre-push CI)
  and [task-44](../work/task-44.task.md) (WarpBuild runner switch) for any conflicts in
  release.yml ordering.
