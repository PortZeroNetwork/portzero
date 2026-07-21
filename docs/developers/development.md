# Development

These instructions are for contributors to portzero-local itself.

The project uses `just` as the task runner everywhere. Run `just --list` to see available commands.

## Running the test suite

- `just test` — full unprivileged suite; the real-TUN/Wintun test skips cleanly because `PORTZERO_REQUIRE_REAL_TUN_E2E=1` is not set.
- `just e2e` — explicitly opts into the real adapter smoke test and runs it with the current OS's required privileges (`sudo` on Unix, Administrator + Wintun on Windows).

## Local checks to save GitHub Actions minutes

GitHub Actions is not free. The main CI job runs:

- `cargo fmt -- --check`
- `cargo clippy --workspace -- -D warnings`
- `cargo test --workspace`
- `cargo run -p portzero-xtask --bin check-file-size-budget` (the `file-size-budget` job — see [Complexity budgets](complexity-budgets.md))

(plus privileged E2E on a separate job).

Run the equivalent locally before you push:

```bash
just verify          # fmt + clippy (-D) + test + complexity
# or individually
just fmt-check
just clippy
just test
just complexity      # file-size budget on client/*.rs; see docs/developers/complexity-budgets.md
```

These are the same commands the CI uses on Linux (and the Rust parts of the other OS jobs).

## Git hooks (automatic checks on commit & push)

We use **lefthook** (a fast, lightweight, cross-platform git hooks framework) so the checks run automatically:

- `pre-commit`: `just fmt-check` + `just complexity --changed` (file-size budget, staged `client/*.rs` files only)
- `pre-push`  : `just fmt-check` + `just clippy` + `just test`

### One-time setup on a machine (Linux, macOS, or Windows)

```bash
just install-hooks
```

This will:

1. Tell you how to install `lefthook` if it is missing (brew / winget: coming soon / scoop / etc.).
2. Run `lefthook install`.

You only need to do this once per clone / per machine. After that the hooks stay active.

### Also set up Ticketry hooks

```bash
ticketry init
```

This installs the non-blocking background hooks that keep the ticket index up to date after commits, checkouts, etc. It is idempotent.

### Privileged / real TUN tests and hooks

**The privileged real adapter test is deliberately not run by the hooks.**

- `just e2e` (and the `real_tun_overlay` test) requires root/Admin.
- Running `sudo` from inside a git hook is unreliable:
  - Hooks often have no controlling tty → password prompt fails or hangs.
  - Behavior is different on Windows (no sudo).
  - It would be surprising and annoying on every push.
- The unprivileged `just test` (which the hooks run) is already comprehensive. The privileged test **skips cleanly and passes** unless it is explicitly opted into with `PORTZERO_REQUIRE_REAL_TUN_E2E=1`.
- The real E2E is still run by CI in the separate "E2E (privileged real TUN)" job.

Run `just e2e` manually when you are working on TUN/overlay networking changes and have root available.

### Bypassing (last resort)

```bash
git commit --no-verify
git push --no-verify
```

Use sparingly. The point of the hooks is to stop broken clippy or failing tests from ever reaching CI.

### Manual hook run

```bash
lefthook run pre-push
lefthook run pre-commit
```

## Other just recipes for development

```bash
just --list
just check        # fast cargo check
just clippy-all   # broader clippy (all targets + features)
just verify       # the full local pre-push equivalent
```
