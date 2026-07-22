---
id: 4ad32aca-81d2-4401-9bac-27d4a0b2c659
slug: task-85
status: done
title: 'agents setup: repo-level provisioning for cloud AI dev environments'
created_at: 2026-07-22T10:38:15.453155551Z
updated_at: 2026-07-22T10:38:15.453155551Z
---

Extend `portzero agents setup` so a run inside a git repo also provisions that repo for cloud AI dev environments (Claude Code on the web and similar sandboxes):

1. `.claude/settings.json` — SessionStart hook that, only in a headless cloud sandbox (root + no systemd + portzero missing), fetches and runs `scripts/cloud-agent-env-install.sh` from the staging branch raw URL. JSON merged read-modify-write; portzero-managed hook replaced in place.
2. `AGENTS.md` — managed instructions block (markers) covering PZ_TUNNEL/port-0 exposure, wait/url/inspect consumption, NO_PROXY curls, and cloud auth (`portzero login` and `portzero login --github-repo --team <slug>`). Mirrored into CLAUDE.md only when the repo has one that does not @-include AGENTS.md.
3. `.mcp.json` — merge a `portzero` stdio MCP server entry (`portzero mcp`).

Flags: `--repo-only` / `--machine-only` to restrict scope; default auto-detects (repo provisioning when CWD is inside a git repo, machine-level always otherwise).

Idempotent between managed markers; second run is a no-op. Tests cover idempotency, JSON merge preserving foreign keys, and marker/hook replacement.
_Note: commits for this work carry the slug `task-84` — reassigned to task-85 after a parallel-worktree collision (task-84 = wait TLS trust fix)._
