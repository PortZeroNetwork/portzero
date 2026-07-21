# Unstable channel

Full installers are cut off `staging` for testing **without** any stable user
detecting, being offered, or receiving them. Call this the **unstable** channel
(not “edge” or “prerelease” in prose). Unstable builds ship **unsigned**.

## Automatic trigger

Whenever `staging` is updated (direct push or merged PR), GitHub Actions runs
an **Unstable Release** (`release.yml` on `push` to `staging`). That is the
normal path — no manual button required. You can still re-run the workflow or
dispatch it with `channel: unstable` if needed.

## The one guarantee

Everything that could pull a stable user toward an update resolves through
GitHub's **`/releases/latest`** endpoint, and that endpoint **ignores releases
marked with GitHub’s `prerelease: true` flag**. In this repo that covers:

- the in-CLI update check — `client/crates/cli/src/update.rs`
- the Linux installer’s default path — `scripts/linux-install.sh`
- the stable Homebrew formula (`portzero`)
- the repo homepage “Latest release” widget

So: **unstable releases use GitHub `prerelease: true`** and `make_latest: false`.
(`prerelease` is GitHub’s flag name, not our channel name.)

Unstable builds are built from unmodified `staging` source and connect to
**production** portzero.cloud (same as stable). The staging *cloud* is only used
inside release interop tests, never compiled into the client.

## What an Unstable Release produces

- version `X.Y.Z-rc.<run-number>` (base from next stable; `-rc.N` sorts below it)
- tag/release `vX.Y.Z-rc.N` with GitHub `prerelease: true`
- full platform matrix — **unsigned** Windows artifacts
- Homebrew formula currently named `portzero-edge` (implementation name; product language is **unstable**)

Stable releases go through **Trigger Stable Release** → tag → **Stable Release**.
Unstable tags never affect the next stable bump calculation.

## Installing an unstable build (opt-in)

Not advertised in the README or stable release notes.

### macOS — Homebrew

```sh
brew tap PortZeroNetwork/portzero
brew install portzeronetwork/portzero/portzero-edge   # unstable formula name today
```

Return to stable: `brew uninstall portzero-edge && brew install portzero`.

### Linux — install script

```sh
curl -fsSL https://github.com/PortZeroNetwork/portzero-local/releases/latest/download/linux-install.sh | PORTZERO_CHANNEL=prerelease sh
```

(`PORTZERO_CHANNEL=prerelease` is the install-script switch for the **unstable** channel.)

### Windows — MSI

Download the `.msi` from the unstable GitHub Release page. winget is stable-only.
Unsigned → SmartScreen may warn.

## Why these choices

- **GitHub `prerelease: true`** keeps `latest` / CLI update / default install off unstable.
- **`-rc.N` SemVer** orders below the eventual stable for GitHub and `update.rs`.
- **Auto on staging** keeps a continuous stream of testable installers without a human gate.
- **Trigger Stable Release** stays a small, auditable action; expensive builds re-run without re-approval.
