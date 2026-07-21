---
id: e50de8d5-951b-4a53-8327-13eaae143de3
slug: task-78
status: done
title: Onboarding funnel UX — install docs, first-run CLI, release surfaces
created_at: 2026-07-16T00:00:00Z
updated_at: 2026-07-16T00:00:00Z
---

## Problem

A full-funnel review (social link → portzero.net → download → install → first
run → real dev use) found the following defects and gaps in this repo:

1. `README.md` and the stable release notes in `release.yml` tell users to run
   `brew trust portzeronetwork/portzero` — not a real Homebrew command; the
   quickstart errors out.
2. README advertises "Windows (winget)" but winget publishing is disabled
   (`if: false` in `release.yml`); the real path is the MSI.
3. macOS builds are never codesigned/notarized; the autostarting tray/app hit
   Gatekeeper with no documented workaround anywhere.
4. Bare `portzero` prints a raw clap usage error instead of a state-aware
   front door.
5. `portzero start` can report "Daemon started" while the overlay is inactive
   (documented in `doctor.rs` as the #1 first-run failure) — the happy path
   never tells the user `doctor` exists, and first-tunnel success is silent.
6. There is no one-command magic moment: examples require the desktop app or
   hand-cloning `portzero-examples`; no `portzero demo`.
7. `update.rs` doc comment names `DEVENV_NO_UPDATE_CHECK` but the code reads
   `PZ_TUNNEL_NO_UPDATE_CHECK`.
8. `docs/users/examples.md` and the desktop app's example runner never
   cross-reference each other.

## Plan

- Fix all documentation/release-notes defects; standardize install commands
  on `curl -fsSL https://portzero.net/install.sh | sh` (Linux) and
  `brew tap PortZeroNetwork/portzero && brew install portzero` (macOS).
- Add gated macOS codesign/notarize steps (skip cleanly when secrets unset)
  and document the Gatekeeper workaround until signing ships.
- Add `portzero demo`: embedded hello server, self-tagged with
  `PZ_TUNNEL=hello.portzero.local:80`, waits for health, prints/opens the URL,
  runs doctor checks inline on failure.
- Make `portzero start` self-verify (key doctor checks) and point fresh
  installs at `portzero demo`; make bare `portzero` a friendly status surface.
