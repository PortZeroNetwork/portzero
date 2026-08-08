# Port Zero Local — developer documentation

Audience: [target-audience.md](target-audience.md).

## Documents

- [Development](development.md) — tests, `just`, lefthook, Ticketry
- [Desktop app](desktop-app.md) — `portzero-app` (Tauri v2), dev workflow, build/embedding
- [Tunnel protocol support](tunnel-protocol-support.md) — what Local vs Cloud tunnels can carry (WebSockets, body limits, timeouts)
- [Which address the daemon dials](backend-address-selection.md) — how a discovered port becomes a backend address, and why the family matters (`::1` vs `127.0.0.1`)
- [Complexity budgets](complexity-budgets.md)
- [Software delivery lifecycle](sdlc.md) — staging, stable tags, workflows
- [Release version numbers](release-version-numbers.md)
- [Component versions](component-versions.md) — one workspace version, and the runtime check that the daemon/tray/app/CLI agree
- [Unstable channel](unstable-channel.md) — auto Unstable Release on every staging push
- [Release conventions](release-conventions.md) — shared with `portzero-cloud` (keep in sync)
- [Windows signing](windows-signing.md)

## Release channel and workflow names

| Prose | Meaning |
|-------|---------|
| **stable** | Full user-facing release (`vX.Y.Z`, default install/update paths) |
| **unstable** | Testing builds; never GitHub “latest” |

| Actions workflow name | Role |
|----------------------|------|
| **Trigger Stable Release** | Gated button: stamp `vX.Y.Z` (or re-publish with `bump: none`) |
| **Stable Release** | Signed multi-platform build off that tag |
| **Unstable Release** | Automatic on every push to `staging` (unsigned) |

Do **not** call the non-stable channel “edge” or “prerelease” in new docs. Product
**edge servers** (cloud tunnels) are unrelated to the unstable release channel.

## Also at repo root

- [CONTRIBUTING.md](../../CONTRIBUTING.md)
- `AGENTS.md` / `CLAUDE.md` — agent notes + `.instructions/` modules
- `justfile`
