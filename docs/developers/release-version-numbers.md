# Release version numbers

The stable version is decided **at release time** by the person cutting the
release, not inferred from a commit message. In the gated **Promote to
Production** workflow (`.github/workflows/promote-production.yml`, `promote`
job) you pick a `bump` —
`patch` / `minor` / `major` — and the next `vX.Y.Z` is computed from the latest
stable tag and stamped as an immutable tag. See
[sdlc.md](sdlc.md#cut-a-stable-release) for the full flow.

```bash
# promote job, roughly:
LATEST_TAG=$(git tag -l "v[0-9]*.[0-9]*.[0-9]*" | grep -v -- '-' | sort -V | tail -1)
LATEST_TAG="${LATEST_TAG:-v0.0.0}"
case "$BUMP" in
  major) MAJOR=$((MAJOR + 1)); MINOR=0; PATCH=0 ;;
  minor) MINOR=$((MINOR + 1)); PATCH=0 ;;
  patch) PATCH=$((PATCH + 1)) ;;
esac
```

The bump you pick is the bump you get:

- Major bump (e.g. `0.0.12` → `1.0.0`): choose `major`.
- Minor bump (e.g. `0.0.12` → `0.1.0`): choose `minor`.
- Patch bump (e.g. `0.0.12` → `0.0.13`): choose `patch`.

The base version is always computed from the latest **stable** `vX.Y.Z` tag
(`git tag -l "v[0-9]*.[0-9]*.[0-9]*" | grep -v -- '-' | sort -V | tail -1`) —
unstable tags (`vX.Y.Z-rc.N`) are filtered out so they never perturb the next
stable bump. Once pushed, the tag itself is the release of record; the tag build
reads its version straight from the tag name (`github.ref_name`) rather than
recomputing anything.

## Unstable builds

An unstable-channel `workflow_dispatch` run of the same workflow (Actions →
Release → Run workflow off `staging`, `channel: unstable`) instead produces a
unstable versioned `X.Y.Z-rc.<run-number>` and tagged/marked
`prerelease: true`. For edge builds only, the `X.Y.Z` base is still derived from
the tip commit's markers (`BREAKING CHANGE`/`[major]` → major, `feat:`/`[minor]`
→ minor, else patch) on top of the latest stable tag, so an unstable cut always
sorts just below the next possible stable. Those builds are the unstable testing
channel and never touch the stable install paths — see
[unstable-channel.md](unstable-channel.md).
