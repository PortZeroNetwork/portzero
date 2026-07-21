---
id: 93ec304d-cb60-42f5-9e1c-439e217ea1f5
slug: task-76
status: done
title: Add portzero review command and cloud feedback MCP tools
created_at: 2026-07-11T15:10:52.440267Z
updated_at: 2026-07-11T15:10:52.440267Z
---

Client side of the Review Records feature on portzero.cloud:

- `portzero review [--base <ref>] [--domain <domain>] [--project <name>] [--open]`
  uploads the branch's commits + diff as a review record (POST /review-records/),
  reporting any feedback threads advanced by `Fixes PZ-<n>` commit messages.
- MCP tools `list_feedback` and `propose_fix` let AI coding agents read
  reviewer feedback threads and propose fixes; the MCP server now passes
  `tools/call` arguments through to tools.
- Docs: docs/review-records.md, docs/mcp.md updates.
