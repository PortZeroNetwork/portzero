# File-size / complexity budgets

Nothing enforced a maximum file size or complexity in this repo until
task-50. `client/crates/daemon/src/discovery.rs` grew to ~3.2k lines before
anyone noticed and split it (task-49) into `discovery.rs` +
`discovery/process.rs` + `discovery_loop.rs`. This document explains what is
enforced now, the thresholds chosen, and what is deliberately out of scope.

## What's enforced

**File-size budget.** A Rust source file under `client/` may not exceed
**1000 lines**. This is checked by `portzero-xtask`'s `check-file-size-budget`
binary (invoked via `just complexity`) and runs:

- **Locally, pre-commit** (`lefthook.yml`): scoped to staged `client/*.rs`
  files only (`just complexity --changed`), so it stays fast (a handful of
  `wc -l` calls) and matches the pattern already used by the `fmt` pre-commit
  hook.
- **In CI** (`.github/workflows/ci.yml`, `file-size-budget` job): the full
  check over every `client/*.rs` file, on every PR into `staging` and on
  every push to `staging`. This is
  a required job, so the budget can't be bypassed with `git commit
  --no-verify` / `git push --no-verify` — those only skip the local
  lefthook hooks, not CI.
- **On demand**: `just complexity` (whole tree) or `just complexity
  --changed` (staged files).

## Why 1000 lines

2500 was derived from the actual max file size in `client/` right after the
discovery.rs split, but that headroom turned out to be too generous: several
files quietly grew past 2000 lines again with nothing flagging it. 1000
lines is a tighter, more conventional ceiling — low enough that a file
creeping toward it is a real signal to split it, rather than something that
only trips once a file is already unwieldy.

The threshold lives in `client/crates/xtask/src/bin/check_file_size_budget.rs`
(`PORTZERO_FILE_SIZE_BUDGET`, default `1000`) and can be overridden via that
env var for local experimentation, but the default is what CI enforces.

## Cognitive complexity

Per-function cognitive complexity is enforced via clippy's
`clippy::cognitive_complexity` lint, at clippy's own default threshold of
**25**, set in the workspace-root `clippy.toml`
(`cognitive-complexity-threshold = 25`) and enabled with
`-D clippy::cognitive_complexity` alongside `-D warnings` in `just clippy`
/ `just clippy-all` and CI's "Clippy" step (`.github/workflows/ci.yml`).

An earlier version of this doc claimed `cognitive_complexity`-style lints
were clippy-nightly-only and deferred enforcement for that reason. That was
incorrect (or became outdated) — as of clippy 1.94 (this repo's pinned
stable toolchain), `clippy::cognitive_complexity` works with no nightly
features, and its threshold is configurable via `clippy.toml`. There was no
tooling gap; it just hadn't been re-verified. Since `just clippy` /
`just clippy-all` already run with `-D warnings` in CI and pre-push, adding
`-D clippy::cognitive_complexity` alongside them was a one-line change, not
a new enforcement mechanism.

## Files

- `client/crates/xtask/src/bin/check_file_size_budget.rs` — the file-size check.
- `clippy.toml` — cognitive-complexity threshold (25).
- `justfile` — `just complexity [--changed]` and `just clippy[-all]` recipes.
- `lefthook.yml` — `pre-commit.complexity` (changed files only) and
  `pre-push` clippy (full workspace, includes cognitive complexity).
- `.github/workflows/ci.yml` — `file-size-budget` job (full tree, required)
  and the `Clippy` step in `check` (full workspace, required).
