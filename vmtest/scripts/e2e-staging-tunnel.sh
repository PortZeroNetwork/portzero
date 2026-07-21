#!/usr/bin/env bash
# FULL E2E: staging cloud tunnel on Linux/macOS. Authenticates a test client
# against real staging with the rotated seed token, registers a public tunnel
# through the daemon, approves it, and fetches the public URL. No jq (not on the
# VMs): flat-JSON fields are pulled with grep/sed. Prints PHASE/RESULT.
set -uo pipefail
EXE="${PORTZERO_EXE:-/root/pz-target/release/portzero}"
DOMAIN=devenvtools.top
SECRETS="${STAGING_SECRETS:-/media/psf/MBP-Sidecar/loumtech/vm-toolchain-cache/common/staging-e2e.env}"
LIB="$(cd "$(dirname "$0")" && pwd)/lib/http-echo.pl"
SUFFIX="$(date +%m%d%H%M%S)"
PLAT="$(uname -s | tr '[:upper:]' '[:lower:]' | cut -c1-3)"   # lin / dar
USERNAME="vmtest${PLAT}${SUFFIX}"
EMAIL="vmtest-${PLAT}-${SUFFIX}@example.com"
ACCOUNT="vmtest-${PLAT}-${SUFFIX}"
CODE=424242
BODY="portzero-staging-tunnel-ok"
PORT=18080
API="https://app.$DOMAIN/api"
EDGE="wss://edge.$DOMAIN/tunnel"
TUNNEL="${USERNAME}.tunnel.$DOMAIN"
SUDO=""; [ "$(id -u)" -ne 0 ] && SUDO="sudo"
case "$(uname)" in Darwin) RH=/var/root;; *) RH=/root;; esac
WORK="$(mktemp -d)"
svc_pid=""

# Extract a flat JSON string field: json <name> <<<"$response"
json() { grep -oE "\"$1\":\"[^\"]*\"" | sed "s/^\"$1\":\"//;s/\"\$//" | head -1; }

cleanup() {
    set +e
    ( $SUDO env HOME="$RH" PZ_TUNNEL_API_URL="$API" PZ_TUNNEL_EDGE_URL="$EDGE" PZ_TUNNEL_BASE_DOMAIN="$DOMAIN" "$EXE" stop >/dev/null 2>&1 ) & sp=$!
    ( sleep 10; kill -9 "$sp" 2>/dev/null ) >/dev/null 2>&1 &
    wait "$sp" 2>/dev/null
    $SUDO pkill -9 -x portzero >/dev/null 2>&1
    $SUDO pkill -9 -f http-echo.pl >/dev/null 2>&1
    [ -n "$svc_pid" ] && kill -9 "$svc_pid" >/dev/null 2>&1
    for d in "$HOME/.portzero" "$RH/.portzero"; do $SUDO rm -f "$d/auth.json" 2>/dev/null; done
    rm -rf "$WORK"
}
trap cleanup EXIT

SEED="$(grep -E '^TEST_LOGIN_SEED_TOKEN=' "$SECRETS" 2>/dev/null | sed 's/^[^=]*=//')"
echo "PHASE=preflight exe=$([ -x "$EXE" ] && echo true || echo false) tunnel=$TUNNEL token=$([ -n "$SEED" ] && echo true || echo false)"
[ -n "$SEED" ] || { echo "RESULT=FAIL no seed token in $SECRETS"; exit 1; }

st="$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 "https://app.$DOMAIN/" 2>/dev/null)"
[ "$st" = 200 ] || { echo "RESULT=FAIL staging not up (HTTP $st)"; exit 1; }

# --- seed an auth code, then verify to get a JWT ---
curl -fsS -H "Authorization: Bearer $SEED" -H 'Content-Type: application/json' \
    -d "{\"email\":\"$EMAIL\",\"username\":\"$USERNAME\",\"account_id\":\"$ACCOUNT\",\"code\":\"$CODE\"}" \
    "$API/auth/test-seed-login" >/dev/null || { echo "RESULT=FAIL seed-login failed"; exit 1; }
VER="$(curl -fsS -H 'Content-Type: application/json' -d "{\"email\":\"$EMAIL\",\"code\":\"$CODE\"}" "$API/auth/verify")"
TOKEN="$(printf '%s' "$VER" | json token)"
[ -n "$TOKEN" ] || { echo "RESULT=FAIL verify returned no token"; exit 1; }
echo "PHASE=auth user=$USERNAME"

# daemon runs as root; write auth.json where root's home is.
$SUDO mkdir -p "$RH/.portzero"
printf '%s' "$VER" | $SUDO tee "$RH/.portzero/auth.json" >/dev/null

# --- tagged service (:80 canonical external port; daemon finds the real port).
# Run as ROOT (same uid as the daemon) so discovery reads its env.
#
# macOS caveat (task-74): a SIP-protected system binary like /usr/bin/perl has
# its env hidden from every other process (neither `ps -E` nor KERN_PROCARGS2
# can read it, at any privilege), so run it from a *copy* of perl at an
# unrestricted path. Linux exposes /proc/<pid>/environ regardless. ---
PERL="perl"
if [ "$(uname)" = "Darwin" ]; then
    PERL="$WORK/perl"
    cp "$(command -v perl)" "$PERL" && chmod +x "$PERL"
fi
$SUDO env PZ_TUNNEL="${TUNNEL}:80" "$PERL" "$LIB" "$BODY" "$PORT" >"$WORK/svc.out" 2>&1 &
svc_pid=$!
up=0; for _ in $(seq 1 30); do curl -sf "http://127.0.0.1:$PORT/" >/dev/null 2>&1 && { up=1; break; }; sleep 0.5; done
[ "$up" = 1 ] || { echo "RESULT=FAIL service not listening"; exit 1; }
echo "PHASE=service port=$PORT"

# --- start daemon pointed at staging. On Unix `portzero start` daemonizes and
# RETURNS, so run it in the FOREGROUND. Use `env` to set vars: `$SUDO VAR=val
# cmd` with an empty $SUDO makes bash execute `VAR=val` as a command (rc=127) —
# `$SUDO env VAR=val cmd` is correct whether $SUDO is empty or `sudo`. HOME=$RH
# is REQUIRED: prlctl exec runs with HOME=/, so otherwise home_dir() is / and the
# daemon never finds our auth.json.
$SUDO env HOME="$RH" PZ_TUNNEL_API_URL="$API" PZ_TUNNEL_EDGE_URL="$EDGE" PZ_TUNNEL_BASE_DOMAIN="$DOMAIN" \
    "$EXE" start --no-browser >"$WORK/start.out" 2>&1
echo "PHASE=daemon-launched rc=$?"

# --- readiness gate: the route appearing in the cloud API ---
reg=0
for _ in $(seq 1 45); do
    if curl -fsS -H "Authorization: Bearer $TOKEN" "$API/routes" 2>/dev/null | grep -q "\"$TUNNEL\""; then reg=1; break; fi
    sleep 2
done
if [ "$reg" != 1 ]; then
    echo "PHASE=diag daemon-log-tail:"
    for d in "$HOME/.portzero" "$RH/.portzero"; do [ -f "$d/daemon/daemon.log" ] && { $SUDO tail -8 "$d/daemon/daemon.log"; break; }; done
    echo "RESULT=FAIL route did not register: $TUNNEL"; exit 1
fi
echo "PHASE=route-registered"

# --- approve + fetch the public tunnel ---
curl -fsS -X POST -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d '{}' \
    "$API/routes/$TUNNEL/approve" >/dev/null 2>&1
ok=0
for _ in $(seq 1 30); do
    b="$(curl -fsS --max-time 10 "https://$TUNNEL/" 2>/dev/null)"
    [ "$b" = "$BODY" ] && { ok=1; break; }
    sleep 2
done
[ "$ok" = 1 ] && echo "RESULT=PASS tunnel=https://$TUNNEL body_ok=true" || { echo "RESULT=FAIL tunnel=https://$TUNNEL"; exit 1; }
