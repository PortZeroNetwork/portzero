# Release conventions

> This document is the canonical description of the staging → trigger stable
> → build model used by PortZero repositories. It is written to be lifted
> verbatim into any new product. `portzero-cloud` (deployed service) and
> `portzero-local` (distributed artifact) are the two reference
> implementations; keep the two copies of this file in sync.

## The model

- **One long-lived branch: `staging`.** It is the default branch and the only
  integration branch. Day-to-day work merges into it via PR. There is no
  `production`, `develop`, `main`, or `release/*` branch.
- **Production is a promoted, tag-addressed release** (Heroku-style), not a
  branch. Promotion is a deliberate, manual act: dispatch the
  **Trigger Stable Release** workflow, then approve the `production` GitHub
  Environment gate (a required reviewer) — that approval is the "button", and
  it is logged with who/when.
- **The release of record is an immutable `vX.Y.Z` git tag.** The promote
  stamps it; pushing it triggers the **Release** workflow (build + GitHub
  Release). Release history is browsable under Tags/Releases — no branch
  divergence, no merge-commit churn.
- **The version bump is explicit.** The promote takes a `bump` input
  (`patch` / `minor` / `major`) chosen by the person promoting — never
  inferred from commit messages. The bump you pick is the bump you get.
- **Rollback is a re-promote**: dispatch Trigger Stable Release again with
  `ref` set to an older tag/SHA and `bump: none` — it redeploys/re-publishes
  the existing release without cutting a new tag.

### The two archetypes

The same names cover both kinds of product:

| | Deployed service (`portzero-cloud`) | Distributed artifact (`portzero-local`) |
|---|---|---|
| "Trigger Stable Release" means | Deploy the ref to the production infra, health-check it, then stamp `vX.Y.Z` | Stamp `vX.Y.Z` on the ref (the gate is the whole act) |
| Build workflow does | Build/publish artifacts for the record | Build signed installers, publish the GitHub Release, bump Homebrew/WinGet |
| `bump: none` rollback | Redeploy the older ref's already-built image; no new tag | Re-run **Stable Release** for the older tag (`channel: stable`), re-pointing `latest` and package managers |
| `Deploy Staging` / `Teardown Staging` | Yes — a live staging environment | Not applicable (CI tests against portzero-cloud's staging instead) |

## Canonical names

### Workflows

| File | `name:` / run title | Trigger | Purpose |
|---|---|---|---|
| `ci.yml` | `CI` | PRs into `staging` + pushes to `staging` | Build, lint, test — plus fast credential-free static config checks |
| `deploy-staging.yml` | `Deploy Staging` | push to `staging` | Ungated auto-deploy + live E2E (**service** archetype only) |
| `trigger-stable-release.yml` | `Trigger Stable Release` | `workflow_dispatch` + `production` env gate | Stamp `vX.Y.Z` (or `bump: none` re-publish). Downloadable: tag only. Service: deploy production then tag. |
| `release.yml` | run-name **Stable Release** or **Unstable Release** | **Stable:** push `vX.Y.Z` tag or `workflow_dispatch` `channel=stable`. **Unstable (downloadable):** every push to `staging` or `workflow_dispatch` `channel=unstable` | Multi-platform build + GitHub Release (+ packages). Unstable is unsigned and never latest. |
| `teardown-staging.yml` | `Teardown Staging` | `workflow_dispatch` | Destroy staging infra on demand (service archetype only) |

Product-specific extra workflows (extra E2E suites, `terraform-plan.yml` PR
plans, retry helpers, …) are fine — they are additions, not replacements, and
should not reuse the five canonical names above for anything else.

### Jobs

| Job id | Where | Notes |
|---|---|---|
| `check` | CI | The main build/lint/test job |
| `deploy` | Deploy Staging | Deploy + verify staging |
| `promote` | Trigger Stable Release | The gated job (`environment: production`) |
| `version`, `build`, `release` | Release | Determine version from the tag; build artifacts; publish |
| `teardown` | Teardown Staging | Terraform destroy |

Renaming a job (or its display `name:`) breaks any branch-protection
**required status check** that references the old name — update the
protection rule in the same change.

### GitHub Environments

| Environment | Gate | Used by |
|---|---|---|
| `staging` | none (ungated) | Deploy Staging, staging-scoped secrets/vars in CI/E2E |
| `production` | **required reviewer** | The `promote` job — in every repo, both archetypes |

`production` is the single gate-environment name everywhere, even for
artifact products where nothing is "deployed" — the concept being gated
(a user-visible release) is the same.

### Dispatch inputs

| Input | Values | Meaning |
|---|---|---|
| `ref` | any ref; blank = the ref dispatched from (normally `staging`) | What to promote/release. An older tag/SHA = rollback |
| `bump` | `patch` \| `minor` \| `major` \| `none` | Explicit semver bump; `none` = rollback/re-publish of an existing ref, cuts no tag |
| `channel` | `stable` \| `unstable` | Only for products with an unstable channel. `unstable` = ungated, unsigned build (GitHub `prerelease: true`, never `latest`); `stable` = re-publish an existing `vX.Y.Z` tag |

### Secrets and variables

Release-machinery names — identical in every product:

| Name | Kind | Purpose |
|---|---|---|
| `RELEASE_TAG_TOKEN` | secret (PAT, `contents: read+write` on the repo) | Pushes the `vX.Y.Z` tag so the push actually triggers `Stable Release` / `Unstable Release` (see below) |
| `HOMEBREW_TAP_TOKEN` | secret (PAT on the tap repo) | Artifact products: push formula bumps to the Homebrew tap |
| `WINGET_TOKEN` | secret (PAT) | Artifact products: submit WinGet manifests |

Product-specific external credentials: SCREAMING_SNAKE, provider-prefixed —
`DO_TOKEN`, `DOCKERHUB_TOKEN`, `CF_API_TOKEN`, `STRIPE_*`, `SENDGRID_API_KEY`,
`AZURE_*`. Don't rename working third-party secrets for cosmetics; the
GitHub-Settings side does not rename with the workflow reference.

### Tags

- `vX.Y.Z` — annotated, immutable, the release of record. Never moved, never
  reused, never deleted to "undo" a release.
- `vX.Y.Z-rc.N` — unstable-channel tags, excluded from stable version
  computation.
- Moving convenience tags (e.g. `production-current` for cold-boot pointers)
  are allowed but must never be inputs to version computation.

## The trigger-safe tag push (copy this snippet)

A tag pushed with the default `GITHUB_TOKEN` does **not** trigger workflows
(GitHub's anti-recursion rule) — `Stable Release` / `Unstable Release` would silently never fire. Two
things are required, and both fail silently if forgotten:

1. Push the tag with a **PAT** (`RELEASE_TAG_TOKEN`, `contents: read+write`).
2. **Strip the header `actions/checkout` persisted.** Checkout stores the
   `GITHUB_TOKEN` as an `http.https://github.com/.extraheader` auth header,
   and git sends that header **in preference to** the credentials embedded in
   the push URL — so the push is re-attributed to `GITHUB_TOKEN` and triggers
   nothing, even though your PAT is right there in the URL.

```yaml
- name: Stamp release tag
  if: ${{ inputs.bump != 'none' }}
  env:
    BUMP: ${{ inputs.bump }}
    RELEASE_TAG_TOKEN: ${{ secrets.RELEASE_TAG_TOKEN }}
  run: |
    # Latest stable vX.Y.Z tag across history; default v0.0.0 if none.
    # Unstable-channel tags (vX.Y.Z-rc.N) are excluded via `grep -v -- '-'`.
    LATEST_TAG=$(git tag -l "v[0-9]*.[0-9]*.[0-9]*" | grep -v -- '-' | sort -V | tail -1)
    LATEST_TAG="${LATEST_TAG:-v0.0.0}"
    MAJOR=$(echo "$LATEST_TAG" | sed 's/^v//' | cut -d. -f1)
    MINOR=$(echo "$LATEST_TAG" | cut -d. -f2)
    PATCH=$(echo "$LATEST_TAG" | cut -d. -f3)
    case "$BUMP" in
      major) MAJOR=$((MAJOR + 1)); MINOR=0; PATCH=0 ;;
      minor) MINOR=$((MINOR + 1)); PATCH=0 ;;
      patch) PATCH=$((PATCH + 1)) ;;
    esac
    TAG="v${MAJOR}.${MINOR}.${PATCH}"
    echo "Stamping $TAG on $(git rev-parse --short HEAD) (was $LATEST_TAG)"
    # Annotated tags need a tagger identity; the default checkout sets none.
    git config user.name "github-actions[bot]"
    git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
    git tag -a "$TAG" -m "Release $TAG"
    # Fail loud rather than silently pushing with a token that won't
    # trigger the Release workflow.
    if [ -z "${RELEASE_TAG_TOKEN:-}" ]; then
      echo "::error::RELEASE_TAG_TOKEN secret is not set — a GITHUB_TOKEN tag push would not trigger the release build. Add a PAT with contents:write as RELEASE_TAG_TOKEN." >&2
      exit 1
    fi
    # actions/checkout persists a GITHUB_TOKEN auth header
    # (http.<host>.extraheader) that GitHub honors OVER the URL's PAT
    # credentials, re-attributing the push to GITHUB_TOKEN — which never
    # triggers workflows. Strip it so the PAT authenticates the push.
    git config --local --unset-all http.https://github.com/.extraheader || true
    git push "https://x-access-token:${RELEASE_TAG_TOKEN}@github.com/${GITHUB_REPOSITORY}.git" "$TAG"
```

The job that runs this needs `permissions: contents: write` and a checkout
with `fetch-depth: 0` (so the latest tag is visible to bump from).

## Safety features that stay in the promote path

These are deliberate; do not "clean them away" when copying the model:

- **Health gate before the tag.** A deploy-archetype promote must verify the
  live system over HTTPS *before* stamping the release tag — a broken deploy
  must fail the run, not become the release of record.
- **Explicit destructive-change guard.** If the promote applies infra (e.g.
  Terraform), refuse plans that destroy/replace critical resources unless the
  exact resource address was named in an input (`terraform_replace`).
- **Known-good cold-boot pointers.** If infra can be rebuilt from scratch, a
  moving tag (image `production-current` + matching git tag) should be
  advanced only after the health gate passes, so a rebuild boots the release
  that is actually live.
- **Fail-loud token checks.** Missing `RELEASE_TAG_TOKEN` errors out instead
  of pushing a tag that triggers nothing.

## New product setup checklist

One-time GitHub configuration for a fresh repo adopting this model:

1. **Branch**: create the repo with `staging` as the default (and only
   long-lived) branch. Delete `main` if the platform created one.
2. **Branch protection** on `staging`: require PRs; require the `CI` status
   checks you care about (job display names, e.g. `Check & Test`); no force
   pushes.
3. **Environments**:
   - `staging` — no protection rules. Scope staging-only secrets/vars here.
   - `production` — add yourself (or the release owners) as a **required
     reviewer**. Scope production-only secrets/vars here.
4. **Secrets**:
   - `RELEASE_TAG_TOKEN` — a fine-grained PAT, this repo only,
     `contents: read+write`. Required for every product.
   - Artifact products: `HOMEBREW_TAP_TOKEN` (PAT on the tap repo),
     `WINGET_TOKEN` if shipping WinGet.
   - Product-specific provider credentials as needed (provider-prefixed
     names).
5. **Workflows**: copy `ci.yml`, `trigger-stable-release.yml`, `release.yml`
   (plus `deploy-staging.yml` / `teardown-staging.yml` for a deployed
   service) and strip the product-specific jobs/steps.
6. **First release**: the version machinery defaults to `v0.0.0` when no tag
   exists, so the first promote with `bump: minor` cuts `v0.1.0` — no seed
   tag needed.
7. **Verify the tag trigger once**: after the first promote, confirm a
   `Stable Release` / `Unstable Release` run started for the new tag. If the tag exists but no run
   started, the tag was pushed by `GITHUB_TOKEN` — see the snippet above.
