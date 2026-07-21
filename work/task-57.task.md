---
id: 6dfa743b-ad6e-49f8-9087-3b2329717f06
slug: task-57
status: todo
title: Strip cloud team commands from the CLI
created_at: 2026-07-03T19:11:56.317425Z
updated_at: 2026-07-03T19:11:56.317425Z
priority: high
preferred-model: sonnet
---

## Context

Product decision (Nate, 2026-07-03): portzero-local should carry minimal portzero.cloud features — just enough to run cloud tunnels plus niceties like upgrade prompts. Team management belongs in the app.portzero.cloud UI. The team commands are also already broken against the current cloud API: GET /teams now returns a bare array (client expects {teams:[...]} with a role field) and GET /teams/{id}/members no longer exists, so 'portzero team list/invite/members' all fail at parse or 404.

## Approach

Remove client/crates/cli/src/team.rs and the 'team' subcommand from main.rs; point users to https://app.portzero.cloud for team management (a one-line stub message or clap error is fine). Keep login/whoami, the tunnel flow, plan display, and upgrade prompts. Drop any now-unused API client surface.