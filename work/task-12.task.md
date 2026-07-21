---
id: ab156a7a-9b8d-43f3-91a9-0d5a03f27c57
slug: task-12
status: todo
title: Add system tray icon companion (GUI event loop)
milestones:
- milestone-1
created_at: 2026-06-22T02:23:09.488639888Z
updated_at: 2026-06-22T02:23:09.488639888Z
---

## Description

Deferred from [task-5](../work/task-5.task.md) (visibility layer). [task-5](../work/task-5.task.md) delivered the headless,
crate-free parts of the visibility layer: duplicate `.devenv.local` name
detection, a persisted `issues.json`, actionable logs, native notifications
via shell-out (osascript / notify-send / PowerShell toast), and
`portzero status` surfacing current issues.

A persistent **system-tray icon** (red/yellow status indicator) was
intentionally deferred because it requires a GUI event-loop crate (e.g.
`tray-icon` + `winit`/`gtk`) that conflicts with the headless daemon and
would balloon scope / dependencies. Adding it likely means a small separate
companion binary that reads the daemon's `issues.json` and renders tray state.

Depends conceptually on [task-5](../work/task-5.task.md) (consumes the `issues.json` it introduced).

## Acceptance Criteria

- [ ] Tray icon shows green/yellow/red based on daemon issue state
- [ ] Reads existing `issues.json` (no new daemon coupling)
- [ ] Runs as an opt-in companion, not part of the headless daemon
