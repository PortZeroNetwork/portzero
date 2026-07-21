# Continuous Delivery — downloadable software

**Always** use continuous delivery, even from the very beginning of a project.
Also enable `continuous-delivery-shared` for branch/Terraform rules common to
both CD modes.

## Channel terminology

Use these names consistently in docs, agent notes, and human conversation:

| Term | Meaning |
|------|---------|
| **stable** | A full, user-facing release. Immutable `vX.Y.Z` tag / package channel that update checks and default installers use. |
| **unstable** | A non-stable build for testing. Must never become GitHub “latest” or the default install path. |

**Call the non-stable channel unstable** — not “edge” and not “prerelease” — in
prose. Implementation may still use GitHub’s `prerelease: true` flag or `-rc`
version suffixes as mechanics; those are not the product name of the channel.
Prefer workflow inputs and package formula names that say `unstable` when you
touch them.

## Canonical workflow split

Separate the **gate/version decision** from the **expensive multi-platform
build**. Humans approve a small, auditable action; the build runs automatically
from the result and can be re-run without re-opening the approval gate.

| Workflow `name:` (Actions UI) | Typical file | Trigger | What it does |
|-------------------------------|--------------|---------|--------------|
| **Trigger Stable Release** | `trigger-stable-release.yml` (or legacy `promote-production.yml`) | `workflow_dispatch` only, behind the `production` GitHub Environment (required reviewer when mature) | Computes the next `vX.Y.Z` from an explicit `bump` input (`patch` / `minor` / `major` — never inferred from commit messages), stamps an annotated tag on the chosen ref (normally `staging`), and pushes the tag with a PAT (`RELEASE_TAG_TOKEN`) so the tag event fires the build workflow (a `GITHUB_TOKEN` tag push will not trigger workflows). With `bump: none` and `ref` set to an existing `vX.Y.Z` tag, re-dispatches the stable build for that tag (rollback / re-publish) without cutting a new version. |
| **Stable Release** | `release.yml` (or `stable-release.yml`) | Push of tag `vX.Y.Z`, or `workflow_dispatch` with channel `stable` on that tag | Builds **signed** installers for all platforms, runs release tests, publishes the GitHub Release (latest), bumps package managers (Homebrew / WinGet). Idempotent: re-run without re-triggering the gate. |
| **Unstable Release** | same build workflow or a twin | **Every push to `staging`** (and optional `workflow_dispatch`) | Builds **unsigned** installers from `staging`, marks the GitHub Release so it is not latest (`prerelease: true` / `make_latest: false`), uses an unstable package formula if needed. Must never be offered by default update/install paths. |

Use `run-name:` on a shared build workflow if one file serves both channels, so
the Actions run list still shows **Stable Release** vs **Unstable Release**.

## Rules

- There **must** always be a long-lived staging branch.
- There must **never** be a production, main, or master branch. There may eventually be branches like `release/1.0.x` and `release/1.1.x` so security fixes and features can be backported to previous versions, but that structure should NEVER be created until backporting is necessary.
- Whenever the staging branch is updated (direct push or PR merge into `staging`), GitHub Actions **must** automatically run an **Unstable Release** build. Unstable must not wait for a manual button as the normal path (manual re-run / `workflow_dispatch` is fine as a supplement).
- Unstable builds **must** be marked so stable update paths ignore them.
- **Stable** releases **must** go through **Trigger Stable Release** (gated) then **Stable Release** (build). Never cut a stable `vX.Y.Z` automatically from a branch push alone.
- **Roll back** a stable artifact by re-running **Trigger Stable Release** with `ref` set to an older `vX.Y.Z` and `bump: none` (re-publishes that tag’s build without a new version).
