---
id: 6f7a3c21-5d84-4d2b-9a1f-2c8e0b7d4a55
slug: task-93
status: in-progress
title: 'app: show the PortZero version and verify tray/daemon/CLI agree on it'
created_at: 2026-07-24T00:00:00.000000000Z
updated_at: 2026-07-24T00:00:00.000000000Z
---

The desktop app never showed which version of PortZero the user is running, and
nothing checked that the four binaries we ship (`portzero`, the daemon it runs,
`portzero-tray`, `portzero-app`) are from the same build.

Two problems had to be fixed for that check to mean anything:

1. **Only the CLI crate was ever version-stamped.** `scripts/set-cli-version.sh`
   rewrote `client/crates/cli/Cargo.toml` alone, so a release built from
   `v1.2.3` shipped a daemon and tray hardcoded at `0.1.0` and an app at
   `0.0.1`. `/v1/daemon/status` reported `0.1.0` for every release ever cut.
2. **Nothing reported a running component's version.** The daemon exposed one
   over HTTP; the tray and app exposed none, so a stale tray left over from an
   upgrade was invisible.

Delivered:

- One workspace version (`[workspace.package] version`), inherited by every
  crate; `scripts/set-version.sh` stamps it (plus `Cargo.lock` and
  `tauri.conf.json`) at release time.
- `portzero_daemon::versions`: long-running components write
  `~/.portzero/components/<name>.json` (version + pid) at startup and remove it
  on clean exit; readers treat a dead pid as absent.
- Surfaces: a **Version** panel in the desktop app (with the version in the
  title bar), `portzero version`, a `component versions` check in
  `portzero doctor`, and the version in the tray menu.

The mismatch case the check exists for: an upgrade replaces the binaries on disk
while the old daemon/tray/app keep running the previous build.
