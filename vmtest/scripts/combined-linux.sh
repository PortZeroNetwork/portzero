#!/usr/bin/env bash
# COMBINED E2E (Linux): one VM session, one reset, covering everything the four
# old separate flavors (local-overlay, staging-tunnel, lifecycle, upgrade) used
# to check across four separate resets. Flow:
#
#   install -> local-tunnel test -> upgrade -> cloud-tunnel test -> uninstall
#
# Concretely: install the PRIOR released .deb if one is available (else install
# the new .deb directly and skip the upgrade step), prove the real overlay
# (.portzero.local via TUN) works against whatever just got installed, install
# the NEW .deb over the top and assert the upgrade duplicated nothing, prove the
# real cloud tunnel works against the now-installed new binary, then uninstall
# and assert every artifact (binary, caps, hosts pin, systemd unit, trust-store
# CA, autostart) is GONE.
#
# Output is greppable per the vmtest README: `PHASE=<name> ... ok=<true|false>`
# for every assertion, then a final `RESULT=PASS|FAIL`.
#
# Env:
#   PORTZERO_DEB       new .deb under test. If unset, searches
#                      vmtest/.downloaded-artifacts/linux/*.deb then target/debian/*.deb.
#   PORTZERO_DEB_OLD   prior-version .deb to install FIRST. If unset, tries to
#                      fetch the latest published release .deb via `gh`. When
#                      neither is available the upgrade step SKIPs cleanly (never
#                      a false fail) and the local/cloud tunnel tests run against
#                      the new .deb installed directly.
#   STAGING_SECRETS    path to the staging-e2e.env seed-token file for the
#                      cloud-tunnel phase. Defaults to the same MBP-Sidecar share
#                      path e2e-staging-tunnel.sh uses.
#   COMBINED_SKIP_CLOUD=1  skip the cloud-tunnel phase outright (e.g. a
#                      metered/offline session). Off by default.
# Helper predicates below are invoked indirectly (their names are passed to
# `assert`/`assert_not` as commands), which defeats shellcheck's reachability
# heuristic and yields spurious SC2317 "unreachable" info for every one of them.
# shellcheck disable=SC2317
set -uo pipefail

BIN=/usr/local/bin/portzero
SYSTEM_UNIT=/usr/lib/systemd/user/portzero-daemon.service
USER_UNIT="${HOME}/.config/systemd/user/portzero-daemon.service"
HOSTS_PIN='# portzero-local'
# Trust-store destinations, mirroring client/crates/daemon/src/tls/trust.rs.
CA_DEBIAN=/usr/share/ca-certificates/portzero/portzero-local-ca.crt
CA_SSL_PEM=/etc/ssl/certs/portzero-local-ca.pem
CA_RHEL=/etc/ca-certificates/trust-source/anchors/portzero-local-ca.crt
CA_CONFIG=/etc/ca-certificates.conf
CA_CONFIG_LINE='portzero/portzero-local-ca.crt'
CA_BUNDLE=/etc/ssl/certs/ca-certificates.crt
CA_SUBJECT_CN='PortZero Local CA'

# --- local-tunnel test fixtures ---
LOCAL_NAME=vmtestlocal
LOCAL_DOMAIN="$LOCAL_NAME.portzero.local"
LOCAL_BODY="portzero-local-overlay-ok"

# --- cloud-tunnel test fixtures ---
STAGING_DOMAIN=devenvtools.top
STAGING_SECRETS="${STAGING_SECRETS:-/media/psf/MBP-Sidecar/loumtech/vm-toolchain-cache/common/staging-e2e.env}"
STAGING_BODY="portzero-staging-tunnel-ok"

PORT=18080
LIB="$(cd "$(dirname "$0")" && pwd)/lib/http-echo.pl"

fails=0
sudo_() { if [ "$(id -u)" -eq 0 ]; then "$@"; else sudo "$@"; fi; }

# assert <phase> <condition-cmd...> : run the command, print a greppable line,
# and bump the failure counter when the condition is false.
assert() {
    local phase="$1"; shift
    if "$@" >/dev/null 2>&1; then
        echo "PHASE=$phase ok=true"
    else
        echo "PHASE=$phase ok=false"
        fails=$((fails + 1))
    fi
}
# assert_not <phase> <condition-cmd...> : inverse — the condition must be FALSE
# (used for "this artifact must be GONE" assertions).
assert_not() {
    local phase="$1"; shift
    if "$@" >/dev/null 2>&1; then
        echo "PHASE=$phase ok=false"
        fails=$((fails + 1))
    else
        echo "PHASE=$phase ok=true"
    fi
}

has_cap() { # <cap-substring>
    local caps; caps="$(getcap "$BIN" 2>/dev/null || true)"
    case "$caps" in *"$1"*) return 0 ;; *) return 1 ;; esac
}
hosts_has_pin() { grep -qF "$HOSTS_PIN" /etc/hosts 2>/dev/null; }
hosts_pin_count() { local n; n="$(grep -cF "$HOSTS_PIN" /etc/hosts 2>/dev/null)"; echo "${n:-0}"; }
unit_count() { # A clean upgrade replaces the unit in place; there must be exactly one.
    find /usr/lib/systemd/user /etc/systemd/user "$HOME/.config/systemd/user" \
        -maxdepth 1 -name 'portzero-daemon.service' 2>/dev/null | wc -l | tr -d ' '
}
config_has_ca_line() { grep -qxF "$CA_CONFIG_LINE" "$CA_CONFIG" 2>/dev/null; }
bundle_has_ca() {
    # The distro bundle is the concatenation update-ca-certificates produces;
    # our anchor's CN must appear in a parsed cert, not merely as a substring.
    command -v openssl >/dev/null 2>&1 || return 2
    [ -r "$CA_BUNDLE" ] || return 1
    openssl storeutl -noout -certs "$CA_BUNDLE" 2>/dev/null | grep -qF "$CA_SUBJECT_CN" \
        || openssl crl2pkcs7 -nocrl -certfile "$CA_BUNDLE" 2>/dev/null \
            | openssl pkcs7 -print_certs -noout 2>/dev/null | grep -qF "$CA_SUBJECT_CN"
}
autostart_installed() { "$BIN" autostart status 2>/dev/null | grep -qi 'Autostart: installed'; }

find_deb() {
    if [ -n "${PORTZERO_DEB:-}" ]; then printf '%s\n' "$PORTZERO_DEB"; return; fi
    local here d
    here="$(cd "$(dirname "$0")/../.." && pwd)"
    for d in "$here"/vmtest/.downloaded-artifacts/linux/*.deb "$here"/target/debian/*.deb; do
        [ -f "$d" ] && { printf '%s\n' "$d"; return; }
    done
}
fetch_prior_deb() { # best-effort fetch of the latest published release .deb
    command -v gh >/dev/null 2>&1 || return 1
    local out; out="$(mktemp -d)"
    gh release download --repo PortZeroNetwork/portzero-local \
        --pattern '*.deb' --dir "$out" >/dev/null 2>&1 || return 1
    local d; d="$(find "$out" -name '*.deb' | head -1)"
    [ -n "$d" ] && { printf '%s\n' "$d"; return 0; }
    return 1
}

install_deb() { # <path> — dpkg -i, retrying with apt -f install on dep failure
    sudo_ dpkg -i "$1" >/tmp/pz-dpkg-i.log 2>&1 || {
        sudo_ apt-get -f install -y >/dev/null 2>&1 || true
        sudo_ dpkg -i "$1" >/tmp/pz-dpkg-i.log 2>&1 || true
    }
}

# --- local-tunnel helper: serve a tagged echo, start the daemon, assert the
# .portzero.local domain resolves+serves through the overlay directly, then
# stop the daemon. Local-only: a cloud tunnel's public domain isn't reachable
# via a same-host http:// poll the way the overlay intercepts .portzero.local
# (it needs route-registration + approval first, and only serves over the real
# public HTTPS edge) — see run_cloud_tunnel_test below for that flow.
run_tunnel_test() { # <phase-prefix> <tunnel-env-value> <domain-to-check> <expected-body>
    local prefix="$1" tunnel_env="$2" domain="$3" body="$4"
    local work; work="$(mktemp -d)"
    local svc_pid=""

    sudo_ env PZ_TUNNEL="$tunnel_env" perl "$LIB" "$body" "$PORT" >"$work/svc.out" 2>&1 &
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

    sudo_ env HOME="$HOME" "$BIN" start --no-browser >"$work/start.out" 2>&1
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
        [ -f "$HOME/.portzero/daemon/daemon.log" ] && tail -8 "$HOME/.portzero/daemon/daemon.log"
        fails=$((fails + 1))
    fi

    # Bounded stop, then force-kill — never let cleanup wedge the rest of the run.
    ( sudo_ env HOME="$HOME" "$BIN" stop >/dev/null 2>&1 ) & local sp=$!
    ( sleep 10; kill -9 "$sp" 2>/dev/null ) >/dev/null 2>&1 &
    wait "$sp" 2>/dev/null
    kill -9 "$svc_pid" >/dev/null 2>&1
    sudo_ pkill -9 -f http-echo.pl >/dev/null 2>&1
    rm -rf "$work"
}

# --- cloud-tunnel helper: serve a tagged echo, start the daemon pointed at
# staging, wait for the route to actually REGISTER via the cloud API (the
# real readiness gate — the daemon connects out to the edge asynchronously),
# approve it, then fetch the real public HTTPS URL. Keeps the daemon alive
# through approval + fetch and stops it exactly once at the end.
run_cloud_tunnel_test() { # <tunnel> <token>
    local tunnel="$1" token="$2"
    local work; work="$(mktemp -d)"
    local svc_pid=""

    sudo_ env PZ_TUNNEL="${tunnel}:80" perl "$LIB" "$STAGING_BODY" "$PORT" >"$work/svc.out" 2>&1 &
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

    sudo_ env HOME="$HOME" PZ_TUNNEL_API_URL="$API" PZ_TUNNEL_EDGE_URL="$EDGE" PZ_TUNNEL_BASE_DOMAIN="$STAGING_DOMAIN" \
        "$BIN" start --no-browser >"$work/start.out" 2>&1
    echo "PHASE=cloud-daemon-launched ok=true"

    local reg=0
    for _ in $(seq 1 45); do
        curl -fsS -H "Authorization: Bearer $token" "$API/routes" 2>/dev/null | grep -q "\"$tunnel\"" && { reg=1; break; }
        sleep 2
    done
    if [ "$reg" != 1 ]; then
        echo "PHASE=cloud-route-registered ok=false"
        [ -f "$HOME/.portzero/daemon/daemon.log" ] && tail -8 "$HOME/.portzero/daemon/daemon.log"
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
    ( sudo_ env HOME="$HOME" PZ_TUNNEL_API_URL="$API" PZ_TUNNEL_EDGE_URL="$EDGE" PZ_TUNNEL_BASE_DOMAIN="$STAGING_DOMAIN" \
        "$BIN" stop >/dev/null 2>&1 ) & local sp=$!
    ( sleep 10; kill -9 "$sp" 2>/dev/null ) >/dev/null 2>&1 &
    wait "$sp" 2>/dev/null
    kill -9 "$svc_pid" >/dev/null 2>&1
    sudo_ pkill -9 -f http-echo.pl >/dev/null 2>&1
    rm -rf "$work"
}

# =============================================================================
NEW="$(find_deb)"
OLD="${PORTZERO_DEB_OLD:-}"
[ -z "$OLD" ] && OLD="$(fetch_prior_deb || true)"
echo "PHASE=preflight new=${NEW:-none} old=${OLD:-none}"
if [ -z "$NEW" ] || [ ! -f "$NEW" ]; then
    echo "RESULT=FAIL no new .deb found (set PORTZERO_DEB or drop one under vmtest/.downloaded-artifacts/linux/)"
    exit 1
fi
HAVE_UPGRADE=0
if [ -n "$OLD" ] && [ -f "$OLD" ]; then HAVE_UPGRADE=1; fi

# --- 1. INSTALL (real package: prior version if we have one, else new) -----
FIRST="$NEW"; [ "$HAVE_UPGRADE" = 1 ] && FIRST="$OLD"
echo ">> installing $FIRST"
install_deb "$FIRST"
assert install-binary            test -x "$BIN"
assert install-cap-net-admin     has_cap cap_net_admin
assert install-cap-net-bind      has_cap cap_net_bind_service
assert install-hosts-pin         hosts_has_pin
assert install-systemd-unit      test -f "$SYSTEM_UNIT"

echo ">> installing local CA into the system trust store"
sudo_ env HOME="$HOME" "$BIN" trust generate >/dev/null 2>&1 || true
sudo_ env HOME="$HOME" "$BIN" trust install  >/dev/null 2>&1 || true
assert trust-install-anchor  test -e "$CA_DEBIAN" -o -e "$CA_RHEL"
assert trust-install-config  config_has_ca_line
assert trust-install-bundle  bundle_has_ca

echo ">> enabling autostart (systemd user unit)"
"$BIN" autostart enable >/dev/null 2>&1 || true
assert autostart-user-unit   test -f "$USER_UNIT"
assert autostart-status      autostart_installed

# --- 2. LOCAL TUNNEL TEST (real TUN + scoped DNS + local proxy) ------------
echo ">> local-tunnel test against the installed binary"
for d in "$HOME/.portzero"; do sudo_ rm -f "$d/auth.json" 2>/dev/null; done
run_tunnel_test local "$LOCAL_DOMAIN" "$LOCAL_DOMAIN" "$LOCAL_BODY"

# --- 3. UPGRADE (new .deb over the top; assert nothing duplicated) --------
if [ "$HAVE_UPGRADE" = 1 ]; then
    assert prior-single-hosts-pin test "$(hosts_pin_count)" = 1
    assert prior-single-unit      test "$(unit_count)" = 1
    echo ">> upgrading to new version ($NEW)"
    install_deb "$NEW"
    assert upgraded-installed        test -x "$BIN"
    assert upgrade-single-hosts-pin  test "$(hosts_pin_count)" = 1
    assert upgrade-single-unit       test "$(unit_count)" = 1
else
    echo "PHASE=upgrade skipped=no-prior-artifact (set PORTZERO_DEB_OLD or provide network+gh)"
fi

# --- 4. CLOUD TUNNEL TEST (real staging tunnel, against the new binary) ---
if [ "${COMBINED_SKIP_CLOUD:-0}" = 1 ]; then
    echo "PHASE=cloud-tunnel ok=SKIP reason=\"COMBINED_SKIP_CLOUD=1\""
else
    SEED="$(grep -E '^TEST_LOGIN_SEED_TOKEN=' "$STAGING_SECRETS" 2>/dev/null | sed 's/^[^=]*=//')"
    if [ -z "$SEED" ]; then
        echo "PHASE=cloud-tunnel ok=SKIP reason=\"no seed token at $STAGING_SECRETS\""
    else
        st="$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 "https://app.$STAGING_DOMAIN/" 2>/dev/null)"
        if [ "$st" != 200 ]; then
            echo "PHASE=cloud-tunnel ok=SKIP reason=\"staging not up (HTTP $st)\""
        else
            SUFFIX="$(date +%m%d%H%M%S)"
            USERNAME="vmtestlin${SUFFIX}"
            EMAIL="vmtest-lin-${SUFFIX}@example.com"
            ACCOUNT="vmtest-lin-${SUFFIX}"
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
                sudo_ mkdir -p "$HOME/.portzero"
                printf '%s' "$VER" | sudo_ tee "$HOME/.portzero/auth.json" >/dev/null

                run_cloud_tunnel_test "$TUNNEL" "$TOKEN"

                sudo_ rm -f "$HOME/.portzero/auth.json" 2>/dev/null
            fi
        fi
    fi
fi

# --- 5. UNINSTALL (the way a user removes it) ------------------------------
echo ">> running trust uninstall + autostart disable"
sudo_ env HOME="$HOME" "$BIN" trust uninstall >/dev/null 2>&1 || true
"$BIN" autostart disable >/dev/null 2>&1 || true
echo ">> removing the package (dpkg --purge exercises prerm)"
sudo_ dpkg --purge portzero >/tmp/pz-dpkg-r.log 2>&1 || sudo_ dpkg -r portzero >/tmp/pz-dpkg-r.log 2>&1 || true

# --- ASSERT CLEAN: every install artifact must be GONE ---------------------
assert_not clean-binary          test -e "$BIN"
assert_not clean-systemd-unit    test -e "$SYSTEM_UNIT"
assert_not clean-hosts-pin       hosts_has_pin
assert_not clean-user-unit       test -e "$USER_UNIT"
assert_not clean-autostart       autostart_installed
assert_not clean-ca-anchor       test -e "$CA_DEBIAN"
assert_not clean-ca-anchor-rhel  test -e "$CA_RHEL"
assert_not clean-ca-ssl-pem      test -e "$CA_SSL_PEM"
assert_not clean-ca-config       config_has_ca_line
assert_not clean-ca-bundle       bundle_has_ca

if [ "$fails" -eq 0 ]; then
    echo "RESULT=PASS combined (install -> local-tunnel -> upgrade -> cloud-tunnel -> uninstall -> assert-clean)"
    exit 0
fi
echo "RESULT=FAIL combined assertions failed=$fails"
exit 1
