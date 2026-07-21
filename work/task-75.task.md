---
id: bd8b84c4-092e-4fa6-88a3-ceecaafa1fd2
slug: task-75
status: done
title: Bake Homebrew into the macOS test VM (metered-safe, cached in snapshot)
created_at: 2026-07-11T15:33:47.000000Z
updated_at: 2026-07-11T15:33:47.000000Z
---

## Why

The macOS Parallels test VM ([[parallels-vm-testing]]) was pristine (no Xcode CLT,
no Homebrew), so `just vm-test macos` and the CI VM-E2E job could not exercise the
real `brew install portzero` path (the Homebrew tap is a first-class install
channel).

## What

Installed Xcode CLT + **Homebrew 6.0.9** **once** into the internal macOS VM and
baked it into the `portzero-built` reset point (what vm-test/CI revert to). The
host is on a metered link, so the test path never re-downloads — it only reverts
to the snapshot.

Durability (never download again):
- `portzero-toolchain` — permanent CLT+Homebrew anchor, never auto-overwritten
  (the durable cache of the one-time download).
- `portzero-pre-brew` — pristine pre-Homebrew baseline preserved; golden untouched.
- Whole VM (all 6 snapshots) mirrored to the 4 TB drive via `just vm-sync-macos`.

Added `vmtest/scripts/macos-install-homebrew.sh` (guest, idempotent, headless),
`macos-add-brew.sh` (host orchestrator), `just vm-macos-add-brew`. Documented the
Cache-first rule and the human-owns-golden / repo-owns-everything-after boundary
in `vmtest/README.md`.

Homebrew install gotchas handled: brew refuses to run as root (prlctl exec IS
root) so it installs as the `parallels` admin user via a temporary sudoers NOPASSWD
drop-in removed on exit; `bash -c "$(curl)"` under `sudo VAR=val` mangles argv
(rc 127) so the installer is downloaded to a file and run.

## Follow-ups

- Registered `macOS 15.7.7 archive` VM (CI recovery source via resolve_effective_vm)
  is not refreshed by the sync — still pristine.
- No e2e test yet exercises `brew install portzero`.
