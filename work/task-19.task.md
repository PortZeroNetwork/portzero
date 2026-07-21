---
id: 0bedcc99-e4d4-4ac8-88a0-59b8480fe4d9
slug: task-19
status: done
title: Repoint client update check to GitHub Releases (post server/client split)
milestones:
- milestone-1
created_at: 2026-06-22T16:58:21.781005651Z
updated_at: 2026-06-22T16:58:21.781005651Z
---

## Problem

After splitting the client (this repo) from the server (`port-zero`), the
client's update/version channel was left pointing at the server's release CDN.

- `releases.portzero.cloud` is a CDN over the S3 bucket `port-zero-releases`,
  populated by the OLD `port-zero` repo's `release.yml`
  (`aws s3 cp artifacts/ s3://port-zero-releases/releases/latest/ … version.json`).
- That repo now builds the SERVER bins (`port-zero-api`, `port-zero-edge`),
  so the live `releases.portzero.cloud/releases/latest/version.json` reports the
  server's train (`{"version":"0.3.8","commit":"198f7d3…"}` — a commit not in
  this repo).
- This client repo's `release.yml` runs green on every push and publishes to
  GitHub Releases (`v0.0.x`), but nothing here writes to that S3 bucket.

Result: `devenv update` and the background update check read the server's
manifest, so the client never sees its own releases.

## Decision

Option #2: point the client's update check at this repo's GitHub Releases
instead of the dead CDN. Keeps client + server release infra fully decoupled and
needs no AWS credentials in this repo.

## Changes (this repo)

- `client/crates/cli/src/update.rs` and `client/crates/cli/src/bin/devenv.rs`:
  `RELEASES_BASE_URL` → `https://github.com/LoumTechnologies/port-zero/releases`,
  and the version-manifest URL → `…/releases/latest/download/version.json`
  (GitHub serves the newest stable release's `version.json` asset, which
  `release.yml` already uploads). Dropped the now-unused staging/prerelease
  branch (GitHub exposes no static "latest prerelease" URL and this repo has no
  staging channel).

## Out of scope (must be done separately — not in this repo)

`install.sh` (served from `https://portzero.net/install.sh`) is what actually
downloads + installs the binaries; `devenv update` just re-runs it. It still
targets `releases.portzero.cloud`. It must be updated to fetch the client tarballs
from GitHub Releases (e.g. `…/releases/latest/download/port-zero-<target>.tar.gz`,
asset names per `release.yml`). Tracked here for visibility; lives on the website.

## Acceptance Criteria

- [ ] The update check + `devenv update` version check hit GitHub Releases and
      report this repo's latest `v0.0.x` (not the server's `0.3.x`).
- [ ] No new clippy warnings introduced (CI is separately red on pre-existing
      lints — see follow-up).
- [ ] `install.sh` updated on the website to pull client tarballs from GitHub
      Releases (out-of-repo; checkbox tracked here).
