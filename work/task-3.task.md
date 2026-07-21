---
id: e883dacd-9896-4a21-98ff-f0bf35f62418
slug: task-3
status: done
title: Wire OverlayNetwork into daemon lifecycle and start the virtual stack
relations:
  contains:
  - milestone-1
created_at: 2026-06-21T11:12:11.235958Z
updated_at: 2026-06-21T11:12:11.235958Z
---

## Description
The overlay manager exists but is not started.

- In discovery_loop or a new dedicated loop, create TUN, start OverlayNetwork (which starts stack + DNS)
- Feed updates from network service discovery (PZ_TUNNEL) to the overlay
- Handle start/stop, graceful shutdown
- Support foreground mode and daemon mode
- Coordinate with existing cloud tunnel if both active

## Acceptance Criteria
- [ ] Daemon starts the virtual stack when enabled
- [ ] Services with PZ_TUNNEL are registered in overlay
- [ ] Stack shuts down cleanly
- [ ] No breakage to existing PZ_TUNNEL flow
