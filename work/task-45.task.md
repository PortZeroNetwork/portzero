---
id: 0a0636cf-14c2-4c31-b842-f09a71f92b90
slug: task-45
status: done
title: Add lefthook + just recipes for pre-push CI checks (local clippy + tests)
depends_on:
- 8e41a52d-0248-40e3-b1b5-686242c60281
- 094ba6d7-7f78-47b0-8565-84fbce2c3f76
created_at: 2026-07-01T13:22:44.139043082Z
updated_at: 2026-07-01T13:22:44.139043082Z
---

## Summary

Added support for automatic local CI smoke tests using lefthook (cross-platform git hooks framework) + `just` recipes.

This makes it hard to accidentally push code that will fail the main "Check & Test" GitHub Actions job (especially `cargo clippy --workspace -- -D warnings`).

### Changes
- `lefthook.yml`: defines `pre-commit` (fmt) and `pre-push` (fmt + clippy -D warnings + test)
- `justfile`:
  - `just fmt-check`
  - `just clippy`
  - `just clippy-all`
  - `just check`
  - `just verify` (the full local equivalent of the CI check job)
  - `just install-hooks` (one-command setup for Linux/macOS/Windows)
- Documentation in `docs/troubleshooting.md` and `docs/README.md`

### Usage
After clone (or on a new machine):
```
just install-hooks
ticketry init   # for ticket indexing
```

Then `git push` will run the checks that match CI.

### Privileged tests
`just e2e` (real TUN) is **not** included in hooks (intentionally). It requires sudo and is still run in the separate CI E2E job. The unprivileged `just test` already covers the skip path safely.

Depends on the clippy incident (task-20) and broader CI improvements (task-27).

