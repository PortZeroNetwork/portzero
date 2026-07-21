---
id: 1881d834-830b-4f3d-ad74-d559e5a5d3c9
slug: milestone-1
status: todo
title: Virtual Overlay Network v0.2
ticket_type: milestone
relations:
  contains:
  - task-1
  - task-2
  - task-3
  - task-4
  - task-5
  - task-6
  - task-7
  - task-8
  - task-9
  - task-10
  - task-11
created_at: 2026-06-21T11:11:56.399734Z
updated_at: 2026-06-21T11:12:11.235958Z
---
# Virtual Overlay Network v0.2

Complete the implementation of the Port 0 + Virtual Overlay Network so that `PZ_TUNNEL=foo` + port 0 bindings allow seamless access via `foo.devenv.local` without any port conflicts, even across multiple worktrees.

## Goals
- Full end-to-end packet flow through TUN + smoltcp
- Scoped DNS only for *.devenv.local
- Robust discovery for both native processes and Docker (with/without compose)
- Visibility and helpful errors for misconfigurations and legacy usage
- Platform support and security

See child tasks for breakdown.
