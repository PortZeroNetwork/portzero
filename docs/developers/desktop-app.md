# Desktop app (`portzero-app`)

The **PortZero app** is a native desktop app that replaced the browser-based
local dashboard as the primary way to see daemon/tunnel health, toggle HTTPS,
control the daemon, and run the bundled examples. This page documents its
architecture, how to run it in development, and how it is built and shipped.

The reason it exists: typing `portzero.net` in a browser address bar kept
autocompleting to `portzero.local` (browser history conflating the marketing
domain with the local overlay domain), and a desktop app sidesteps that
entirely while also just being a nicer way to use the tool than a browser tab.

## Architecture

- **Framework**: [Tauri v2](https://v2.tauri.app/). The binary is
  `portzero-app`.
- **Crate**: `client/crates/app` (Rust backend). Add it to the root
  `Cargo.toml` workspace `members` if you don't see it there yet.
- **Frontend**: React + TypeScript, at `client/crates/app/ui`. It renders the
  same data the old browser dashboard did — health, tunnels, issues, the
  HTTPS toggle, daemon start/stop/restart controls, and the examples list —
  as regular React state, not server-rendered HTML.
- **Backend is thin by design.** The Rust side exposes `#[tauri::command]`
  functions that the frontend calls via Tauri's `invoke`. Each command either:
  - proxies the daemon's management REST API (`http://portzero.local/status.json`,
    `http://portzero.local/v1/examples/*` — see
    `client/crates/daemon/src/management/server.rs` for the full route table
    and `api/management-v1.yaml` for the OpenAPI spec), or
  - shells out to the `portzero` CLI for daemon lifecycle actions
    (`start` / `stop` / `restart`) — the same pattern the tray already uses
    in `client/crates/tray/src/actions.rs`, so the app, the tray, and the CLI
    never disagree about how the daemon is spawned.

  There is no daemon logic duplicated into the app: it is a UI over the
  existing management API and CLI, not a second implementation.
- **Single-instance.** The app uses Tauri's single-instance plugin, so
  launching it again (from the tray, from `portzero start`, or by
  double-clicking it) focuses the existing window instead of opening a
  second one.

## Running it in development

```bash
just app-dev
```

Check `just --list` for the exact current recipe name and what it wraps
(typically `tauri dev`, which runs the Vite dev server for `ui/` and the
Rust app shell together with hot reload). You will normally also want the
daemon running in another terminal (`portzero start --foreground` or
`sudo -E portzero start --foreground` if you need the local overlay) so the
app has real data to show — see
[Development](development.md) for the rest of the day-to-day contributor
workflow (tests, hooks, `just`).

## Build and embedding

1. The frontend is built first: `npm run build` inside `client/crates/app/ui`
   produces static assets in `ui/dist`.
2. Tauri embeds `ui/dist` into the `portzero-app` binary at compile time —
   there is no separate asset bundle to ship or a web server to run; the
   built app is self-contained.
3. `cargo build -p portzero-app --release` (or the equivalent packaging step
   in the release workflow) produces the final per-platform binary/bundle.

## Packaging and shipping

`portzero-app` ships alongside the other client binaries (`portzero`,
`portzero-tray`) in the same per-platform installers described in
[Release conventions](release-conventions.md) and
[Software delivery lifecycle](sdlc.md) — Unstable builds on every push to
`staging`, Stable builds off a tagged `vX.Y.Z` via **Trigger Stable
Release** → **Stable Release**. Windows code-signing specifics (currently
scoped to `portzero.exe`) are tracked in [Windows signing](windows-signing.md);
confirm whether `portzero-app.exe` has been added to that signing/verification
list before relying on it being signed. macOS codesigning/notarization is
optional and secret-gated — see [macOS signing](macos-signing.md).

## The old browser dashboard still exists — intentionally unlinked

The daemon still serves the original browser dashboard
(`client/crates/daemon/src/management/assets/dashboard.html`, plus
`/status.json` and the `/v1/examples/*` endpoints it and the app both read)
at `http://portzero.local`. Nothing was removed from the daemon.

What changed is what *links to* it:

- The tray's **Open PortZero** menu item now launches the PortZero app
  instead of opening a browser tab.
- `portzero start` now launches the app (unless run with `--no-browser`)
  instead of opening a browser tab.
- The first-run welcome notification points at the app.

The browser dashboard is kept as a debugging surface (e.g. `curl
http://portzero.local/status.json`, or opening it directly when you don't
have/want the desktop app) — it is not the recommended entry point and is
not linked to from the tray, the CLI, or the notification anymore. Don't add
new links to it from user-facing surfaces; point at the app instead.

## Related docs

- [Development](development.md) — `just`, tests, git hooks.
- [Release conventions](release-conventions.md) /
  [Software delivery lifecycle](sdlc.md) — how Unstable/Stable builds are cut.
- [Windows signing](windows-signing.md) — code-signing runbook.
- [macOS signing](macos-signing.md) — optional, secret-gated codesign +
  notarization.
- [portzero.net/docs](https://portzero.net/docs) — user-facing docs on installing
  and using Port Zero, including [Getting
  started](https://portzero.net/docs/getting-started), which describes the same
  app from the product-user side.
- [Examples](https://portzero.net/docs/examples) — the same bundled examples run
  by hand (clone + `PZ_TUNNEL`), i.e. the CLI equivalent of the app's **Getting
  started** panel that downloads and runs them with one click.
