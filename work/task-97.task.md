---
id: 26d0e737-a2f0-4f8a-b63d-43be34b0749e
slug: task-97
status: todo
title: 'Docs: privacy notice covers process-environment scanning; shared-machine page'
created_at: 2026-07-28T00:00:00Z
updated_at: 2026-07-28T00:00:00Z
---

## Context

From [[decision-2]] (multi-user strategy, Option 1). The privacy notice
(`docs/users/privacy.md` and its canonical twin at portzero.net/docs,
sourced from portzero-cloud's `cloud/landing/blog/src/data/docs/`) does not
mention that the daemon reads process environments to discover `PZ_TUNNEL`,
nor how that is scoped on each platform. Nothing published explains the
single-owner-per-machine model or what other accounts on the machine can and
cannot do.

## Approach

Two documentation changes, kept in sync across both repos:

1. **Privacy notice**: state that the daemon scans process environments for
   `PZ_TUNNEL` (and `PWD` for context), that scanning is scoped to the
   owner's processes ([[task-94]]), that the data never leaves the machine
   for Local tunnels, and what a Cloud tunnel value causes to be
   transmitted.
2. **Shared-machine page** (portzero.net/docs): Port Zero is
   single-owner-per-machine on macOS/Windows; other accounts can resolve
   and consume the owner's `*.portzero.local` tunnels but are never scanned
   or published; how a non-owner account gets Port Zero removed (ask the
   owner/admin — [[task-96]]'s error text should link here); Linux runs
   per-user with the TUN/resolver held by whichever daemon starts first.

Ordering: land after [[task-94]] and [[task-96]] so the docs describe
shipped behavior, not intent.

Done when:

- [ ] `docs/users/privacy.md` updated (scanning, scoping, storage paths
      from [[task-95]])
- [ ] portzero-cloud docs twin updated in the same change window
- [ ] Shared-machine page published and linked from the [[task-96]] error
      message
