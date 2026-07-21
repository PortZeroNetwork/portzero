#!/usr/bin/env bash
# UPGRADE E2E (macOS): install a PRIOR version's privileged setup, then a NEWER
# version's, and assert the upgrade is clean — no DUPLICATED LaunchDaemon,
# /etc/resolver/portzero.local resolver, System-keychain CA, or /etc/hosts pin,
# and the installed daemon is the NEW binary. Upgrade-from-prior-version was
# untested on macOS; a setup flow that appends instead of replacing is exactly
# how you end up with two `# portzero-local` hosts lines, two keychain CAs, or a
# second cloud.portzero.* LaunchDaemon.
#
# macOS ships via Homebrew (release tar.gz + tap formula), so "install a version"
# here == run that version's binary through `trust generate` + `sudo setup` — the
# exact privileged flow the formula's post_install + caveats drive. The prior
# binary comes from a prior release tarball (via gh) or a pinned path; the new
# binary is the host-built one the harness pushes in.
#
# Output is greppable: `PHASE=<name> ... ok=<true|false>`, then RESULT=PASS|FAIL.
#
# Env:
#   PORTZERO_EXE      new (under-test) portzero binary. Defaults to the harness
#                     push location, matching lifecycle-macos.sh.
#   PORTZERO_EXE_OLD  prior-version binary to upgrade FROM. If unset, fetches the
#                     latest published release's darwin tarball via gh and uses
#                     its binary. SKIPs cleanly when neither is available.
#
# In-sandbox / offline limitation: fetching a genuinely-prior RELEASE build needs
# GitHub Releases access + gh. When PORTZERO_EXE_OLD is unset and no network/gh
# is available (the pristine macOS guest), this SKIPS with a clear reason rather
# than pretending to pass — never a false fail.
# Helper predicates are invoked indirectly via assert; silence spurious SC2317.
# shellcheck disable=SC2317
set -uo pipefail

EXE="${PORTZERO_EXE:-/tmp/portzero-vmtest/bin/portzero}"
PLIST=/Library/LaunchDaemons/cloud.portzero.daemon.plist
PLIST_GLOB='/Library/LaunchDaemons/cloud.portzero.'
RESOLVER=/etc/resolver/portzero.local
HOSTS_PIN='# portzero-local'
CERT_CN='PortZero Local CA'
SYS_KEYCHAIN=/Library/Keychains/System.keychain

fails=0
SUDO=""; [ "$(id -u)" -ne 0 ] && SUDO="sudo"

assert() {
    local phase="$1"; shift
    if "$@" >/dev/null 2>&1; then echo "PHASE=$phase ok=true"
    else echo "PHASE=$phase ok=false"; fails=$((fails + 1)); fi
}

hosts_pin_count() { local n; n="$(grep -cF "$HOSTS_PIN" /etc/hosts 2>/dev/null)"; echo "${n:-0}"; }
# One block per matching cert; each entry is headed by a `keychain:` line.
keychain_ca_count() {
    $SUDO security find-certificate -a -c "$CERT_CN" "$SYS_KEYCHAIN" 2>/dev/null \
        | grep -c 'keychain:' | tr -d ' '
}
# A clean upgrade replaces the daemon in place; there must be exactly one
# cloud.portzero.* LaunchDaemon plist, not a stale second one.
plist_count() {
    # shellcheck disable=SC2012
    ls "${PLIST_GLOB}"*.plist 2>/dev/null | wc -l | tr -d ' '
}
# Bounded, non-interactive-safe, idempotent privileged install (== formula
# post_install + `sudo portzero setup`), guarded so a hung step can't wedge.
run_setup() { # <exe>
    local exe="$1"
    $SUDO env HOME="$HOME" "$exe" trust generate >/dev/null 2>&1 || true
    ( $SUDO env HOME="$HOME" "$exe" setup >/tmp/pz-upgrade-setup.log 2>&1 ) & local sp=$!
    ( sleep 120; kill -9 "$sp" 2>/dev/null ) >/dev/null 2>&1 & local killer=$!
    wait "$sp" 2>/dev/null; kill "$killer" 2>/dev/null
}

# Best-effort fetch of the latest published release's darwin tarball, extracting
# its portzero binary to use as the "prior" version.
fetch_prior_binary() {
    command -v gh >/dev/null 2>&1 || return 1
    local arch tar_arch; arch="$(uname -m)"
    case "$arch" in arm64|aarch64) tar_arch=arm64 ;; *) tar_arch=amd64 ;; esac
    local out; out="$(mktemp -d)"
    gh release download --repo PortZeroNetwork/portzero-local \
        --pattern "portzero-darwin-${tar_arch}.tar.gz" --dir "$out" >/dev/null 2>&1 || return 1
    local t; t="$(find "$out" -name '*.tar.gz' | head -1)"
    [ -n "$t" ] || return 1
    local ex="$out/x"; mkdir -p "$ex"
    tar xzf "$t" -C "$ex" >/dev/null 2>&1 || return 1
    local b; b="$(find "$ex" -name portzero -type f | head -1)"
    [ -n "$b" ] && { chmod +x "$b"; printf '%s\n' "$b"; return 0; }
    return 1
}

NEW="$EXE"
OLD="${PORTZERO_EXE_OLD:-}"
[ -z "$OLD" ] && OLD="$(fetch_prior_binary || true)"

echo "PHASE=preflight new=$([ -x "$NEW" ] && echo "$NEW" || echo none) old=${OLD:-none}"
if [ -z "$NEW" ] || [ ! -x "$NEW" ]; then
    echo "RESULT=FAIL new portzero binary not found at $NEW (set PORTZERO_EXE)"
    exit 1
fi
if [ -z "$OLD" ] || [ ! -x "$OLD" ]; then
    echo "PHASE=prior-version skipped=unavailable (set PORTZERO_EXE_OLD or provide network+gh)"
    echo "RESULT=SKIP upgrade (no prior-version binary to upgrade FROM)"
    exit 0
fi

echo ">> installing prior version ($OLD)"
run_setup "$OLD"
assert prior-single-hosts-pin  test "$(hosts_pin_count)" = 1
assert prior-single-keychain-ca test "$(keychain_ca_count)" = 1
assert prior-single-plist      test "$(plist_count)" = 1
assert prior-resolver          test -f "$RESOLVER"

echo ">> upgrading to new version ($NEW)"
run_setup "$NEW"
# The core upgrade invariants: nothing got DUPLICATED.
assert upgrade-single-hosts-pin   test "$(hosts_pin_count)" = 1
assert upgrade-single-keychain-ca test "$(keychain_ca_count)" = 1
assert upgrade-single-plist       test "$(plist_count)" = 1
assert upgrade-resolver           test -f "$RESOLVER"
# The installed daemon is the NEW binary: its LaunchDaemon references the new
# path, not the prior one (OLD and NEW are staged at distinct paths).
assert upgrade-daemon-is-new      grep -qF "$NEW" "$PLIST"

echo ">> cleaning up (uninstall the upgraded install)"
$SUDO env HOME="$HOME" "$NEW" stop >/dev/null 2>&1 || true
$SUDO env HOME="$HOME" "$NEW" trust uninstall >/dev/null 2>&1 || true
$SUDO env HOME="$HOME" "$NEW" autostart disable >/dev/null 2>&1 || true
$SUDO rm -f "$RESOLVER" 2>/dev/null || true
if grep -qF "$HOSTS_PIN" /etc/hosts 2>/dev/null; then
    tmp="$(mktemp)"; grep -vF "$HOSTS_PIN" /etc/hosts > "$tmp" 2>/dev/null || true
    $SUDO install -m 0644 "$tmp" /etc/hosts 2>/dev/null || true
    rm -f "$tmp"
fi
assert upgrade-clean-hosts-pin test "$(hosts_pin_count)" = 0

if [ "$fails" -eq 0 ]; then
    echo "RESULT=PASS upgrade (prior -> new, no duplicated daemon/resolver/CA/hosts pin)"
    exit 0
fi
echo "RESULT=FAIL upgrade assertions failed=$fails"
exit 1
