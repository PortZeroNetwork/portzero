---
id: 83866695-aa4f-4628-9aef-a50fc7788b62
slug: task-90
status: todo
title: 'full-example: MCP walkthrough flow (just test-mcp) exercising every portzero mcp tool'
created_at: 2026-07-22T16:49:26.730480654Z
updated_at: 2026-07-22T16:49:26.730480654Z
---

The full-example repo gains a third walkthrough flow, `just test-mcp`, that spawns the real `portzero mcp` stdio server and speaks newline-delimited JSON-RPC to it the way an AI agent would: initialize handshake + tools/list (all 7 tools), list_services, list_tunnels, exercised_routes (with X-PZ-Test attribution), observed_edges (inbound + web→db dependency edge), overview, list_feedback, and propose_fix (verified out-of-band via the cloud API). Plan-agnostic: either production test account.

Related: task-63 (MCP server), task-76 (feedback MCP tools).