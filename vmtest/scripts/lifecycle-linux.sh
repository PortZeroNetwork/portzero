#!/usr/bin/env bash
# LIFECYCLE E2E (Linux): real .deb install -> verify artifacts landed -> real
# uninstall -> ASSERT every artifact is GONE. This is the coverage the launch
# audit flagged as missing: nothing proved that removing portzero takes its
# trusted CA out of the OS trust store, removes the /etc/hosts DNS pin, unloads
# the autostart service, and cleans up files. Leftover trusted-CA / DNS residue
# after uninstall is the loud-complaint bug class, so every removal is asserted.
#
# Unlike the old harness (which ran a raw `cargo build` binary and never touched
# a package), this installs the REAL `.deb` with `dpkg -i`, so the real postinst
# (setcap, /etc/hosts pin, systemd unit) and prerm run exactly as a user's would.
#
# Output is greppable per the vmtest README: `PHASE=<name> ... ok=<true|false>`
# for every assertion, then a final `RESULT=PASS|FAIL`.
#
# Env:
#   PORTZERO_DEB           path to the .deb to install. If unset, searches
#                          vmtest/.downloaded-artifacts/linux/*.deb then
#                          target/debian/*.deb.
#   LIFECYCLE_SKIP_CLI=1   skip the trust-store + autostart phases (the ones that
#                          need the REAL portzero binary to install a CA / a
#                          systemd user unit). Used only for the in-sandbox
#                          synthetic-.deb smoke that validates the package
#                          lifecycle (postinst/prerm/hosts/setcap/unit) itself.
#                          The VM runner leaves this UNSET so trust + autostart
#                          install/uninstall are exercised for real.
#   LIFECYCLE_TUN=1        additionally bring the overlay up and assert the TUN
#                          interface appears then is gone after teardown. Off by
#                          default (needs /dev/net/tun; the byte path is already
#                          covered by the real_tun_overlay Rust e2e).
# Helper predicates below are invoked indirectly (their names are passed to
# `assert`/`assert_not` as commands), which defeats shellcheck's reachability
# heuristic and yields spurious SC2317 "unreachable" info for every one of them.
# shellcheck disable=SC2317
set -uo pipefail

BIN=/usr/local/bin/portzero
TRAY_BIN=/usr/local/bin/portzero-tray
SYSTEM_UNIT=/usr/lib/systemd/user/portzero-daemon.service
USER_UNIT="${HOME}/.config/systemd/user/portzero-daemon.service"
# Shipped by the .deb (client/crates/cli/Cargo.toml assets), not generated at
# runtime: the app-launcher entry and the tray's XDG autostart registration.
APP_LAUNCHER_ENTRY=/usr/share/applications/portzero.desktop
TRAY_AUTOSTART_ENTRY=/etc/xdg/autostart/portzero-tray.desktop
HOSTS_PIN='# portzero-local'
# Trust-store destinations, mirroring client/crates/daemon/src/tls/trust.rs.
CA_DEBIAN=/usr/share/ca-certificates/portzero/portzero-local-ca.crt
CA_SSL_PEM=/etc/ssl/certs/portzero-local-ca.pem
CA_RHEL=/etc/ca-certificates/trust-source/anchors/portzero-local-ca.crt
CA_CONFIG=/etc/ca-certificates.conf
CA_CONFIG_LINE='portzero/portzero-local-ca.crt'
CA_BUNDLE=/etc/ssl/certs/ca-certificates.crt
CA_SUBJECT_CN='PortZero Local CA'
TUN_IFACE=deven0

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
# Like assert, but never counts as a failure: reports ok=true when the
# predicate holds, else ok=SKIP with a reason. Reserved for runtime states a
# headless `prlctl exec` install genuinely cannot guarantee — whether the tray
# is actually running depends on a live desktop session existing at install
# time (see tray_process_running below), which this VM login state can't
# promise either way.
assert_or_skip() {
    local phase="$1" reason="$2"; shift 2
    if "$@" >/dev/null 2>&1; then echo "PHASE=$phase ok=true"
    else echo "PHASE=$phase ok=SKIP reason=\"$reason\""; fi
}

has_cap() { # <cap-substring>
    local caps; caps="$(getcap "$BIN" 2>/dev/null || true)"
    case "$caps" in *"$1"*) return 0 ;; *) return 1 ;; esac
}
hosts_has_pin() { grep -qF "$HOSTS_PIN" /etc/hosts 2>/dev/null; }
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
tun_present() { ip link show "$TUN_IFACE" >/dev/null 2>&1; }
tray_process_running() { pgrep -x portzero-tray >/dev/null 2>&1; }

find_deb() {
    if [ -n "${PORTZERO_DEB:-}" ]; then printf '%s\n' "$PORTZERO_DEB"; return; fi
    local here; here="$(cd "$(dirname "$0")/../.." && pwd)"
    local d
    for d in "$here"/vmtest/.downloaded-artifacts/linux/*.deb "$here"/target/debian/*.deb; do
        [ -f "$d" ] && { printf '%s\n' "$d"; return; }
    done
}

DEB="$(find_deb)"
echo "PHASE=preflight deb=${DEB:-none} skip_cli=${LIFECYCLE_SKIP_CLI:-0} tun=${LIFECYCLE_TUN:-0}"
if [ -z "$DEB" ] || [ ! -f "$DEB" ]; then
    echo "RESULT=FAIL no .deb found (set PORTZERO_DEB or drop one under vmtest/.downloaded-artifacts/linux/)"
    exit 1
fi

# --- INSTALL (real package) ------------------------------------------------
echo ">> installing $DEB"
if ! sudo_ dpkg -i "$DEB" >/tmp/pz-dpkg-i.log 2>&1; then
    # Resolve any missing dependencies the way a user is told to, then re-run.
    sudo_ apt-get -f install -y >/dev/null 2>&1 || true
    sudo_ dpkg -i "$DEB" >/tmp/pz-dpkg-i.log 2>&1 || true
fi
assert install-binary            test -x "$BIN"
assert install-cap-net-admin     has_cap cap_net_admin
assert install-cap-net-bind      has_cap cap_net_bind_service
assert install-hosts-pin         hosts_has_pin
assert install-systemd-unit      test -f "$SYSTEM_UNIT"

# Tray icon + app-launcher entry: proves the package lands the tray binary,
# registers it for XDG autostart, and gives the desktop app a launcher entry,
# all without any post-install step from the user.
assert install-tray-binary          test -x "$TRAY_BIN"
assert install-app-launcher-entry   test -f "$APP_LAUNCHER_ENTRY"
assert install-tray-autostart-entry test -f "$TRAY_AUTOSTART_ENTRY"
# postinst best-effort launches the tray immediately when a live desktop
# session (D-Bus socket) is present for the installing user; whether this VM's
# `dpkg -i` runs under one is not guaranteed, so this can't be a hard assert.
assert_or_skip install-tray-running \
    "postinst only launches the tray immediately under a live desktop session for the installing user; this VM's dpkg -i may run headless (autostart is still registered regardless, via install-tray-autostart-entry)" \
    tray_process_running

# --- USE: trust store + autostart (needs the REAL binary) ------------------
if [ "${LIFECYCLE_SKIP_CLI:-0}" = 1 ]; then
    echo "PHASE=trust-install skipped=no-real-binary"
    echo "PHASE=autostart-enable skipped=no-real-binary"
else
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
fi

# --- optional: real TUN interface lifecycle --------------------------------
if [ "${LIFECYCLE_TUN:-0}" = 1 ] && [ "${LIFECYCLE_SKIP_CLI:-0}" != 1 ]; then
    echo ">> starting daemon to bring up the overlay TUN"
    sudo_ env HOME="$HOME" "$BIN" start --no-browser >/dev/null 2>&1 || true
    up=0; for _ in $(seq 1 30); do tun_present && { up=1; break; }; sleep 1; done
    assert tun-up test "$up" = 1
    sudo_ env HOME="$HOME" "$BIN" stop >/dev/null 2>&1 || true
    sleep 2
fi

# --- UNINSTALL (the way a user removes it) ---------------------------------
if [ "${LIFECYCLE_SKIP_CLI:-0}" != 1 ]; then
    echo ">> running trust uninstall + autostart disable"
    sudo_ env HOME="$HOME" "$BIN" trust uninstall >/dev/null 2>&1 || true
    "$BIN" autostart disable >/dev/null 2>&1 || true
fi
echo ">> removing the package (dpkg --purge exercises prerm)"
sudo_ dpkg --purge portzero >/tmp/pz-dpkg-r.log 2>&1 || sudo_ dpkg -r portzero >/tmp/pz-dpkg-r.log 2>&1 || true

# --- ASSERT CLEAN: every install artifact must be GONE ---------------------
assert_not clean-binary          test -e "$BIN"
assert_not clean-systemd-unit    test -e "$SYSTEM_UNIT"
assert_not clean-hosts-pin       hosts_has_pin
assert_not clean-tun             tun_present
assert_not clean-tray-binary          test -e "$TRAY_BIN"
assert_not clean-app-launcher-entry   test -e "$APP_LAUNCHER_ENTRY"
assert_not clean-tray-autostart-entry test -e "$TRAY_AUTOSTART_ENTRY"
assert_not clean-tray-running         tray_process_running
if [ "${LIFECYCLE_SKIP_CLI:-0}" != 1 ]; then
    assert_not clean-user-unit       test -e "$USER_UNIT"
    assert_not clean-autostart       autostart_installed
    assert_not clean-ca-anchor       test -e "$CA_DEBIAN"
    assert_not clean-ca-anchor-rhel  test -e "$CA_RHEL"
    assert_not clean-ca-ssl-pem      test -e "$CA_SSL_PEM"
    assert_not clean-ca-config       config_has_ca_line
    assert_not clean-ca-bundle       bundle_has_ca
fi

if [ "$fails" -eq 0 ]; then
    echo "RESULT=PASS lifecycle (install -> verify -> uninstall -> assert-clean)"
    exit 0
fi
echo "RESULT=FAIL lifecycle assertions failed=$fails"
exit 1
