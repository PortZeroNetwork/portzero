#!/usr/bin/env bash
# FULL E2E: local overlay tunnel (.portzero.local) on Linux/macOS. No cloud, no
# secrets. Exercises the privileged overlay path (TUN/utun + scoped DNS + local
# proxy): serve a tagged process, start the daemon, prove the name resolves
# through the overlay and HTTP returns the served body. Prints PHASE/RESULT.
#
# Two tagged services are served, not one: a normal IPv4-loopback service and a
# second bound to `::1` ALONE. The IPv6 one is the regression this covers — such
# a service is discovered correctly (right domain, right port, right PID) and
# was then proxied to an empty 127.0.0.1, so the tunnel connected and returned
# zero bytes. Only the daemon's backend dial differs; the client side of the
# overlay is IPv4 either way.
set -uo pipefail
EXE="${PORTZERO_EXE:-/root/pz-target/release/portzero}"
NAME=vmtestlocal
DOMAIN="$NAME.portzero.local"
BODY="portzero-local-overlay-ok"
PORT=18080
NAME6=vmtestlocal6
DOMAIN6="$NAME6.portzero.local"
BODY6="portzero-local-overlay-ipv6-ok"
PORT6=18081
LIB="$(cd "$(dirname "$0")" && pwd)/lib/http-echo.pl"
WORK="$(mktemp -d)"
# The daemon needs root for TUN/utun. prlctl exec is root on Linux; on macOS
# we're a normal user with passwordless sudo.
SUDO=""; [ "$(id -u)" -ne 0 ] && SUDO="sudo"
case "$(uname)" in Darwin) RH=/var/root;; *) RH=/root;; esac

svc_pid=""
svc6_pid=""
cleanup() {
    set +e
    # Bounded stop, then force-kill — never let cleanup wedge.
    ( $SUDO env HOME="$RH" "$EXE" stop >/dev/null 2>&1 ) & sp=$!
    ( sleep 10; kill -9 "$sp" 2>/dev/null ) >/dev/null 2>&1 &
    wait "$sp" 2>/dev/null
    $SUDO pkill -9 -x portzero >/dev/null 2>&1
    $SUDO pkill -9 -f http-echo.pl >/dev/null 2>&1
    [ -n "$svc_pid" ] && kill -9 "$svc_pid" >/dev/null 2>&1
    [ -n "$svc6_pid" ] && kill -9 "$svc6_pid" >/dev/null 2>&1
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

# --- second tagged service, bound to ::1 ONLY. If the guest cannot serve it at
# all (IPv6 disabled, perl without IO::Socket::IP), the IPv6 leg is skipped
# rather than failed — but a service that IS reachable on [::1] directly and
# NOT through its tunnel is a real failure, asserted below.
$SUDO env PZ_TUNNEL="$DOMAIN6" "$PERL" "$LIB" "$BODY6" "$PORT6" '::1' >"$WORK/svc6.out" 2>&1 &
svc6_pid=$!
up6=0
for _ in $(seq 1 20); do curl -sf --max-time 2 "http://[::1]:$PORT6/" >/dev/null 2>&1 && { up6=1; break; }; sleep 0.5; done
if [ "$up6" = 1 ]; then
    echo "PHASE=service-ipv6 pid=$svc6_pid port=$PORT6 tunnel=$DOMAIN6"
else
    echo "PHASE=service-ipv6 ok=SKIP reason=\"no IPv6 loopback service on this guest; $(head -1 "$WORK/svc6.out" 2>/dev/null)\""
    kill -9 "$svc6_pid" >/dev/null 2>&1
    svc6_pid=""
fi

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
daemon_log_tail() {
    echo "PHASE=diag daemon-log-tail:"
    for d in "$HOME/.portzero" "$RH/.portzero"; do
        [ -f "$d/daemon/daemon.log" ] && { $SUDO tail -8 "$d/daemon/daemon.log"; break; }
    done
}

if [ "$ok" != 1 ]; then
    daemon_log_tail
    echo "RESULT=FAIL domain=$DOMAIN (overlay did not serve within timeout)"
    exit 1
fi
echo "PHASE=tunnel ok=true domain=$DOMAIN body_ok=true"

# --- the IPv6-only backend must be reachable through its tunnel too. The
# overlay is already up by now, so this needs far less patience than the first.
if [ -n "$svc6_pid" ]; then
    ok6=0
    for _ in $(seq 1 30); do
        b6="$(curl -sf --max-time 5 "http://$DOMAIN6/" 2>/dev/null)"
        [ "$b6" = "$BODY6" ] && { ok6=1; break; }
        sleep 2
    done
    if [ "$ok6" != 1 ]; then
        daemon_log_tail
        echo "RESULT=FAIL domain=$DOMAIN6 (IPv6-only backend not reachable through its tunnel)"
        exit 1
    fi
    echo "PHASE=tunnel-ipv6 ok=true domain=$DOMAIN6 body_ok=true"
fi

ipv6_result=skipped
[ -n "$svc6_pid" ] && ipv6_result="$DOMAIN6"
echo "RESULT=PASS domain=$DOMAIN body_ok=true ipv6_domain=$ipv6_result"
