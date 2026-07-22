---
id: e682fa8a-98f1-40c5-a952-edbb8f069e21
slug: task-82
status: done
title: 'portzero login --github-repo: autonomous cloud auth by proving push access'
created_at: 2026-07-22T10:26:13.141709523Z
updated_at: 2026-07-22T10:26:13.141709523Z
---

Implement portzero login --github-repo --team <slug> [--repo owner/repo] in the CLI: call POST /auth/github-repo/start, push a proof ref (refs/portzero/auth/<nonce>, branch fallback refs/heads/portzero/auth-<nonce>), call POST /auth/github-repo/exchange, save AuthConfig, restart the daemon when running, best-effort ref cleanup. Non-interactive, agent-sandbox safe. Unit tests for remote-URL parsing and types; mocked-API happy-path integration test.