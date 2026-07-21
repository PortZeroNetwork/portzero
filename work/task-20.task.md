---
id: 8e41a52d-0248-40e3-b1b5-686242c60281
slug: task-20
status: done
title: 'Fix CI: clear clippy -D warnings failures (workspace)'
milestones:
- milestone-1
created_at: 2026-06-22T17:05:38.745921064Z
updated_at: 2026-06-22T17:05:38.745921064Z
---

## Problem

`ci.yml` runs `cargo clippy --workspace -- -D warnings`, which has been failing
on every push (the `check` job is red). Releases are unaffected (`release.yml` is
a separate workflow that only builds), but CI never goes green.

Clippy `-D warnings` failures in the daemon lib:
- `legacy_monitor.rs:162` — `clippy::unnecessary_sort_by` (`sort_by` with a
  key-comparison closure that should be `sort_by_key`).
- `net/overlay.rs:31-32` — `clippy::doc_lazy_continuation` (a numbered list in the
  `dns_listen` doc comment, added in task-15).
- `net/resolver_config.rs:98` — `clippy::useless_format` (`format!` of a literal
  with no interpolation).

## Fix

- `legacy_monitor.rs`: `issues.sort_by(|a,b| key(a).cmp(&key(b)))` →
  `issues.sort_by_key(issue_sort_key)`.
- `net/overlay.rs`: rewrote the `dns_listen` doc comment as prose (no markdown
  list) — same content.
- `net/resolver_config.rs`: `format!("…literal…")` → `String::from("…literal…")`.

No behavior change. The pre-existing `unused variable: ip/ip2` warnings in
`virtual_ip.rs` are in TEST code only, so they don't affect the CI clippy step
(which lints lib+bins, not tests) — left untouched.

## Acceptance Criteria

- [x] `cargo check --workspace` clean.
- [x] `cargo clippy --workspace -- -D warnings` passes (exit 0).
- [x] `cargo test --workspace` fully green.
- [ ] CI `check` job goes green on the next push (verify via `gh`).