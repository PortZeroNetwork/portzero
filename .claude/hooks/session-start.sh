#!/bin/bash
# Warms the cargo registry + build cache so the first `cargo fmt`/`clippy`/
# `test` of a Claude Code web session (see docs/dev/development.md) isn't
# also paying for a from-scratch dependency compile. Remote-only: a local
# `claude` session already has a warm target/ from prior runs.
set -euo pipefail

if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  exit 0
fi

cd "$CLAUDE_PROJECT_DIR"

cargo fetch --locked
cargo build --workspace --all-targets
