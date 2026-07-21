---
id: deadae2f-b8d3-4b14-b4c4-6cffde7bc2a1
slug: task-41
status: done
title: Deduplicate duplicate overlay dashboard entries for the same local process
created_at: 2026-06-29T13:25:17.096099510Z
updated_at: 2026-06-29T13:25:17.096099510Z
---

The `portzero.local` dashboard can show multiple local-service rows for the same
effective backend, including duplicate `5173` entries for a single dev server.

Expected outcome:
- Exact duplicate overlay discoveries for the same effective process context are
  collapsed before they reach `overlay.json` and the dashboard.
- Distinct worktrees or other genuinely different claimants still remain visible
  so duplicate-name diagnostics continue to work.
