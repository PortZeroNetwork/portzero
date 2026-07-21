# Software delivery lifecycle

This repository uses a Heroku-style, tag-addressed release model (the same one
`portzero-cloud` uses):

- `staging` is the single long-lived integration branch and the default branch.
  Day-to-day work merges here via PR.
- There is **no** `release/*` branch. A stable **release is an immutable
  `vX.Y.Z` tag** — the release of record, browsable under Tags.
- Cutting a stable release is a deliberate, gated action: you dispatch the
  **Trigger Stable Release** workflow and pick the version bump, then a required
  reviewer approves the `production` GitHub Environment. That stamps the tag,
  and the tag push builds and publishes the signed release. (Naming: see
  [docs/developers/release-conventions.md](release-conventions.md) — the same workflow
  name, inputs, and gate as `portzero-cloud`.)
- GitHub Actions CI runs on PRs into `staging` (see
  `.github/workflows/ci.yml`).

## Local validation before release

Run the local checks on `staging` before cutting a release:

```bash
git switch staging
git pull
just verify
just e2e
```

The pre-commit and pre-push hooks also run the fast local checks, but `just e2e`
is intentionally manual because it needs root or Administrator privileges.

## Cut a stable release

A stable release is cut by the gated **Trigger Stable Release** workflow
(`.github/workflows/trigger-stable-release.yml`), which picks the bump explicitly
and pushes the `vX.Y.Z` tag from the `staging` tip:

1. Go to **Actions → Trigger Stable Release → Run workflow**, with `staging`
   selected as the branch to run from (or set **ref** explicitly).
2. Pick the **bump** (`patch` / `minor` / `major`).
3. Run it. The `promote` job pauses on the `production` environment gate until
   a **required reviewer** approves it — this is the release "button".
4. On approval, `promote` computes the next `vX.Y.Z` from the latest stable tag
   and pushes that tag from the chosen ref.

The tag push triggers the **Release** workflow's tag build (`push: tags: v*`),
which produces the **signed** stable artifacts and the GitHub Release. Inspect
or follow the runs from the **Actions** tab, or with the GitHub CLI:

```bash
gh run list --workflow Release
gh run watch
```

Because the version is decided at release time by the person cutting it, there
is no commit-message marker to remember and nothing to compute from the tip
commit — the bump you pick is the bump you get.

## When a release goes wrong

The `vX.Y.Z` tag is the immutable release of record, so recovery is almost
always **re-run** (same tag) or **roll forward** (new tag) — never move or
reuse a tag. Pick the matching case:

### The tag was pushed but the build failed partway

Symptom: the tag `vX.Y.Z` exists, but the Release run went red (a runner died,
an interop test flaked, signing hiccuped) — no GitHub Release, or a
half-populated one.

Fix: **re-run the same run — do not cut a new version.** The tag already points
at the right commit, so the build is fully reproducible from it.

- Actions → the failed **Release** run → **Re-run failed jobs** (or **Re-run all
  jobs**). This rebuilds off the exact same `vX.Y.Z` tag.

A transient CI/runner failure is not a reason to burn a version number.

### The tag was pushed but no build started at all

Symptom: `vX.Y.Z` exists but there is **no** Release run for it in Actions — the
`push: tags: v*` trigger never fired.

Re-pushing an identical tag is a no-op and won't re-trigger. Delete the remote
tag and push it again on the same commit (safe only while nothing has consumed
it yet — i.e. no GitHub Release, no formula bump):

```bash
git push origin :refs/tags/vX.Y.Z   # delete the remote tag
git push origin vX.Y.Z              # re-push the SAME tag on the SAME commit → re-fires the build
```

If that still doesn't fire, the tag push is being made with a token that can't
trigger workflows — push the tag from a PAT / GitHub App token instead of the
default `GITHUB_TOKEN` (see the trigger-safe tag push snippet in
[docs/developers/release-conventions.md](release-conventions.md)), or just cut the next
patch with a fresh promote.

### A bad release actually shipped (roll back)

You do **not** delete or move the tag to undo a release. Two steps:

1. **Stop the bleeding first.** In the GitHub **Release** UI, edit the bad
   release and mark it **pre-release** (or delete the Release; the tag can
   stay). Everything that pulls stable users toward an update resolves through
   GitHub's `/releases/latest`, which **ignores** pre-releases — so this single
   toggle immediately drops the bad build out of the in-CLI update check and the
   `curl | sh` / homepage paths. (See
   [unstable-channel.md](unstable-channel.md) for why `/releases/latest` is
   the linchpin.)
   - **Homebrew is separate:** `update-homebrew` already pushed the bad version
     into the tap's stable `portzero` formula, and the toggle above does **not**
     revert it. Until a re-publish or roll-forward release bumps it again,
     either revert the offending commit in the `homebrew-portzero` tap by hand,
     or accept that `brew install portzero` serves the bad version meanwhile.
2. **Re-publish the last good release (roll back).** Dispatch **Promote to
   Production** with **ref** = the last good `vX.Y.Z` tag and **bump** =
   `none`. That re-runs the Release build for that tag: the old release
   becomes `latest` again and `update-homebrew` re-pins the stable formula to
   it. (Only works for tags cut after the `channel: stable` re-publish path
   existed; older tags: re-run their original Release run from the Actions UI.)
3. **Roll forward.** Fix on `staging`, then cut a **new patch** release the
   normal way (Cut a stable release, above). The higher `vX.Y.Z` becomes
   `latest` and supersedes the bad one everywhere.

## Unstable releases

An unstable release is cut off `staging` by dispatching the same workflow with
**channel** `unstable` (the default). It is ungated and produces an **unsigned**
build tagged/marked GitHub `prerelease: true`, so it never touches the stable
install paths. See [unstable-channel.md](unstable-channel.md).
