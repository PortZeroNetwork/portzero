#!/usr/bin/env bash
# Port Zero installer for cloud coding-agent environments
# (Claude Code on the web and similar headless Linux containers).
#
# The stock installer (https://portzero.net/install.sh) targets a desktop Linux
# box: it installs a tray + app, wires the daemon into systemd, and hooks the
# scoped .portzero.local resolver into systemd-resolved. A cloud agent sandbox
# has none of that -- no systemd (PID 1 is not systemd, no user bus) and no
# browser -- but it is typically rootful with /dev/net/tun available. This
# script installs only what a headless agent needs and fills the
# systemd-resolved gap with a tiny split-DNS forwarder so *.portzero.local
# resolves transparently for curl / tests / Playwright.
#
# It is idempotent: safe to run at the top of every web session (e.g. from a
# SessionStart hook). Local (*.portzero.local) tunnels only; for cloud tunnels
# run `portzero login` afterwards.
#
# Env knobs:
#   PORT_ZERO_VERSION   pin a release tag (default: latest)
#   PZ_UPSTREAM_DNS     override upstream resolver (default: autodetected)
set -euo pipefail

REPO="PortZeroNetwork/portzero"
BIN=/usr/local/bin/portzero
LIBDIR=/usr/local/lib/portzero
DNS_SCRIPT="$LIBDIR/pzlocal-dns.py"
DNS_PIDFILE=/run/pzlocal-dns.pid
DNS_LOG=/var/log/pzlocal-dns.log
RESOLV=/etc/resolv.conf
RESOLV_BACKUP=/etc/resolv.conf.pz-backup

log() { printf '\033[0;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[0;33mwarn:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[0;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

[ "$(id -u)" -eq 0 ] || die "run as root (needed for setcap, /etc/resolv.conf, port 53)."
command -v python3 >/dev/null || die "python3 is required for the DNS forwarder."

# --- 1. Install the portzero binary --------------------------------------
install_binary() {
  if command -v portzero >/dev/null && [ -z "${PORT_ZERO_VERSION:-}" ]; then
    log "portzero already installed ($(portzero --version 2>/dev/null || echo '?')) -- skipping download."
    return
  fi
  case "$(uname -m)" in
    x86_64|amd64) arch=amd64 ;;
    aarch64|arm64) arch=arm64 ;;
    *) die "unsupported arch: $(uname -m)" ;;
  esac
  if [ -n "${PORT_ZERO_VERSION:-}" ]; then
    url="https://github.com/$REPO/releases/download/${PORT_ZERO_VERSION}/portzero-linux-${arch}.tar.gz"
  else
    url="https://github.com/$REPO/releases/latest/download/portzero-linux-${arch}.tar.gz"
  fi
  tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' RETURN
  log "downloading $url"
  curl -fsSL -o "$tmp/pz.tar.gz" "$url" || die "download failed. Check releases at https://github.com/$REPO/releases"
  tar -xzf "$tmp/pz.tar.gz" -C "$tmp"
  src="$(find "$tmp" -type f -name portzero | head -1)"
  [ -n "$src" ] || die "portzero binary not found in tarball."
  install -m 0755 "$src" "$BIN"
  log "installed $($BIN --version)"
}

# --- 2. Grant overlay capabilities ---------------------------------------
grant_caps() {
  # The daemon self-execs a helper that needs CAP_NET_ADMIN (TUN device) and
  # CAP_NET_BIND_SERVICE (port 53/80/443); file caps make it work even though
  # this container's daemon does not inherit them at launch.
  if command -v setcap >/dev/null; then
    setcap 'cap_net_admin,cap_net_bind_service+eip' "$BIN" || warn "setcap failed; overlay may not come up."
  else
    warn "setcap not found; install libcap2-bin if the overlay fails to start."
  fi
}

# --- 3. First-run setup (CA, trust, hosts apex) --------------------------
run_setup() {
  # Generates the local CA, installs it into the OS trust store, adds the
  # portzero.local apex to /etc/hosts. The systemd autostart + systemd-resolved
  # steps are expected to warn/no-op here; we handle DNS ourselves below.
  log "running portzero setup (CA + trust + hosts apex)"
  portzero setup >/dev/null 2>&1 || warn "portzero setup reported issues (expected: no systemd here)."
}

# --- 4. Split-DNS forwarder (replaces systemd-resolved routing) ----------
write_forwarder() {
  mkdir -p "$LIBDIR"
  cat > "$DNS_SCRIPT" <<'PY'
#!/usr/bin/env python3
"""Split-DNS forwarder for Port Zero on headless Linux (no systemd-resolved).
Routes *.portzero.local to the daemon's embedded resolver (10.254.0.1) and
forwards everything else to a real upstream. Stdlib only, UDP only."""
import socket, struct, sys, threading, os
LISTEN_HOST = os.environ.get("PZLOCAL_DNS_LISTEN_HOST", "127.0.0.1")
LISTEN_PORT = int(os.environ.get("PZLOCAL_DNS_LISTEN_PORT", "53"))
PZ_RESOLVER = os.environ.get("PZLOCAL_DNS_PZ_RESOLVER", "10.254.0.1")
UPSTREAM    = os.environ.get("PZLOCAL_DNS_UPSTREAM", "8.8.8.8")
SUFFIX = b"portzero.local"
def qname(pkt):
    i = 12; labels = []
    while pkt[i] != 0:
        n = pkt[i]; labels.append(pkt[i+1:i+1+n]); i += n+1
    return b".".join(labels).lower()
def route(name):
    return PZ_RESOLVER if (name == SUFFIX or name.endswith(b"."+SUFFIX)) else UPSTREAM
def handle(sock, data, addr):
    try:
        up = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); up.settimeout(5)
        up.sendto(data, (route(qname(data)), 53)); resp, _ = up.recvfrom(4096); up.close()
        sock.sendto(resp, addr)
    except Exception as e:
        sys.stderr.write(f"pzlocal-dns: {e}\n")
def main():
    pidfile = os.environ.get("PZLOCAL_DNS_PIDFILE", "/run/pzlocal-dns.pid")
    try:
        with open(pidfile, "w") as f: f.write(str(os.getpid()))
    except OSError: pass
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind((LISTEN_HOST, LISTEN_PORT))
    sys.stderr.write(f"pzlocal-dns: listening {LISTEN_HOST}:{LISTEN_PORT} "
                     f"({SUFFIX.decode()}->{PZ_RESOLVER}, else->{UPSTREAM})\n")
    while True:
        data, addr = s.recvfrom(4096)
        threading.Thread(target=handle, args=(s, data, addr), daemon=True).start()
if __name__ == "__main__":
    main()
PY
  chmod +x "$DNS_SCRIPT"
}

detect_upstream() {
  # Capture the real upstream BEFORE repointing resolv.conf. If resolv.conf is
  # already our loopback (re-run), read it from the backup to avoid a loop.
  if [ -n "${PZ_UPSTREAM_DNS:-}" ]; then echo "$PZ_UPSTREAM_DNS"; return; fi
  up="$(awk '/^nameserver/{print $2; exit}' "$RESOLV" 2>/dev/null || true)"
  if [ "$up" = "127.0.0.1" ] || [ -z "$up" ]; then
    up="$(awk '/^nameserver/{print $2; exit}' "$RESOLV_BACKUP" 2>/dev/null || true)"
  fi
  [ "$up" = "127.0.0.1" ] && up=""      # never point the forwarder at itself
  echo "${up:-8.8.8.8}"
}

forwarder_alive() {
  [ -f "$DNS_PIDFILE" ] && kill -0 "$(cat "$DNS_PIDFILE" 2>/dev/null)" 2>/dev/null
}

start_forwarder() {
  local upstream; upstream="$(detect_upstream)"
  if forwarder_alive; then
    log "DNS forwarder already running (pid $(cat "$DNS_PIDFILE"))."
  else
    log "starting split-DNS forwarder (*.portzero.local -> 10.254.0.1, else -> $upstream)"
    PZLOCAL_DNS_UPSTREAM="$upstream" PZLOCAL_DNS_PIDFILE="$DNS_PIDFILE" \
      setsid python3 "$DNS_SCRIPT" >"$DNS_LOG" 2>&1 < /dev/null &
    sleep 1
    forwarder_alive || warn "forwarder did not come up; see $DNS_LOG"
  fi
  # Point the system resolver at the forwarder (backup once). Anchor the
  # match: without the trailing $ this used to match Docker's embedded DNS
  # (127.0.0.11) and silently skip the repoint, leaving *.portzero.local
  # unresolvable in container sandboxes.
  if ! grep -q '^nameserver 127\.0\.0\.1$' "$RESOLV" 2>/dev/null; then
    [ -f "$RESOLV_BACKUP" ] || cp -a "$RESOLV" "$RESOLV_BACKUP"
    printf '# Port Zero split-DNS forwarder for *.portzero.local (orig: resolv.conf.pz-backup)\nnameserver 127.0.0.1\n' > "$RESOLV"
    log "repointed $RESOLV at the forwarder (backup: $RESOLV_BACKUP)"
  fi
}

# --- 5. Start the daemon (no systemd; plain background process) -----------
start_daemon() {
  if portzero status >/dev/null 2>&1 && portzero status 2>/dev/null | grep -q 'running'; then
    log "daemon already running; restarting to pick up caps/DNS."
    portzero restart >/dev/null 2>&1 || true
  else
    log "starting daemon"
    portzero start --no-browser >/dev/null 2>&1 || true
  fi
  sleep 2
}

# --- 6. Verify ------------------------------------------------------------
verify() {
  log "verifying overlay + resolution"
  portzero doctor 2>&1 | sed 's/^/    /' || true
  echo
  if getent hosts portzero.local >/dev/null 2>&1; then
    log "OK: portzero.local resolves ($(getent hosts portzero.local | awk '{print $1}'))"
  else
    warn "portzero.local did not resolve; check the daemon log at ~/.portzero/daemon/daemon.log"
  fi
}

main() {
  install_binary
  grant_caps
  run_setup
  write_forwarder
  start_daemon        # daemon must be up first so 10.254.0.1 answers
  start_forwarder
  start_daemon >/dev/null 2>&1 || true   # ensure caps applied post-setcap
  verify
  cat <<EOF

Port Zero is ready for *.portzero.local tunnels in this session.

Expose a service (set PZ_TUNNEL BEFORE launch, bind to port 0):
    PZ_TUNNEL=web.portzero.local:80 python3 -m http.server 0

Then, from anywhere in this container:
    curl --noproxy '*' http://web.portzero.local/

Notes:
  * Use --noproxy '*' (or NO_PROXY) so requests skip the agent HTTPS proxy.
  * portzero inspect        -- see discovered tunnels
  * portzero url <name>     -- print a tunnel's URL
  * For internet-reachable cloud tunnels: portzero login
EOF
}

main "$@"
