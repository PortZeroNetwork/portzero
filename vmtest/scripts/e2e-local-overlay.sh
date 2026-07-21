#!/usr/bin/env bash
# FULL E2E: local overlay tunnel (.portzero.local) on Linux/macOS. No cloud, no
# secrets. Exercises the privileged overlay path (TUN/utun + scoped DNS + local
# proxy): serve a tagged process, start the daemon, prove the name resolves
# through the overlay and HTTP returns the served body. Prints PHASE/RESULT.
set -uo pipefail
EXE="${PORTZERO_EXE:-/root/pz-target/release/portzero}"
NAME=vmtestlocal
DOMAIN="$NAME.portzero.local"
BODY="portzero-local-overlay-ok"
PORT=18080
LIB="$(cd "$(dirname "$0")" && pwd)/lib/http-echo.pl"
WORK="$(mktemp -d)"
# The daemon needs root for TUN/utun. prlctl exec is root on Linux; on macOS
# we're a normal user with passwordless sudo.
SUDO=""; [ "$(id -u)" -ne 0 ] && SUDO="sudo"
case "$(uname)" in Darwin) RH=/var/root;; *) RH=/root;; esac

svc_pid=""
cleanup() {
    set +e
    # Bounded stop, then force-kill — never let cleanup wedge.
    ( $SUDO env HOME="$RH" "$EXE" stop >/dev/null 2>&1 ) & sp=$!
    ( sleep 10; kill -9 "$sp" 2>/dev/null ) >/dev/null 2>&1 &
    wait "$sp" 2>/dev/null
    $SUDO pkill -9 -x portzero >/dev/null 2>&1
    $SUDO pkill -9 -f http-echo.pl >/dev/null 2>&1
    [ -n "$svc_pid" ] && kill -9 "$svc_pid" >/dev/null 2>&1
    rm -rf "$WORK"
}
trap cleanup EXIT

echo "PHASE=preflight exe=$([ -x "$EXE" ] && echo true || echo false) perl=$(command -v perl >/dev/null && echo true || echo false)"
# Local-only: make sure no auth.json biases the daemon toward cloud mode.
for d in "$HOME/.portzero" "$RH/.portzero"; do $SUDO rm -f "$d/auth.json" 2>/dev/null; done

# --- tagged service (its env carries PZ_TUNNEL so the daemon discovers it).
# Run it as ROOT (same uid as the daemon) so discovery reads its env.
#
# macOS caveat (task-74): a SIP-protected system binary like /usr/bin/perl has
# its environment hidden from *every* other process — neither `ps -E` nor
# sysctl(KERN_PROCARGS2) can read it, at any privilege. So the daemon could
# never see PZ_TUNNEL when the tagged service ran as system perl. Run it from a
# *copy* of perl at an unrestricted path, which is not SIP-protected and whose
# env is readable. On Linux /proc/<pid>/environ is exposed regardless, so use
# perl as-is.
PERL="perl"
if [ "$(uname)" = "Darwin" ]; then
    PERL="$WORK/perl"
    cp "$(command -v perl)" "$PERL" && chmod +x "$PERL"
fi
$SUDO env PZ_TUNNEL="$DOMAIN" "$PERL" "$LIB" "$BODY" "$PORT" >"$WORK/svc.out" 2>&1 &
svc_pid=$!
up=0
for _ in $(seq 1 30); do curl -sf "http://127.0.0.1:$PORT/" >/dev/null 2>&1 && { up=1; break; }; sleep 0.5; done
[ "$up" = 1 ] || { echo "RESULT=FAIL service not listening; $(cat "$WORK/svc.out" 2>/dev/null)"; exit 1; }
echo "PHASE=service pid=$svc_pid port=$PORT tunnel=$DOMAIN"

# --- start the daemon (creates TUN/utun, scoped DNS, overlay) ---
# `env` (not `$SUDO HOME=... cmd`, which runs `HOME=...` as a command when $SUDO
# is empty). Foreground: `portzero start` daemonizes and returns on Unix. HOME=$RH
# pins the daemon's home (prlctl exec has HOME=/) so its state/log land under root.
$SUDO env HOME="$RH" "$EXE" start --no-browser >"$WORK/start.out" 2>&1
echo "PHASE=daemon launched (verifying via the overlay URL directly)"

# --- readiness gate: overlay resolves + serves. No `portzero status` dependency.
# ~120s covers overlay bring-up (fast on SSD, slower on USB/first-run).
ok=0
for _ in $(seq 1 60); do
    b="$(curl -sf --max-time 5 "http://$DOMAIN/" 2>/dev/null)"
    [ "$b" = "$BODY" ] && { ok=1; break; }
    sleep 2
done
if [ "$ok" = 1 ]; then
    echo "RESULT=PASS domain=$DOMAIN body_ok=true"
else
    echo "PHASE=diag daemon-log-tail:"
    for d in "$HOME/.portzero" "$RH/.portzero"; do
        [ -f "$d/daemon/daemon.log" ] && { $SUDO tail -8 "$d/daemon/daemon.log"; break; }
    done
    echo "RESULT=FAIL domain=$DOMAIN (overlay did not serve within timeout)"
    exit 1
fi
