---
id: f0e28c12-91e0-49ae-8bc5-d41811df8680
slug: task-53
status: todo
title: Add cloud plan awareness and upsell diagnostics for portzero.cloud tunnels
created_at: 2026-07-02T18:06:00.645596Z
updated_at: 2026-07-02T18:06:00.645596Z
---

## Goal
Surface the user's cloud plan (from edge Welcome) and any status/plan-limit messages so that:
- `portzero status` shows Plan and friendly messages with upgrade link
- The portzero.local dashboard renders plan + message banners
- A diagnostic warning appears when attempting cloud tunnels on free plan

## Changes
- `CloudConnector`: store `plan` and `status_message` (protected by Mutex) populated from `ServerMessage::Welcome { plan, .. }` and related paths.
- Extended `write_cloud_state` / `read_*` helpers to persist and read plan + message alongside connected/error.
- `discovery_loop`: snapshot live plan/message into cloud_state.json periodically (while connected) and on connect/reconnect events. This keeps `portzero status` and web UI up to date without reconnect.
- CLI `status()`: print `Plan: ...`, any message + "Upgrade: https://app.portzero.cloud", plus a gentle upsell note for free plan.
- Management handlers: expose `cloud_plan` and `cloud_message` in `/status.json`; update embedded dashboard HTML to render them with styling + link.
- Diagnostics: new `check_cloud_plan()` that emits a Warning diagnostic (category "auth") when logged-in user has cloud routes but plan is free/unknown. Integrated into `run_diagnostics`.

## Notes
- Local-only `.portzero.local` remains free/unrestricted.
- Messages from edge (e.g. plan limit explanations) are passed through verbatim.
- No server-side or proto changes; all client-side consumption of existing Welcome payload.

## Acceptance criteria
- [ ] `portzero status` shows plan when available
- [ ] Dashboard shows plan tag and message banner with upgrade CTA when relevant
- [ ] Diagnostics report includes cloud plan warning for free users with .portzero.cloud routes
- [ ] No behavior change for paying users or local-only usage