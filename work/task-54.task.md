---
id: 30ed07e6-5172-45b0-841f-84fb2f503a1c
slug: task-54
status: done
title: Bound Windows real-TUN E2E CI hangs
created_at: 2026-07-03T01:06:02.483133Z
updated_at: 2026-07-03T01:06:02.483133Z
---

## Context

The Windows `real_tun_overlay` CI matrix entry can hang after printing
`real_tun_overlay: bringing up real overlay`, even though `just e2e` passes in
an elevated Windows 11 PowerShell.

## Changes

- Add a wall-clock watchdog around the privileged real-TUN E2E worker so blocking Wintun setup fails quickly with the last recorded phase.
- Add manual CI dispatch inputs for job group and runner OS so the Windows E2E matrix entry can be run by itself.

## Verification

- `cargo fmt --check`
- `CARGO_TARGET_DIR=/private/tmp/portzero-target cargo test -p portzero-daemon --test overlay_e2e real_tun_overlay --no-run`

## Resolution notes

- The wall-clock watchdog already landed on `develop` (via the
  `milestone-4-aux` merge): `client/crates/daemon/tests/overlay_e2e.rs` wraps
  the privileged worker in a `TEST_TIMEOUT` (15s) watchdog, tracks
  `RealTunStep` phases (wired into `OverlayNetwork::start_with_progress`), and
  on Windows runs the worker in a killable child process that reports the last
  recorded phase via a progress file when it wedges — so a blocking Wintun
  setup fails fast with `last step: <phase>` instead of hanging.
- This ticket's remaining delta adds `workflow_dispatch` inputs (`job`,
  `os`) to `.github/workflows/ci.yml` so the Windows E2E matrix entry can be
  run by itself (job=e2e, os=windows-latest). The `pull_request` trigger still
  runs the full matrix unchanged.
