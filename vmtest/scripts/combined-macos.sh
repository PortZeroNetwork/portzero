#!/usr/bin/env bash
# COMBINED E2E (macOS): one VM session, one reset, covering everything the four
# old separate flavors (local-overlay, staging-tunnel, lifecycle, upgrade) used
# to check across four separate resets. Flow:
#
#   brew install/uninstall (delivery mechanism smoke)
#   -> setup (install) -> local-tunnel test -> upgrade -> cloud-tunnel test -> uninstall
#
# Concretely: exercise the real offline `brew install` of a locally-generated
# formula (delivery-mechanism smoke, independent of the upgrade tracking below),
# then run the PRIOR released version's privileged `setup` if one is available
# (else the new version's, skipping the upgrade step), prove the real overlay
# (.portzero.local via utun) works against whatever just got set up, run the NEW
# version's `setup` over the top and assert the upgrade duplicated nothing, prove
# the real cloud tunnel works against the now-installed new binary, then run the
# real uninstall and assert every artifact (keychain CA, LaunchDaemon, resolver,
# hosts pin, utun) is GONE.
#
# Output is greppable per the vmtest README: `PHASE=<name> ... ok=<true|false>`
# for every assertion, then a final `RESULT=PASS|FAIL`.
#
# Env:
#   PORTZERO_EXE       new (under-test) portzero binary. Defaults to the harness
#                      push location (see vm.sh push_binary_macos).
#   PORTZERO_EXE_OLD   prior-version binary to set up FIRST. If unset, fetches the
#                      latest published release's darwin tarball via `gh`. When
#                      neither is available the upgrade step SKIPs cleanly (never
#                      a false fail) and the local/cloud tunnel tests run against
#                      the new binary set up directly.
#   STAGING_SECRETS    path to the staging-e2e.env seed-token file for the
#                      cloud-tunnel phase (see vm.sh push_secrets_macos).
#   LIFECYCLE_SKIP_BREW=1  skip the offline brew-install phase entirely.
#   COMBINED_SKIP_CLOUD=1  skip the cloud-tunnel phase outright. Off by default
#                      — and the only supported way to skip it. With a seed
#                      token present, a staging that does not answer 200 FAILS
#                      the run.
# Helper predicates are invoked indirectly via assert/assert_not; silence the
# resulting spurious SC2317 "unreachable" info.
# shellcheck disable=SC2317
set -uo pipefail

PLIST=/Library/LaunchDaemons/cloud.portzero.daemon.plist
RESOLVER=/etc/resolver/portzero.local
HOSTS_PIN='# portzero-local'
CERT_CN='PortZero Local CA'
SYS_KEYCHAIN=/Library/Keychains/System.keychain
TUN_PREFIX=utun
CA_APP_SUPPORT="$HOME/Library/Application Support/PortZero/ca.crt"

# --- local-tunnel test fixtures ---
LOCAL_NAME=vmtestlocal
LOCAL_DOMAIN="$LOCAL_NAME.portzero.local"
LOCAL_BODY="portzero-local-overlay-ok"

# --- cloud-tunnel test fixtures ---
STAGING_DOMAIN=devenvtools.top
STAGING_SECRETS="${STAGING_SECRETS:-/tmp/portzero-vmtest/staging-e2e.env}"
STAGING_BODY="portzero-staging-tunnel-ok"

PORT=18080
LIB="$(cd "$(dirname "$0")" && pwd)/lib/http-echo.pl"
SUDO=""; [ "$(id -u)" -ne 0 ] && SUDO="sudo"
RH=/var/root

fails=0
assert() {
    local phase="$1"; shift
    if "$@" >/dev/null 2>&1; then echo "PHASE=$phase ok=true"
    else echo "PHASE=$phase ok=false"; fails=$((fails + 1)); fi
}
assert_not() {
    local phase="$1"; shift
    if "$@" >/dev/null 2>&1; then echo "PHASE=$phase ok=false"; fails=$((fails + 1))
    else echo "PHASE=$phase ok=true"; fi
}
# Like assert, but never counts as a failure: reports ok=true when the predicate
# holds, else ok=SKIP with a reason. Reserved for *runtime* states a headless
# `prlctl exec` session genuinely cannot establish on macOS (System-keychain
# trust settings need a securityd/GUI auth session; `launchctl bootstrap`/`load`
# into the system domain returns EIO with no bootstrap context) — the
# local-tunnel test below exercises the daemon running for real instead.
assert_or_skip() {
    local phase="$1" reason="$2"; shift 2
    if "$@" >/dev/null 2>&1; then echo "PHASE=$phase ok=true"
    else echo "PHASE=$phase ok=SKIP reason=\"$reason\""; fi
}

keychain_has_ca() { $SUDO security find-certificate -c "$CERT_CN" "$SYS_KEYCHAIN" >/dev/null 2>&1; }
keychain_ca_count() { # one block per matching cert, headed by a `keychain:` line
    $SUDO security find-certificate -a -c "$CERT_CN" "$SYS_KEYCHAIN" 2>/dev/null | grep -c 'keychain:' | tr -d ' '
}
launchdaemon_loaded() { $SUDO launchctl print system/cloud.portzero.daemon >/dev/null 2>&1; }
hosts_has_pin() { grep -qF "$HOSTS_PIN" /etc/hosts 2>/dev/null; }
hosts_pin_count() { local n; n="$(grep -cF "$HOSTS_PIN" /etc/hosts 2>/dev/null)"; echo "${n:-0}"; }
plist_count() { # a clean upgrade replaces the daemon in place; exactly one plist
    # shellcheck disable=SC2012
    ls /Library/LaunchDaemons/cloud.portzero.*.plist 2>/dev/null | wc -l | tr -d ' '
}
utun_present() {
    # A portzero utun carries the overlay gateway 10.254.0.1; match on that so we
    # don't trip over unrelated utun interfaces (VPNs etc.).
    ifconfig 2>/dev/null | grep -A3 "^${TUN_PREFIX}" | grep -q '10\.254\.0\.1'
}

# Bounded, non-interactive-safe, idempotent privileged install (== formula
# post_install + `sudo portzero setup`), guarded so a hung step can't wedge.
run_setup() { # <exe>
    local exe="$1"
    $SUDO env HOME="$HOME" "$exe" trust generate >/dev/null 2>&1 || true
    ( $SUDO env HOME="$HOME" "$exe" setup >/tmp/pz-setup.log 2>&1 ) & local sp=$!
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
    gh release download --repo PortZeroNetwork/portzero \
        --pattern "portzero-darwin-${tar_arch}.tar.gz" --dir "$out" >/dev/null 2>&1 || return 1
    local t; t="$(find "$out" -name '*.tar.gz' | head -1)"
    [ -n "$t" ] || return 1
    local ex="$out/x"; mkdir -p "$ex"
    tar xzf "$t" -C "$ex" >/dev/null 2>&1 || return 1
    local b; b="$(find "$ex" -name portzero -type f | head -1)"
    [ -n "$b" ] && { chmod +x "$b"; printf '%s\n' "$b"; return 0; }
    return 1
}

brew_caveats_mentions_setup() { brew info --formula "$1" 2>/dev/null | grep -q 'sudo portzero setup'; }

# --- BREW: real offline `brew install` of a locally-generated formula --------
# Exercises the actual delivery mechanism (formula install + post_install) with
# zero network, using the host-built (new) binary. Independent of the prior/new
# upgrade tracking below. SKIPs cleanly when Homebrew is absent from the golden
# snapshot.
run_brew_phase() { # <new-exe>
    local exe="$1"
    if [ "${LIFECYCLE_SKIP_BREW:-0}" = 1 ]; then
        echo 'PHASE=brew-install ok=SKIP reason="LIFECYCLE_SKIP_BREW=1"'
        return
    fi
    if ! command -v brew >/dev/null 2>&1; then
        echo 'PHASE=brew-install ok=SKIP reason="Homebrew not installed in golden snapshot"'
        return
    fi

    local arch tar_arch work tarball formula sha prefix
    arch="$(uname -m)"
    case "$arch" in arm64|aarch64) tar_arch="arm64" ;; *) tar_arch="amd64" ;; esac
    work="$(mktemp -d /tmp/pz-brew.XXXXXX)"
    tarball="$work/portzero-darwin-${tar_arch}.tar.gz"

    local stage="$work/portzero-darwin-${tar_arch}"
    mkdir -p "$stage"
    cp "$exe" "$stage/portzero"
    chmod +x "$stage/portzero"
    ( cd "$work" && tar czf "$tarball" "portzero-darwin-${tar_arch}" )

    sha="$(shasum -a 256 "$tarball" | awk '{print $1}')"

    formula="$work/portzero.rb"
    cat > "$formula" <<EOF
class Portzero < Formula
  desc "PortZero local development overlay (vmtest offline lifecycle build)"
  homepage "https://portzero.cloud"
  version "0.0.0-vmtest"
  license "GPL-3.0-or-later"

  url "file://$tarball"
  sha256 "$sha"

  def install
    bin.install "portzero"
  end

  def post_install
    system "#{bin}/portzero", "trust", "generate"
  end

  def caveats
    <<~CAVEAT
      To complete setup, run:

        sudo portzero setup
    CAVEAT
  end
end
EOF

    if command -v ruby >/dev/null 2>&1; then
        assert brew-formula-ruby-valid ruby -c "$formula"
    fi

    export HOMEBREW_NO_AUTO_UPDATE=1
    export HOMEBREW_NO_INSTALL_FROM_API=1
    export HOMEBREW_NO_ANALYTICS=1
    export HOMEBREW_NO_INSTALL_UPGRADE=1
    export HOMEBREW_NO_INSTALLED_DEPENDENTS_CHECK=1

    echo ">> brew install --formula (offline, generated formula)"
    rm -f "$CA_APP_SUPPORT" 2>/dev/null || true
    local ilog="$work/brew-install.log"
    brew install --formula "$formula" >"$ilog" 2>&1 || true

    prefix="$(brew --prefix 2>/dev/null)"
    assert brew-install-binary        test -x "$prefix/bin/portzero"
    assert brew-install-on-path       command -v portzero
    assert brew-post-install-ca       test -f "$CA_APP_SUPPORT"
    assert brew-caveats-setup         brew_caveats_mentions_setup "$formula"

    echo ">> brew uninstall --formula"
    brew uninstall --formula portzero >/dev/null 2>&1 || true
    assert brew-uninstall-binary-gone test ! -e "$prefix/bin/portzero"

    rm -rf "$work" 2>/dev/null || true
}

# --- local-tunnel helper: serve a tagged echo, start the daemon, assert the
# .portzero.local domain resolves+serves through the overlay directly, then
# stop the daemon. Local-only: a cloud tunnel's public domain isn't reachable
# via a same-host http:// poll the way the overlay intercepts .portzero.local
# (it needs route-registration + approval first, and only serves over the real
# public HTTPS edge) — see run_cloud_tunnel_test below for that flow.
run_tunnel_test() { # <phase-prefix> <exe> <tunnel-env-value> <domain-to-check> <expected-body>
    local prefix="$1" exe="$2" tunnel_env="$3" domain="$4" body="$5"
    local work; work="$(mktemp -d)"
    local svc_pid=""

    # macOS caveat (task-74): a SIP-protected system binary like /usr/bin/perl has
    # its env hidden from every other process, so run a *copy* at an unrestricted
    # path (whose env IS readable) instead of system perl.
    local perl_copy="$work/perl"
    cp "$(command -v perl)" "$perl_copy" && chmod +x "$perl_copy"

    $SUDO env PZ_TUNNEL="$tunnel_env" "$perl_copy" "$LIB" "$body" "$PORT" >"$work/svc.out" 2>&1 &
    svc_pid=$!
    local up=0
    for _ in $(seq 1 30); do curl -sf "http://127.0.0.1:$PORT/" >/dev/null 2>&1 && { up=1; break; }; sleep 0.5; done
    if [ "$up" != 1 ]; then
        echo "PHASE=$prefix-service ok=false"
        fails=$((fails + 1))
        kill -9 "$svc_pid" >/dev/null 2>&1
        rm -rf "$work"
        return
    fi
    echo "PHASE=$prefix-service ok=true port=$PORT tunnel=$domain"

    $SUDO env HOME="$RH" "$exe" start --no-browser >"$work/start.out" 2>&1
    echo "PHASE=$prefix-daemon-launched ok=true"

    local ok=0
    for _ in $(seq 1 60); do
        local b; b="$(curl -sf --max-time 5 "http://$domain/" 2>/dev/null)"
        [ "$b" = "$body" ] && { ok=1; break; }
        sleep 2
    done
    if [ "$ok" = 1 ]; then
        echo "PHASE=$prefix-tunnel ok=true domain=$domain"
    else
        echo "PHASE=$prefix-tunnel ok=false domain=$domain"
        for d in "$HOME/.portzero" "$RH/.portzero"; do
            [ -f "$d/daemon/daemon.log" ] && { $SUDO tail -8 "$d/daemon/daemon.log"; break; }
        done
        fails=$((fails + 1))
    fi

    # Bounded stop, then force-kill — never let cleanup wedge the rest of the run.
    ( $SUDO env HOME="$RH" "$exe" stop >/dev/null 2>&1 ) & local sp=$!
    ( sleep 10; kill -9 "$sp" 2>/dev/null ) >/dev/null 2>&1 &
    wait "$sp" 2>/dev/null
    kill -9 "$svc_pid" >/dev/null 2>&1
    $SUDO pkill -9 -f http-echo.pl >/dev/null 2>&1
    rm -rf "$work"
}

# --- cloud-tunnel helper: serve a tagged echo, start the daemon pointed at
# staging, wait for the route to actually REGISTER via the cloud API (the
# real readiness gate — the daemon connects out to the edge asynchronously),
# approve it, then fetch the real public HTTPS URL. Keeps the daemon alive
# through approval + fetch and stops it exactly once at the end.
run_cloud_tunnel_test() { # <exe> <tunnel> <token>
    local exe="$1" tunnel="$2" token="$3"
    local work; work="$(mktemp -d)"
    local svc_pid=""

    local perl_copy="$work/perl"
    cp "$(command -v perl)" "$perl_copy" && chmod +x "$perl_copy"

    $SUDO env PZ_TUNNEL="${tunnel}:80" "$perl_copy" "$LIB" "$STAGING_BODY" "$PORT" >"$work/svc.out" 2>&1 &
    svc_pid=$!
    local up=0
    for _ in $(seq 1 30); do curl -sf "http://127.0.0.1:$PORT/" >/dev/null 2>&1 && { up=1; break; }; sleep 0.5; done
    if [ "$up" != 1 ]; then
        echo "PHASE=cloud-service ok=false"
        fails=$((fails + 1))
        kill -9 "$svc_pid" >/dev/null 2>&1
        rm -rf "$work"
        return
    fi
    echo "PHASE=cloud-service ok=true port=$PORT tunnel=$tunnel"

    $SUDO env HOME="$RH" PZ_TUNNEL_API_URL="$API" PZ_TUNNEL_EDGE_URL="$EDGE" PZ_TUNNEL_BASE_DOMAIN="$STAGING_DOMAIN" \
        "$exe" start --no-browser >"$work/start.out" 2>&1
    echo "PHASE=cloud-daemon-launched ok=true"

    local reg=0
    for _ in $(seq 1 45); do
        curl -fsS -H "Authorization: Bearer $token" "$API/routes" 2>/dev/null | grep -q "\"$tunnel\"" && { reg=1; break; }
        sleep 2
    done
    if [ "$reg" != 1 ]; then
        echo "PHASE=cloud-route-registered ok=false"
        for d in "$HOME/.portzero" "$RH/.portzero"; do
            [ -f "$d/daemon/daemon.log" ] && { $SUDO tail -8 "$d/daemon/daemon.log"; break; }
        done
        fails=$((fails + 1))
    else
        echo "PHASE=cloud-route-registered ok=true"
        curl -fsS -X POST -H "Authorization: Bearer $token" -H 'Content-Type: application/json' -d '{}' \
            "$API/routes/$tunnel/approve" >/dev/null 2>&1
        local ok=0
        for _ in $(seq 1 30); do
            local b; b="$(curl -fsS --max-time 10 "https://$tunnel/" 2>/dev/null)"
            [ "$b" = "$STAGING_BODY" ] && { ok=1; break; }
            sleep 2
        done
        assert cloud-public-url test "$ok" = 1
    fi

    # Bounded stop, then force-kill — never let cleanup wedge the rest of the run.
    ( $SUDO env HOME="$RH" PZ_TUNNEL_API_URL="$API" PZ_TUNNEL_EDGE_URL="$EDGE" PZ_TUNNEL_BASE_DOMAIN="$STAGING_DOMAIN" \
        "$exe" stop >/dev/null 2>&1 ) & local sp=$!
    ( sleep 10; kill -9 "$sp" 2>/dev/null ) >/dev/null 2>&1 &
    wait "$sp" 2>/dev/null
    kill -9 "$svc_pid" >/dev/null 2>&1
    $SUDO pkill -9 -f http-echo.pl >/dev/null 2>&1
    rm -rf "$work"
}

# =============================================================================
NEW="${PORTZERO_EXE:-/tmp/portzero-vmtest/bin/portzero}"
OLD="${PORTZERO_EXE_OLD:-}"
[ -z "$OLD" ] && OLD="$(fetch_prior_binary || true)"
echo "PHASE=preflight new=$([ -x "$NEW" ] && echo "$NEW" || echo none) old=${OLD:-none}"
if [ -z "$NEW" ] || [ ! -x "$NEW" ]; then
    echo "RESULT=FAIL new portzero binary not found at $NEW (set PORTZERO_EXE)"
    exit 1
fi
HAVE_UPGRADE=0
if [ -n "$OLD" ] && [ -x "$OLD" ]; then HAVE_UPGRADE=1; fi

# --- 1. BREW delivery-mechanism smoke (independent of prior/new tracking) --
run_brew_phase "$NEW"

# --- 2. INSTALL (setup: prior version if we have one, else new) -----------
FIRST="$NEW"; [ "$HAVE_UPGRADE" = 1 ] && FIRST="$OLD"
echo ">> portzero setup ($FIRST)"
run_setup "$FIRST"
assert install-trust-keychain keychain_has_ca
assert install-launchdaemon-plist test -f "$PLIST"
assert_or_skip install-launchdaemon-loaded \
    "launchctl bootstrap/load into the system domain returns EIO under headless prlctl exec; daemon-run state is covered by the local-tunnel test below" \
    launchdaemon_loaded
assert install-resolver test -f "$RESOLVER"
assert install-hosts-pin hosts_has_pin

# --- 3. LOCAL TUNNEL TEST (real utun + scoped DNS + local proxy) ----------
echo ">> local-tunnel test against $FIRST"
for d in "$HOME/.portzero" "$RH/.portzero"; do $SUDO rm -f "$d/auth.json" 2>/dev/null; done
run_tunnel_test local "$FIRST" "$LOCAL_DOMAIN" "$LOCAL_DOMAIN" "$LOCAL_BODY"

# --- 4. UPGRADE (new setup over the top; assert nothing duplicated) -------
if [ "$HAVE_UPGRADE" = 1 ]; then
    assert prior-single-hosts-pin  test "$(hosts_pin_count)" = 1
    assert prior-single-keychain-ca test "$(keychain_ca_count)" = 1
    assert prior-single-plist      test "$(plist_count)" = 1
    assert prior-resolver          test -f "$RESOLVER"

    echo ">> upgrading to new version ($NEW)"
    run_setup "$NEW"
    assert upgrade-single-hosts-pin   test "$(hosts_pin_count)" = 1
    assert upgrade-single-keychain-ca test "$(keychain_ca_count)" = 1
    assert upgrade-single-plist       test "$(plist_count)" = 1
    assert upgrade-resolver           test -f "$RESOLVER"
    assert upgrade-daemon-is-new      grep -qF "$NEW" "$PLIST"
else
    echo "PHASE=upgrade skipped=no-prior-artifact (set PORTZERO_EXE_OLD or provide network+gh)"
fi

# --- 5. CLOUD TUNNEL TEST (real staging tunnel, against the new binary) ---
if [ "${COMBINED_SKIP_CLOUD:-0}" = 1 ]; then
    echo "PHASE=cloud-tunnel ok=SKIP reason=\"COMBINED_SKIP_CLOUD=1\""
else
    SEED="$(grep -E '^TEST_LOGIN_SEED_TOKEN=' "$STAGING_SECRETS" 2>/dev/null | sed 's/^[^=]*=//')"
    if [ -z "$SEED" ]; then
        echo "PHASE=cloud-tunnel ok=SKIP reason=\"no seed token at $STAGING_SECRETS\""
    else
        # Probe staging up to three times before believing it is down: this
        # guest shares one physical host with the others (see vm-e2e.yml), so a
        # single 10s curl can lose to a transient blip.
        st=000
        probes=0
        while :; do
            st="$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 "https://app.$STAGING_DOMAIN/" 2>/dev/null)"
            [ "$st" = 200 ] && break
            probes=$((probes + 1))
            [ "$probes" -ge 3 ] && break
            sleep 5
        done
        if [ "$st" != 200 ]; then
            # This used to SKIP, which turned an unreachable staging into a
            # PASS with the whole cloud leg silently unrun — a green build that
            # proved nothing about the cloud path. Not reaching staging is a
            # failure; the deliberate opt-out is COMBINED_SKIP_CLOUD=1.
            echo "PHASE=cloud-reachable ok=false http=$st url=https://app.$STAGING_DOMAIN/"
            echo ">> staging did not answer 200, so the cloud-tunnel test could not run."
            if [ "$st" = 000 ]; then
                echo ">>   HTTP 000 means no response at all: this guest has no egress, DNS"
                echo ">>   failed, or the TLS handshake was rejected. Usually the guest's"
                echo ">>   network adapter, not staging."
            else
                echo ">>   staging answered $st, so it is up but unhealthy."
            fi
            echo ">> next steps:"
            echo ">>   - curl https://app.$STAGING_DOMAIN/ from the host; if that works, fix the guest's network"
            echo ">>   - if it fails there too, check the Deploy Staging workflow in portzero-cloud"
            echo ">>   - to run without the cloud leg on purpose, re-run with COMBINED_SKIP_CLOUD=1"
            fails=$((fails + 1))
        else
            SUFFIX="$(date +%m%d%H%M%S)"
            USERNAME="vmtestdar${SUFFIX}"
            EMAIL="vmtest-dar-${SUFFIX}@example.com"
            ACCOUNT="vmtest-dar-${SUFFIX}"
            CODE=424242
            API="https://app.$STAGING_DOMAIN/api"
            EDGE="wss://edge.$STAGING_DOMAIN/tunnel"
            TUNNEL="${USERNAME}.tunnel.$STAGING_DOMAIN"
            json() { grep -oE "\"$1\":\"[^\"]*\"" | sed "s/^\"$1\":\"//;s/\"\$//" | head -1; }

            curl -fsS -H "Authorization: Bearer $SEED" -H 'Content-Type: application/json' \
                -d "{\"email\":\"$EMAIL\",\"username\":\"$USERNAME\",\"account_id\":\"$ACCOUNT\",\"code\":\"$CODE\"}" \
                "$API/auth/test-seed-login" >/dev/null 2>&1
            VER="$(curl -fsS -H 'Content-Type: application/json' -d "{\"email\":\"$EMAIL\",\"code\":\"$CODE\"}" "$API/auth/verify" 2>/dev/null)"
            TOKEN="$(printf '%s' "$VER" | json token)"
            if [ -z "$TOKEN" ]; then
                echo "PHASE=cloud-auth ok=false"
                fails=$((fails + 1))
            else
                echo "PHASE=cloud-auth ok=true user=$USERNAME"
                $SUDO mkdir -p "$RH/.portzero"
                printf '%s' "$VER" | $SUDO tee "$RH/.portzero/auth.json" >/dev/null

                run_cloud_tunnel_test "$NEW" "$TUNNEL" "$TOKEN"

                for d in "$HOME/.portzero" "$RH/.portzero"; do $SUDO rm -f "$d/auth.json" 2>/dev/null; done
            fi
        fi
    fi
fi

# --- 6. UNINSTALL (the way the brew caveats / a user removes it) ----------
echo ">> uninstall (trust uninstall + autostart disable + resolver + hosts)"
$SUDO env HOME="$HOME" "$NEW" stop >/dev/null 2>&1 || true
$SUDO env HOME="$HOME" "$NEW" trust uninstall >/dev/null 2>&1 || true
$SUDO env HOME="$HOME" "$NEW" autostart disable >/dev/null 2>&1 || true
$SUDO rm -f "$RESOLVER" 2>/dev/null || true
if hosts_has_pin; then
    tmp="$(mktemp)"; grep -vF "$HOSTS_PIN" /etc/hosts > "$tmp" 2>/dev/null || true
    $SUDO install -m 0644 "$tmp" /etc/hosts 2>/dev/null || true
    rm -f "$tmp"
fi
sleep 2

# --- ASSERT CLEAN: every install artifact must be GONE ---------------------
assert_not clean-trust-keychain keychain_has_ca
assert_not clean-launchdaemon-plist test -e "$PLIST"
assert_not clean-launchdaemon-loaded launchdaemon_loaded
assert_not clean-resolver test -e "$RESOLVER"
assert_not clean-hosts-pin hosts_has_pin
assert_not clean-tun utun_present

if [ "$fails" -eq 0 ]; then
    echo "RESULT=PASS combined (brew -> setup -> local-tunnel -> upgrade -> cloud-tunnel -> uninstall -> assert-clean)"
    exit 0
fi
echo "RESULT=FAIL combined assertions failed=$fails"
exit 1
