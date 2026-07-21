---
id: 4f2d3fd3-96bb-45de-8b14-30f867aab123
slug: task-6
status: done
title: Implement legacy port monitoring, process attribution (cwd, remote port), and migration helpers
relations:
  contains:
  - milestone-1
created_at: 2026-06-21T11:12:11.235958Z
updated_at: 2026-06-21T11:12:11.235958Z
---

## Description
Fuzzy UX to help migrate and catch errors:

- Periodically scan for processes listening on common ports (or service_ports of known overlays)
- When detected, use process cwd to determine worktree context; compare to registered service
- For connections: inspect source port (RemotePort), lookup owning process, check its cwd/git context
- Show tray / log / status messages with helpful guidance
- Detect docker container start failures due to port bind conflicts
- Optionally auto-suggest or watch for direct localhost access

Also watch for bind attempts on ports we're "managing".

## Acceptance Criteria
- [ ] Detects and reports legacy listeners with context
- [ ] Attributes connections via port to processes
- [ ] Helpful messages for Docker conflicts
- [ ] Works cross-platform (best effort)
