---
id: 4c4e37a7-0dbd-448f-8867-112cae20d27f
slug: task-88
status: todo
title: 'Claude Code plugin: package cloud-agent hook + portzero mcp + skill for the community marketplace'
created_at: 2026-07-22T13:03:15.662501321Z
updated_at: 2026-07-22T13:03:15.662501321Z
---

Package the cloud-agent story as a Claude Code plugin: SessionStart hook running scripts/cloud-agent-env-install.sh (as 'portzero agents setup' now writes per-repo), the 'portzero mcp' stdio server, and a skill teaching PZ_TUNNEL/port-0/wait/login workflows. Submit to the community marketplace (platform.claude.com/plugins/submit). Note: MCP OAuth cannot hand credentials to the daemon and cloud sessions cannot run the OAuth flow, so the plugin is distribution/UX — auth remains portzero login (device code) / --github-repo.