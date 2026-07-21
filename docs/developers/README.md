# Port Zero Local — developer documentation

Audience: [target-audience.md](target-audience.md).

## Documents

- [Development](development.md) — tests, `just`, lefthook, Ticketry
- [Desktop app](desktop-app.md) — `portzero-app` (Tauri v2), dev workflow, build/embedding
- [Complexity budgets](complexity-budgets.md)
- [Software delivery lifecycle](sdlc.md) — staging, stable tags, workflows
- [Release version numbers](release-version-numbers.md)
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
