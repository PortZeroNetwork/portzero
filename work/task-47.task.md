---
id: 6d9fa43b-c8b1-4c19-85db-f5e2751e6126
slug: task-47
status: done
title: Delete hand-written SDKs
milestones:
- e0776fa1-3fc9-4807-bcd7-12e412360163
relations:
  contains:
  - e0776fa1-3fc9-4807-bcd7-12e412360163
created_at: 2026-07-01T23:40:04.811786048Z
updated_at: 2026-07-01T23:46:29.943859447Z
---

Remove the hand-written, independently-maintained SDKs entirely:
`sdks/node/index.js` (+ `index.d.ts`), `sdks/python/port_zero.py`,
`sdks/go/port_zero.go` — along with their `package.json`/`go.mod`,
`README.md`, and `examples/` directories.

- Delete `sdks/node`, `sdks/python`, `sdks/go` in full.
- Update `sdks/README.md` (top-level) to drop references to the removed
  SDKs, or remove it if nothing remains under `sdks/`.
- Check for any references to these SDKs elsewhere (root `README.md`,
  `docs/`, CI workflows, `sdks/direnv`) and remove/update them.
- Close/update [task-14](../work/task-14.task.md) (additional language SDKs) to reflect that
  hand-written SDKs are no longer the direction — future SDK work (if any)
  is a separate decision, not scoped here.

No generation, no replacement — this ticket is deletion only.
