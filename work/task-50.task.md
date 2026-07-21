---
id: c8e8362e-fc85-4dbb-9185-52c34af7a25d
slug: task-50
status: done
title: Add cyclomatic complexity and max file size budgets (lefthook + CI)
relations:
  contains:
  - e0776fa1-3fc9-4807-bcd7-12e412360163
depends_on:
- d8f896b1-7f1d-4bfc-8817-a0f0d60246bd
created_at: 2026-07-01T23:40:04.867542086Z
updated_at: 2026-07-01T23:40:04.867542086Z
---

Nothing currently enforces file-size or complexity limits, so files like
the pre-split `discovery.rs` (3.2k lines) can grow unnoticed until
someone has to touch them.

- Pick tooling: cargo-geiger doesn't fit; use `cargo clippy`'s
  `cognitive_complexity`-style lints aren't built in for stable Rust, so
  evaluate `cargo-complexity` or a custom line-count/AST check; at
  minimum add a max-file-size check (e.g. a small script/just recipe
  that greps `find client -name '*.rs' | xargs wc -l` against a
  threshold, informed by the current max after split-discovery).
- Add a `just complexity` (or `just lint-budgets`) recipe.
- Wire it into `lefthook.yml` under `pre-commit` (fast, so keep it cheap
  — file-size check is O(files), complexity check should be scoped to
  changed files only if it's slow).
- Wire the same check into `.github/workflows/ci.yml` as a required job
  so it can't be bypassed with `--no-verify`.
- Document the thresholds and rationale in `docs/` (or inline in the
  script) so future contributors know why a PR failed.
