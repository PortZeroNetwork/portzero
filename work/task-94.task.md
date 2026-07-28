---
id: 478f1c45-2535-4cc9-b528-514f8cdbc155
slug: task-94
status: todo
title: 'Discovery: only act on processes owned by the daemon owner''s UID'
created_at: 2026-07-28T00:00:00Z
updated_at: 2026-07-28T00:00:00Z
---

## Context

First implementation ticket from [[decision-2]] (multi-user strategy, Option
1). On macOS the daemon runs as a root LaunchDaemon and
`scan_process_env` (`discovery/process/platform.rs`, KERN_PROCARGS2) can read
**every** account's process environments; Windows will have the same reach
under the [[task-28]] Administrator model. So another account's process that
sets `PZ_TUNNEL` — e.g. from a repo's committed `.env` — gets tunneled without
consent, and a cloud value publishes under the **owner's** account. Linux
already behaves correctly for free: the unprivileged user service cannot read
other accounts' `/proc/<pid>/environ`.

## Approach

The daemon knows its owning user (the LaunchDaemon pins `HOME` via
`SUDO_USER` — see `autostart.rs::real_user_home`). Resolve that user's UID at
startup and make discovery skip any process not owned by it, before reading
its environment. Prefer filtering at enumeration time so we don't read
environments we will discard.

Done when:

- [ ] macOS: process discovery ignores processes whose owning UID differs
      from the daemon owner's (Docker discovery is unaffected — the Docker
      socket is already an explicit grant)
- [ ] Windows: same filter under the Administrator service model
- [ ] Linux: no behavior change (assert/document that the unprivileged
      service already cannot cross accounts)
- [ ] A log line at debug level counts skipped foreign-UID processes, so a
      shared-machine user can see why their process was not picked up
- [ ] Covered by unit tests with synthetic process lists
