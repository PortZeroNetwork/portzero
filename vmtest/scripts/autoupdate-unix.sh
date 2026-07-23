#!/usr/bin/env bash
# AUTO-UPDATE E2E (Linux + macOS): prove the real `portzero update` self-updater
# works end to end against a throwaway local release mirror — the thing that, if
# it ever breaks, silently strands every installed user's ability to update.
#
# `portzero update` resolves its download base from
# `portzero_domain::endpoints::update_download_base`, overridable with
# `PZ_TUNNEL_UPDATE_BASE_URL`. We point it at a local directory holding a
# hand-built `version.json` (the exact `{"version","commit"}` shape release.yml
# publishes) and a `portzero-<os>-<arch>.tar.gz` archive, then drive the real
# command and assert it detects, downloads, unpacks, and swaps the binary in
# place — with no network and no second build required.
#
# Phases (greppable per the vmtest README: `PHASE=<name> ... ok=<true|false>`,
# then a final `RESULT=PASS|FAIL`):
#   detect      `update --check` reports an available update against a bumped manifest
#   apply       `update --force` downloads+unpacks+swaps; result runs and matches the archive
#   noop        `update` against a same-version manifest reports up-to-date and swaps nothing
#   crossver    (gated) a PRIOR binary self-updates to the new build and its --version advances
#
# The bump-to-99.0.0 mirror means the assertions hold regardless of the built
# binary's actual version. Because CI builds exactly one binary, the apply phase
# proves the *swap mechanism* (new inode, functional result, byte-provenance),
# not a version bump; the gated crossver phase proves a real version advance when
# a prior binary that already understands `update` is provided via PORTZERO_EXE_OLD.
#
# Env:
#   PORTZERO_EXE      new (under-test) portzero binary. Defaults to the harness
#                     push locations used by the other unix flavors.
#   PORTZERO_EXE_OLD  optional prior-version binary. The crossver phase runs only
#                     if it is set AND already understands `portzero update`
#                     (releases predating this feature can't self-update — that's
#                     exactly the forward-compat contract this test guards); it
#                     SKIPs cleanly otherwise.
# Helper predicates are invoked indirectly via assert; silence spurious SC2317.
# shellcheck disable=SC2317
set -uo pipefail

fails=0

assert() { # <phase> <condition-cmd...>
    local phase="$1"; shift
    if "$@" >/dev/null 2>&1; then echo "PHASE=$phase ok=true"
    else echo "PHASE=$phase ok=false"; fails=$((fails + 1)); fi
}

contains() { printf '%s' "$2" | grep -qi -- "$1"; } # <needle> <haystack>
sha256_of() { # <file>
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
    else shasum -a 256 "$1" | awk '{print $1}'; fi
}
inode_of() { ls -i "$1" 2>/dev/null | awk '{print $1}'; }
version_of() { "$1" --version 2>/dev/null | awk '{print $NF}'; }

# <os>-<arch>, matching release.yml's archive_name.
target_triple() {
    local os arch
    case "$(uname -s)" in
        Linux*)  os=linux ;;
        Darwin*) os=darwin ;;
        *) echo "unsupported-os" ; return 1 ;;
    esac
    case "$(uname -m)" in
        x86_64|amd64)  arch=amd64 ;;
        aarch64|arm64) arch=arm64 ;;
        i686|i386)     arch=x86 ;;
        *) echo "unsupported-arch" ; return 1 ;;
    esac
    printf '%s-%s\n' "$os" "$arch"
}

# Build a release mirror dir: version.json (given version) + a tar.gz archive
# whose single binary is <bin>. Echoes the mirror path.
build_mirror() { # <bin> <version> <target>
    local bin="$1" version="$2" target="$3"
    local mirror; mirror="$(mktemp -d)"
    local stage="$mirror/stage/portzero-${target}"
    mkdir -p "$stage"
    cp "$bin" "$stage/portzero"; chmod +x "$stage/portzero"
    ( cd "$mirror/stage" && tar czf "$mirror/portzero-${target}.tar.gz" "portzero-${target}" )
    rm -rf "$mirror/stage"
    # Same shape release.yml emits — binds this test to the manifest format.
    printf '{"version":"%s","commit":"autoupdate-test"}\n' "$version" > "$mirror/version.json"
    printf '%s\n' "$mirror"
}

# --- preflight -------------------------------------------------------------
EXE="${PORTZERO_EXE:-}"
for cand in "$EXE" /root/pz-target/release/portzero /tmp/portzero-vmtest/bin/portzero \
            "$(cd "$(dirname "$0")/../.." 2>/dev/null && pwd)/target/release/portzero"; do
    [ -n "$cand" ] && [ -x "$cand" ] && { EXE="$cand"; break; }
done

TARGET="$(target_triple || true)"
echo "PHASE=preflight exe=${EXE:-none} target=${TARGET:-none}"
if [ -z "${EXE:-}" ] || [ ! -x "$EXE" ]; then
    echo "RESULT=FAIL no portzero binary found (set PORTZERO_EXE)"; exit 1
fi
if [ "$TARGET" = "unsupported-os" ] || [ "$TARGET" = "unsupported-arch" ] || [ -z "$TARGET" ]; then
    echo "RESULT=FAIL unsupported OS/arch for self-update ($TARGET)"; exit 1
fi
if ! "$EXE" update --help >/dev/null 2>&1; then
    echo "RESULT=FAIL the under-test binary has no \`update\` subcommand"; exit 1
fi

ARCHIVE_SHA=""     # sha256 of the archive's binary, for the provenance assert
export PZ_TUNNEL_NO_UPDATE_CHECK=1   # silence the background notifier during runs
export PZ_TUNNEL_UPDATE_SKIP_SETUP=1 # hermetic: no privileged setup side effects

# Install the new binary at a writable, isolated path (so a non-root swap works
# and nothing touches the real install).
INSTALL_DIR="$(mktemp -d)"; P="$INSTALL_DIR/portzero"
cp "$EXE" "$P"; chmod +x "$P"

# --- detect ----------------------------------------------------------------
BUMP_MIRROR="$(build_mirror "$EXE" "99.0.0" "$TARGET")"
ARCHIVE_SHA="$(sha256_of "$EXE")"
CHECK_OUT="$(PZ_TUNNEL_UPDATE_BASE_URL="$BUMP_MIRROR" "$P" update --check 2>&1)"
assert detect-reports-available contains "update is available" "$CHECK_OUT"

# --- apply -----------------------------------------------------------------
BEFORE_INODE="$(inode_of "$P")"
if PZ_TUNNEL_UPDATE_BASE_URL="$BUMP_MIRROR" "$P" update --force >/tmp/pz-autoupdate-apply.log 2>&1; then
    echo "PHASE=apply-exit-zero ok=true"
else
    echo "PHASE=apply-exit-zero ok=false"; fails=$((fails + 1))
fi
assert apply-binary-runs "$P" --version
AFTER_INODE="$(inode_of "$P")"
assert apply-swapped-binary test "$BEFORE_INODE" != "$AFTER_INODE"
assert apply-provenance test "$(sha256_of "$P")" = "$ARCHIVE_SHA"

# --- noop (up-to-date) -----------------------------------------------------
CURVER="$(version_of "$P")"
SAME_MIRROR="$(build_mirror "$EXE" "${CURVER#v}" "$TARGET")"
NOOP_INODE_BEFORE="$(inode_of "$P")"
NOOP_OUT="$(PZ_TUNNEL_UPDATE_BASE_URL="$SAME_MIRROR" "$P" update 2>&1)"
assert noop-reports-up-to-date contains "already up to date" "$NOOP_OUT"
assert noop-no-swap test "$(inode_of "$P")" = "$NOOP_INODE_BEFORE"

# --- crossver (gated real version advance) ---------------------------------
OLD="${PORTZERO_EXE_OLD:-}"
if [ -z "$OLD" ] || [ ! -x "$OLD" ]; then
    echo "PHASE=crossver skipped=no-prior-binary (set PORTZERO_EXE_OLD to exercise a real version advance)"
elif ! "$OLD" update --help >/dev/null 2>&1; then
    echo "PHASE=crossver skipped=prior-has-no-update (predates the self-updater; forward-compat contract not yet exercisable)"
else
    NEWVER="$(version_of "$EXE")"
    OLD_DIR="$(mktemp -d)"; P_OLD="$OLD_DIR/portzero"; cp "$OLD" "$P_OLD"; chmod +x "$P_OLD"
    NEW_MIRROR="$(build_mirror "$EXE" "${NEWVER#v}" "$TARGET")"
    PZ_TUNNEL_UPDATE_BASE_URL="$NEW_MIRROR" "$P_OLD" update --force >/tmp/pz-autoupdate-crossver.log 2>&1 || true
    # After the prior binary self-updates, it reports the new build's version.
    assert crossver-advanced test "$(version_of "$P_OLD")" = "${NEWVER#v}"
fi

if [ "$fails" -eq 0 ]; then
    echo "RESULT=PASS autoupdate (detect -> apply -> up-to-date no-op$([ -n "$OLD" ] && echo ' -> crossver'))"
    exit 0
fi
echo "RESULT=FAIL autoupdate assertions failed=$fails"
exit 1
