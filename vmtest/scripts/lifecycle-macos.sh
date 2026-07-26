#!/usr/bin/env bash
# LIFECYCLE E2E (macOS): real offline `brew install` of a locally-generated
# formula -> full privileged setup -> verify artifacts landed -> uninstall ->
# ASSERT every artifact is GONE. Closes the audit gap where uninstall was
# completely untested on macOS: nothing proved that removing portzero pulls its
# trusted CA out of the System keychain, removes the /etc/resolver/portzero.local
# resolver, unloads the root LaunchDaemon, tears down the utun interface, and
# removes the /etc/hosts pin.
#
# What the brew phase exercises (NEW): macOS ships via Homebrew (a release tar.gz
# + tap formula), not a .pkg. The Parallels macOS guest is a network-pristine,
# artifact-only end-user machine, so we cannot fetch the *published* release
# tarball in-VM (that needs GitHub Releases + the real formula sha256 — checked
# instead by release.yml's post-publish url+sha256 smoke). What we CAN do — and
# now do — is exercise the real install *mechanism* fully offline: tar the
# host-built binary into a `portzero-darwin-<arch>.tar.gz`, GENERATE a formula
# whose `url` is a `file://` path to it and whose `sha256` is computed from it,
# mirroring the shipped formula's `install` (`bin.install "portzero"`),
# `post_install` (`portzero trust generate`), and `caveats` blocks, then run
# `brew install --formula <generated.rb>` with all network access disabled. This
# runs the real formula code paths — keg install onto PATH, post_install CA
# generation — proving the delivery vehicle works, not just its residue. It
# requires Homebrew to be present in the golden snapshot; if `brew` is absent the
# brew phase SKIPs cleanly (never a false fail) and the privileged residue
# lifecycle below still runs.
#
# Prints greppable `PHASE=<name> ... ok=<true|false>` lines, then RESULT=PASS|FAIL.
#
# Env:
#   PORTZERO_EXE  path to the portzero binary (the harness pushes the host-built
#                 one in and sets this). Defaults to a copy under /tmp.
#   PORTZERO_TRAY_EXE  path to the portzero-tray binary, if built for this run
#                 (see vm-e2e.yml's "Build macOS binary" step). Defaults to a
#                 sibling `portzero-tray` next to PORTZERO_EXE. When neither
#                 exists the tray-specific checks below SKIP cleanly (never a
#                 false fail) — same idiom as the brew-not-installed SKIP.
#   LIFECYCLE_SKIP_BREW=1  skip the offline brew-install phase entirely (used
#                 only when driving the residue lifecycle in isolation).
# Helper predicates are invoked indirectly via assert/assert_not; silence the
# resulting spurious SC2317 "unreachable" info.
# shellcheck disable=SC2317
set -uo pipefail

EXE="${PORTZERO_EXE:-/tmp/portzero-vmtest/bin/portzero}"
TRAY_EXE="${PORTZERO_TRAY_EXE:-$(dirname "$EXE")/portzero-tray}"
TRAY_PLIST="$HOME/Library/LaunchAgents/cloud.portzero.tray.plist"
PLIST=/Library/LaunchDaemons/cloud.portzero.daemon.plist
RESOLVER=/etc/resolver/portzero.local
HOSTS_PIN='# portzero-local'
CERT_CN='PortZero Local CA'
SYS_KEYCHAIN=/Library/Keychains/System.keychain
TUN_PREFIX=utun

fails=0
SUDO=""; [ "$(id -u)" -ne 0 ] && SUDO="sudo"

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
# holds, else ok=SKIP with a reason. Reserved for *runtime* states that a
# headless `prlctl exec` session genuinely cannot establish on macOS, and only
# those: setting System-keychain trust settings needs a securityd/GUI auth
# session, and `launchctl bootstrap`/`load` into the system domain returns EIO
# with no bootstrap context. Both are exercised for real by e2e-local-overlay.sh
# (which brings the daemon up via `portzero start`); here we only prove the
# persistent on-disk artifacts land and are removed (the uninstall audit gap).
assert_or_skip() {
    local phase="$1" reason="$2"; shift 2
    if "$@" >/dev/null 2>&1; then echo "PHASE=$phase ok=true"
    else echo "PHASE=$phase ok=SKIP reason=\"$reason\""; fi
}

keychain_has_ca() { $SUDO security find-certificate -c "$CERT_CN" "$SYS_KEYCHAIN" >/dev/null 2>&1; }
launchdaemon_loaded() { $SUDO launchctl print system/cloud.portzero.daemon >/dev/null 2>&1; }
hosts_has_pin() { grep -qF "$HOSTS_PIN" /etc/hosts 2>/dev/null; }
utun_present() {
    # A portzero utun carries the overlay gateway 10.254.0.1; match on that so we
    # don't trip over unrelated utun interfaces (VPNs etc.).
    ifconfig 2>/dev/null | grep -A3 "^${TUN_PREFIX}" | grep -q '10\.254\.0\.1'
}
tray_launchagent_loaded() { launchctl print "gui/$(id -u)/cloud.portzero.tray" >/dev/null 2>&1; }
tray_process_running() { pgrep -x portzero-tray >/dev/null 2>&1; }

echo "PHASE=preflight exe=$([ -x "$EXE" ] && echo true || echo false) tray_exe=$([ -x "$TRAY_EXE" ] && echo true || echo false) tun=${LIFECYCLE_TUN:-0}"
if [ ! -x "$EXE" ]; then
    echo "RESULT=FAIL portzero binary not found at $EXE (set PORTZERO_EXE)"
    exit 1
fi

# --- BREW: real offline `brew install` of a locally-generated formula --------
# Exercises the actual delivery mechanism (formula install + post_install)
# with zero network, using the host-built binary. SKIPs cleanly (never a false
# fail) when Homebrew is not present in the golden snapshot.
CA_APP_SUPPORT="$HOME/Library/Application Support/PortZero/ca.crt"
brew_prefix_bin() { brew --prefix 2>/dev/null; }

run_brew_phase() {
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
    case "$arch" in
        arm64|aarch64) tar_arch="arm64" ;;
        *)             tar_arch="amd64" ;;
    esac
    work="$(mktemp -d /tmp/pz-brew.XXXXXX)"
    tarball="$work/portzero-darwin-${tar_arch}.tar.gz"

    # Stage the binary exactly as the release tarball is laid out: a single
    # top-level dir holding `portzero`, which Homebrew strips so the formula's
    # `bin.install "portzero"` finds it at the extraction root.
    local stage="$work/portzero-darwin-${tar_arch}"
    mkdir -p "$stage"
    cp "$EXE" "$stage/portzero"
    chmod +x "$stage/portzero"
    local have_tray=0
    if [ -x "$TRAY_EXE" ]; then
        cp "$TRAY_EXE" "$stage/portzero-tray"
        chmod +x "$stage/portzero-tray"
        have_tray=1
    fi
    ( cd "$work" && tar czf "$tarball" "portzero-darwin-${tar_arch}" )

    sha="$(shasum -a 256 "$tarball" | awk '{print $1}')"

    # Generate a formula mirroring packaging/homebrew/Formula/portzero.rb's real
    # code paths (install + post_install + caveats), but pointed at the local
    # tarball via file:// so the install runs fully offline. Basename `portzero`
    # -> class `Portzero`, as Homebrew requires.
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
    bin.install "portzero-tray" if File.exist?("portzero-tray")
  end

  def post_install
    # Mirror the shipped formula: generate the local CA (unprivileged; writes to
    # ~/Library/Application Support/PortZero/). Idempotent.
    system "#{bin}/portzero", "trust", "generate"

    # Mirror the shipped formula's per-user LaunchAgent so the tray starts at
    # login without further intervention. Best-effort: only load it if the
    # tray binary shipped in this tarball.
    return unless File.exist?("#{opt_bin}/portzero-tray")

    require "fileutils"
    agents_dir = File.expand_path("~/Library/LaunchAgents")
    plist_path = "#{agents_dir}/cloud.portzero.tray.plist"
    FileUtils.mkdir_p(agents_dir)
    File.write(plist_path, <<~PLIST)
      <?xml version="1.0" encoding="UTF-8"?>
      <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
      <plist version="1.0"><dict>
        <key>Label</key><string>cloud.portzero.tray</string>
        <key>ProgramArguments</key>
        <array><string>#{opt_bin}/portzero-tray</string></array>
        <key>RunAtLoad</key><true/>
        <key>KeepAlive</key><true/>
      </dict></plist>
    PLIST
    quiet_system "/bin/launchctl", "unload", plist_path
    quiet_system "/bin/launchctl", "load", plist_path
  end

  def caveats
    <<~CAVEAT
      To complete setup, run:

        sudo portzero setup
    CAVEAT
  end
end
EOF

    # Validate the generated Ruby before handing it to brew, so a malformed
    # heredoc surfaces as a named failure rather than an opaque brew error.
    if command -v ruby >/dev/null 2>&1; then
        assert brew-formula-ruby-valid ruby -c "$formula"
    fi

    # Fully offline: no auto-update, no API/formula fetch, no analytics. The
    # file:// url means the "download" is a local copy, so nothing hits the net.
    export HOMEBREW_NO_AUTO_UPDATE=1
    export HOMEBREW_NO_INSTALL_FROM_API=1
    export HOMEBREW_NO_ANALYTICS=1
    export HOMEBREW_NO_INSTALL_UPGRADE=1
    export HOMEBREW_NO_INSTALLED_DEPENDENTS_CHECK=1

    echo ">> brew install --formula (offline, generated formula)"
    rm -f "$CA_APP_SUPPORT" 2>/dev/null || true
    rm -f "$TRAY_PLIST" 2>/dev/null || true
    local ilog="$work/brew-install.log"
    brew install --formula "$formula" >"$ilog" 2>&1 || true

    prefix="$(brew_prefix_bin)"
    # The real install mechanism ran: binary linked onto PATH under the brew
    # prefix, and post_install generated the CA cert.
    assert brew-install-binary        test -x "$prefix/bin/portzero"
    assert brew-install-on-path       command -v portzero
    assert brew-post-install-ca       test -f "$CA_APP_SUPPORT"
    # Caveats logic is sound: the formula surfaces the `sudo portzero setup`
    # instruction (brew renders caveats from the formula's method).
    assert brew-caveats-setup         brew_caveats_mentions_setup "$formula"

    # Tray icon: proves the formula lands portzero-tray and registers it as a
    # per-user LaunchAgent, so it starts at login without further intervention
    # — the delivery-vehicle half of the "you get a tray icon" ask. Only
    # exercised when a tray binary was actually available to stage (see
    # PORTZERO_TRAY_EXE / vm-e2e.yml's "Build macOS binary" step); otherwise
    # SKIP cleanly, never a false fail.
    if [ "$have_tray" = 1 ]; then
        assert brew-install-tray-binary             test -x "$prefix/bin/portzero-tray"
        assert brew-post-install-launchagent-plist  test -f "$TRAY_PLIST"
        # `launchctl load` into the per-user gui domain from a headless
        # `prlctl exec` session may not attach without an active Aqua login
        # (the same class of limitation as install-launchdaemon-loaded below,
        # just gui domain instead of system domain), so these can't be hard
        # asserts. Daemon/tray-running state is covered for real by
        # e2e-local-overlay.sh's interactive session.
        assert_or_skip brew-tray-launchagent-loaded \
            "launchctl load into the per-user gui domain from a headless prlctl exec session may not attach without an active Aqua login" \
            tray_launchagent_loaded
        assert_or_skip brew-tray-running \
            "same headless-session limitation as brew-tray-launchagent-loaded" \
            tray_process_running
    else
        echo 'PHASE=brew-install-tray-binary ok=SKIP reason="no portzero-tray binary available (set PORTZERO_TRAY_EXE)"'
    fi

    echo ">> brew uninstall --formula"
    pkill -x portzero-tray 2>/dev/null || true
    launchctl unload "$TRAY_PLIST" >/dev/null 2>&1 || true
    brew uninstall --formula portzero >/dev/null 2>&1 || true
    assert brew-uninstall-binary-gone test ! -e "$prefix/bin/portzero"
    if [ "$have_tray" = 1 ]; then
        assert brew-uninstall-tray-binary-gone test ! -e "$prefix/bin/portzero-tray"
    fi
    rm -f "$TRAY_PLIST" 2>/dev/null || true

    rm -rf "$work" 2>/dev/null || true
}
brew_caveats_mentions_setup() { brew info --formula "$1" 2>/dev/null | grep -q 'sudo portzero setup'; }

run_brew_phase

# --- INSTALL: the documented privileged setup (== brew post_install + setup) --
echo ">> portzero setup (trust install + autostart + resolver + hosts pin)"
$SUDO env HOME="$HOME" "$EXE" trust generate >/dev/null 2>&1 || true
# `setup` is non-interactive-safe and idempotent; run it under a background PID
# with a bounded wait so a hung privileged step can't wedge the whole test.
( $SUDO env HOME="$HOME" "$EXE" setup >/tmp/pz-setup.log 2>&1 ) & sp=$!
( sleep 120; kill -9 "$sp" 2>/dev/null ) >/dev/null 2>&1 & killer=$!
wait "$sp" 2>/dev/null; kill "$killer" 2>/dev/null

assert install-trust-keychain keychain_has_ca
assert install-launchdaemon-plist test -f "$PLIST"
assert_or_skip install-launchdaemon-loaded \
    "launchctl bootstrap/load into the system domain returns EIO under headless prlctl exec; daemon-run state is covered by the local-overlay e2e" \
    launchdaemon_loaded
assert install-resolver test -f "$RESOLVER"
assert install-hosts-pin hosts_has_pin
if [ "${LIFECYCLE_TUN:-0}" = 1 ]; then
    up=0; for _ in $(seq 1 30); do utun_present && { up=1; break; }; sleep 1; done
    assert tun-up test "$up" = 1
fi

# --- UNINSTALL: the way the brew caveats / a user removes it -----------------
echo ">> uninstall (trust uninstall + autostart disable + resolver + hosts)"
$SUDO env HOME="$HOME" "$EXE" stop >/dev/null 2>&1 || true
$SUDO env HOME="$HOME" "$EXE" trust uninstall >/dev/null 2>&1 || true
$SUDO env HOME="$HOME" "$EXE" autostart disable >/dev/null 2>&1 || true
$SUDO rm -f "$RESOLVER" 2>/dev/null || true
# Remove the managed hosts pin, preserving user lines (same shape as prerm/sed).
if hosts_has_pin; then
    tmp="$(mktemp)"; grep -vF "$HOSTS_PIN" /etc/hosts > "$tmp" 2>/dev/null || true
    $SUDO install -m 0644 "$tmp" /etc/hosts 2>/dev/null || true
    rm -f "$tmp"
fi
sleep 2

# --- ASSERT CLEAN: every install artifact must be GONE ----------------------
assert_not clean-trust-keychain keychain_has_ca
assert_not clean-launchdaemon-plist test -e "$PLIST"
assert_not clean-launchdaemon-loaded launchdaemon_loaded
assert_not clean-resolver test -e "$RESOLVER"
assert_not clean-hosts-pin hosts_has_pin
assert_not clean-tun utun_present

if [ "$fails" -eq 0 ]; then
    echo "RESULT=PASS lifecycle (setup -> verify -> uninstall -> assert-clean)"
    exit 0
fi
echo "RESULT=FAIL lifecycle assertions failed=$fails"
exit 1
