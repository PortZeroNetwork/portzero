# Component versions

A PortZero install is four programs, not one: `portzero` (the CLI, which is also
the process the daemon runs in), `portzero-tray`, and `portzero-app`. They are
built together and installed together, but they *run* independently — so after
an upgrade, a daemon started three days ago is still executing the old build
while the binary on disk is new. That mismatch is invisible until it produces a
confusing, unrelated-looking failure (a state file the old daemon doesn't
understand, a menu item the tray doesn't have).

This page covers how one version is stamped across every component, and how the
product reports and checks it at runtime.

## One version, one place

The root `Cargo.toml` holds `[workspace.package] version`, and every crate
inherits it:

```toml
# client/crates/tray/Cargo.toml
version.workspace = true
```

`scripts/set-version.sh <semver>` is the only thing that rewrites it. It stamps
three files — the workspace version, the resolved versions in `Cargo.lock`, and
the Tauri bundle version in `client/crates/app/tauri.conf.json` — and the
release workflow runs it before every build (see
[Release version numbers](release-version-numbers.md)).

Never give a crate its own literal `version = "..."`. The runtime consistency
check below only means something because a single build cannot produce
components with different versions; a hardcoded crate version reintroduces
exactly the drift the check exists to catch.

> Historical note: before this, `scripts/set-cli-version.sh` stamped only the
> CLI crate. Every release ever cut shipped a daemon and tray hardcoded at
> `0.1.0` and an app at `0.0.1`, and `/v1/daemon/status` reported `0.1.0`
> regardless of the release it came from.

## How a running component reports its version

`portzero_daemon::versions` (`client/crates/daemon/src/versions.rs`) owns this.
It lives in the daemon crate because the CLI, tray, and app all depend on it —
the same layering that lets the tray read daemon state files.

Long-running components announce themselves at startup and withdraw on a clean
exit:

```rust
versions::announce(&config, versions::Component::Tray, versions::BUILD_VERSION);
```

That writes `~/.portzero/components/<name>.json` with the version and the PID.
Readers treat a record as absent unless that PID is still running *that
component's own binary* (`pid_lookup::process_is_alive_named`), so a component
that crashed never leaves a phantom version behind.

Checking the binary name, not just liveness, is load-bearing. PIDs are recycled,
so a record left by a crashed component eventually names an unrelated process
and a liveness-only check reports it as running forever. On Linux that happens
even sooner than PID exhaustion suggests: `/proc/<n>` resolves for *thread* ids
as well as process ids, so any thread spawned with the old number is enough.
A real install hit exactly this — a dead tray's PID was taken by a `kaccess`
thread, after which the tray's single-instance guard refused to start a tray at
every login while `portzero version` and `portzero doctor` both insisted the
tray was running. See `task-95`.

| Component | Where its version comes from |
|-----------|------------------------------|
| Daemon | its record file (written by `run_discovery_loop`) |
| Tray | its record file (written after the singleton is claimed) |
| Desktop app | its record file (written in `main`) |
| CLI | running the installed binary: `portzero --version` |

The CLI has no record because it is never long-lived — there is no CLI process
to ask, so the report reads the *installed* binary instead. That is also why the
CLI's row is the reference point for a mismatch: it is what the daemon, tray,
and app would run if they were restarted right now.

A daemon that is running but has no record is reported specially: it predates
version reporting, which is itself proof it is older than the installed build.

`versions::collect()` gathers all four; `versions::evaluate()` decides whether
they agree and produces the user-facing summary and next steps. Collection does
I/O, evaluation is pure — the interesting logic is unit-tested without touching
the filesystem.

## Where it surfaces

- **Desktop app** — the **Version** panel (`ui/src/components/Versions.tsx`,
  backed by the `get_versions` command) lists every component with its version
  and state, and shows the remediation steps when they disagree. The app's own
  version is also in the title bar, with a `versions differ` warning next to it.
- **`portzero version`** — the same report as a terminal table. Distinct from
  `portzero --version`, which speaks only for the CLI binary you just ran.
- **`portzero doctor`** — the `component versions` check, a warning (not a
  failure) on mismatch, since nothing is broken yet.
- **Tray menu** — a `PortZero vX.Y.Z` line. The tray only reports itself; it
  never runs the CLI to compare, because its menu rebuilds every few seconds.
- **`/status.json` and `/v1/daemon/status`** — the running daemon's version, for
  API clients.

## Adding a component

1. Add a variant to `versions::Component` (`id()`, `label()`, and `ALL`).
2. Call `versions::announce` at startup and `versions::withdraw` on exit if it
   is long-running; otherwise give the binary a `--version` flag and read it
   like the CLI's.
3. Make sure its crate inherits `version.workspace = true`.
