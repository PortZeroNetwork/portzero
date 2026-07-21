---
id: fd0d4b49-2aae-46eb-9145-a58d738f5763
slug: task-52
status: done
title: Support editing HTTPS policy (and portzero config) from the portzero.local dashboard
created_at: 2026-07-02T12:28:51.597792141Z
updated_at: 2026-07-02T12:29:29.675171Z
---

## Goal

Allow users to control the overlay HTTPS policy (and eventually other portzero config) directly from the `portzero.local` dashboard UI, without hand-editing `~/.portzero/config.toml`.

This change starts with HTTPS support toggles.

## Changes

- `DaemonConfig::write_https_policy` — persist (partial) updates to `[overlay.https]` while preserving other config keys via `toml::Value` merge.
- `/status.json` now surfaces the effective `https_policy` (loaded from disk).
- New `PUT /v1/config/https` management API endpoint accepting partial `HttpsPolicyUpdate`.
- Dashboard UI: new "HTTPS settings" section with three checkboxes (enable_for_port_80, redirect_port_80, passthrough_port_443). Changes POST/PUT immediately and refresh.
- Added unit test for the write roundtrip (using temp dir).
- UI includes hint that daemon restart is required for the running virtual stack to pick up the policy.

## Notes

- The policy is still snapshotted at overlay startup (`OverlayNetwork::start` / `VirtualStack`). Live reload of the stack for policy changes is future work.
- `enable_for_port_80` is the main "turn HTTPS on/off" control for `.portzero.local` services.
- Only the daemon crate is affected; no OpenAPI doc update needed (endpoint is dashboard-only like `/status.json`).

## Acceptance criteria (for this increment)

- [x] Dashboard shows current HTTPS policy values.
- [x] Toggles write to config.toml.
- [x] Tests + checks pass.
