---
id: d8f896b1-7f1d-4bfc-8817-a0f0d60246bd
slug: task-49
status: done
title: Split discovery.rs into process and Docker discovery modules
relations:
  contains:
  - e0776fa1-3fc9-4807-bcd7-12e412360163
created_at: 2026-07-01T23:40:04.851914710Z
updated_at: 2026-07-01T23:40:04.851914710Z
---

`client/crates/daemon/src/discovery.rs` is 3.2k lines mixing process
scanning, Docker container scanning, and domain/tunnel validation in one
file — the largest file in the codebase and the top candidate for merge
conflicts as it grows further.

- Split into `discovery/process.rs` (process env/port scanning across
  Linux/macOS/Windows) and `discovery/docker.rs` (Docker container
  scanning), keeping `discovery/mod.rs` (or `discovery.rs`) as a thin
  entry point re-exporting the public API and holding shared types
  (e.g. PZ_TUNNEL parsing/validation) if they don't cleanly belong to
  either half.
- No behavior change — this is a pure module split. Existing tests
  (including `client/crates/daemon/tests/overlay_e2e.rs`) must keep
  passing unmodified.
- Update any `mod discovery;` / `use crate::discovery::...` references
  elsewhere in the daemon crate.
