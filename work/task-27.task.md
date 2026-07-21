---
id: 094ba6d7-7f78-47b0-8565-84fbce2c3f76
slug: task-27
status: done
title: Add macOS to the CI test matrix (currently ubuntu-only)
milestones:
- milestone-2
created_at: 2026-06-23T12:52:00.429639Z
updated_at: 2026-06-23T12:52:00.429639Z
---

## Context

CI (`.github/workflows`) runs `cargo check` / `clippy` / `cargo test --workspace`
only on `ubuntu-latest`. The release workflow *builds* macOS and Windows
artifacts but runs **no tests** there. So every macOS-specific path landed in
this milestone is currently unguarded — a regression in the utun/resolver/lsof
branches wouldn't be caught.

## Approach

- Extend the Check & Test job to a matrix including `macos-latest` (and
  `windows-latest` once [[[task-28](../work/task-28.task.md)]] is underway).
- Run the unprivileged suite everywhere — the root-gated real-TUN test already
  skips cleanly when `geteuid() != 0`, so it's CI-safe.
- Make sure `clippy -D warnings` passes per-platform (platform `#[cfg]` blocks
  often hide unused-import / dead-code warnings — cf. [task-20](../work/task-20.task.md)). The current
  build already emits an unused `Path` import warning in `autostart.rs` and a
  dead `powershell_escape` on macOS; fix those so the matrix goes green.

Done when:

- [ ] CI test job runs on macOS (matrix) in addition to Linux
- [ ] `cargo test --workspace` and `clippy -D warnings` pass on macOS in CI
- [ ] Existing per-platform warnings cleared so the matrix is green
- [ ] Windows left as a follow-on hook tied to [[[task-28](../work/task-28.task.md)]]