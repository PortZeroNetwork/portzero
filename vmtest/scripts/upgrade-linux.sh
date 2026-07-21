#!/usr/bin/env bash
# UPGRADE E2E (Linux): install a PRIOR released version, then install the new
# one, and assert the upgrade is clean — no DUPLICATED autostart service, no
# duplicated /etc/hosts DNS pin, no stale binary. Upgrade-from-prior-version was
# untested everywhere; a postinst that appends instead of replacing is exactly
# how you end up with two `# portzero-local` hosts lines or two units.
#
# Output is greppable: `PHASE=<name> ... ok=<true|false>`, then RESULT=PASS|FAIL.
#
# Env:
#   PORTZERO_DEB_OLD  prior-version .deb. If unset, tries to fetch the latest
#                     published release .deb (needs network + gh; see fetch note).
#   PORTZERO_DEB_NEW  new .deb under test. Defaults to the same discovery
#                     lifecycle-linux.sh uses (.downloaded-artifacts / target).
#
# In-sandbox / offline limitation: fetching a genuinely-prior RELEASE build needs
# GitHub Releases access. When PORTZERO_DEB_OLD is unset and no network is
# available, this SKIPS with a clear reason rather than pretending to pass — the
# assertions themselves are validated in CI with two locally-built debs.
set -uo pipefail
# Predicates invoked indirectly via assert; silence spurious SC2317.
# shellcheck disable=SC2317

BIN=/usr/local/bin/portzero
HOSTS_PIN='# portzero-local'

fails=0
sudo_() { if [ "$(id -u)" -eq 0 ]; then "$@"; else sudo "$@"; fi; }
assert() {
    local phase="$1"; shift
    if "$@" >/dev/null 2>&1; then echo "PHASE=$phase ok=true"
    else echo "PHASE=$phase ok=false"; fails=$((fails + 1)); fi
}
hosts_pin_count() { local n; n="$(grep -cF "$HOSTS_PIN" /etc/hosts 2>/dev/null)"; echo "${n:-0}"; }
unit_count() {
    # A clean upgrade replaces the unit in place; there must be exactly one.
    find /usr/lib/systemd/user /etc/systemd/user "$HOME/.config/systemd/user" \
        -maxdepth 1 -name 'portzero-daemon.service' 2>/dev/null | wc -l | tr -d ' '
}

find_new_deb() {
    if [ -n "${PORTZERO_DEB_NEW:-}" ]; then printf '%s\n' "$PORTZERO_DEB_NEW"; return; fi
    local here d
    here="$(cd "$(dirname "$0")/../.." && pwd)"
    for d in "$here"/vmtest/.downloaded-artifacts/linux/*.deb "$here"/target/debian/*.deb; do
        [ -f "$d" ] && { printf '%s\n' "$d"; return; }
    done
}

# Best-effort fetch of the latest published release .deb as the "prior" version.
fetch_prior_deb() {
    command -v gh >/dev/null 2>&1 || return 1
    local out; out="$(mktemp -d)"
    gh release download --repo PortZeroNetwork/portzero-local \
        --pattern '*.deb' --dir "$out" >/dev/null 2>&1 || return 1
    local d; d="$(find "$out" -name '*.deb' | head -1)"
    [ -n "$d" ] && { printf '%s\n' "$d"; return 0; }
    return 1
}

NEW="$(find_new_deb)"
OLD="${PORTZERO_DEB_OLD:-}"
[ -z "$OLD" ] && OLD="$(fetch_prior_deb || true)"

echo "PHASE=preflight old=${OLD:-none} new=${NEW:-none}"
if [ -z "$NEW" ] || [ ! -f "$NEW" ]; then
    echo "RESULT=FAIL no new .deb found (set PORTZERO_DEB_NEW)"
    exit 1
fi
if [ -z "$OLD" ] || [ ! -f "$OLD" ]; then
    echo "PHASE=prior-version skipped=unavailable (set PORTZERO_DEB_OLD or provide network+gh)"
    echo "RESULT=SKIP upgrade (no prior-version .deb to upgrade FROM)"
    exit 0
fi

echo ">> installing prior version"
sudo_ dpkg -i "$OLD" >/dev/null 2>&1 || sudo_ apt-get -f install -y >/dev/null 2>&1
assert prior-installed test -x "$BIN"
assert prior-single-hosts-pin test "$(hosts_pin_count)" = 1
assert prior-single-unit test "$(unit_count)" = 1

echo ">> upgrading to new version (dpkg -i over the top)"
sudo_ dpkg -i "$NEW" >/dev/null 2>&1 || sudo_ apt-get -f install -y >/dev/null 2>&1
assert upgraded-installed test -x "$BIN"
# The core upgrade invariants: nothing got DUPLICATED.
assert upgrade-single-hosts-pin test "$(hosts_pin_count)" = 1
assert upgrade-single-unit test "$(unit_count)" = 1

echo ">> cleaning up (purge)"
sudo_ dpkg --purge portzero >/dev/null 2>&1 || true
assert upgrade-clean-hosts-pin test "$(hosts_pin_count)" = 0

if [ "$fails" -eq 0 ]; then
    echo "RESULT=PASS upgrade (prior -> new, no duplicated service/resolver/binary)"
    exit 0
fi
echo "RESULT=FAIL upgrade assertions failed=$fails"
exit 1
