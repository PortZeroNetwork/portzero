---
id: 0ecc0c87-9b8b-49e5-a6d7-e995f18c8e07
slug: task-5
status: done
title: 'Add visibility layer: system tray, notifications, errors for non-unique names and misuse'
relations:
  contains:
  - milestone-1
created_at: 2026-06-21T11:12:11.235958Z
updated_at: 2026-06-21T11:12:11.235958Z
---

## Description
When config isn't unique across worktrees, or when apps hit legacy ports directly, make errors highly visible:

- System tray icon (red/yellow for issues)
- Native notifications or message boxes on macOS/Windows/Linux
- Status command and logs reflect problems
- Detect duplicate resolved names from different worktree contexts during discovery

Use existing daemon state files + a small tray companion if needed.

## Acceptance Criteria
- [ ] Non-unique name triggers visible error
- [ ] Tray/notification on major platforms
- [ ] Clear messages explaining how to fix (use better template, etc.)
