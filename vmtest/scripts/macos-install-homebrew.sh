#!/usr/bin/env bash
# Install Homebrew into the macOS guest headlessly, so `just vm-test macos` /
# the CI VM-E2E job can exercise the real `brew install portzero` path (the
# Homebrew tap is a first-class install channel — see homebrew-portzero).
#
# Runs under `prlctl exec`, i.e. as ROOT. Homebrew refuses to install as root,
# so this:
#   1. ensures Xcode Command Line Tools (Homebrew's hard dependency) via the
#      sibling macos-install-clt.sh — which softwareupdate-installs it as root;
#   2. grants the admin login user (default `parallels`) *temporary* passwordless
#      sudo, because Homebrew's installer shells out to `sudo` for the one-time
#      /usr/local chown and can't prompt on a headless guest;
#   3. runs the official installer AS that user with NONINTERACTIVE=1;
#   4. removes the temporary sudoers drop-in again, leaving the VM as close to a
#      real end-user machine as possible (brew itself needs no sudo post-install).
#
# Idempotent: exits early if `brew` already works. Emits greppable KEY=value /
# PHASE lines like the other vmtest scripts, not prose.
#
# Env overrides:
#   BREW_USER   admin login user to own the install (default: first non-root
#               admin with uid>=500, i.e. `parallels` on the stock image).
set -uo pipefail

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# --- pick the user that will own Homebrew -----------------------------------
pick_brew_user() {
    if [ -n "${BREW_USER:-}" ]; then echo "$BREW_USER"; return; fi
    # First admin-group member that is a real, non-root local account (uid>=500).
    local members u uid
    members="$(dscl . -read /Groups/admin GroupMembership 2>/dev/null | sed 's/^GroupMembership: //')"
    for u in $members; do
        [ "$u" = root ] && continue
        uid="$(dscl . -read "/Users/$u" UniqueID 2>/dev/null | awk '{print $2}')"
        [ -n "$uid" ] && [ "$uid" -ge 500 ] 2>/dev/null && { echo "$u"; return; }
    done
    return 1
}

USER_NAME="$(pick_brew_user)" || { echo "brew_user=NONE_FOUND"; echo "PHASE=preflight ok=no"; exit 1; }
USER_HOME="$(dscl . -read "/Users/$USER_NAME" NFSHomeDirectory 2>/dev/null | awk '{print $2}')"
: "${USER_HOME:=/Users/$USER_NAME}"
echo "brew_user=$USER_NAME"
echo "brew_user_home=$USER_HOME"
echo "arch=$(uname -m)"

# Intel -> /usr/local, Apple Silicon -> /opt/homebrew. This harness's Mac is
# Intel; keep the check so a future arch change fails loudly rather than silently.
case "$(uname -m)" in
    x86_64) BREW_BIN=/usr/local/bin/brew ;;
    arm64)  BREW_BIN=/opt/homebrew/bin/brew ;;
    *) echo "unsupported arch"; exit 1 ;;
esac

# --- idempotency ------------------------------------------------------------
if sudo -u "$USER_NAME" "$BREW_BIN" --version >/dev/null 2>&1; then
    echo "brew_already=yes"
    echo "brew_version=$(sudo -u "$USER_NAME" "$BREW_BIN" --version 2>/dev/null | head -1)"
    echo "PHASE=install ok=yes"
    exit 0
fi

# --- 1. Command Line Tools (Homebrew dependency) ----------------------------
echo "PHASE=clt start"
if ! clang -x c -o /tmp/_cltcheck - <<<'int main(){return 0;}' 2>/dev/null; then
    bash "$SELF_DIR/macos-install-clt.sh" || { echo "PHASE=clt ok=no"; exit 1; }
fi
clang -x c -o /tmp/_cltcheck - <<<'int main(){return 0;}' 2>/dev/null \
    && echo "PHASE=clt ok=yes" || { echo "PHASE=clt ok=no"; exit 1; }

# --- 2. temporary passwordless sudo for the install user --------------------
SUDOERS=/etc/sudoers.d/portzero-brew-install
cleanup() { rm -f "$SUDOERS"; }
trap cleanup EXIT
printf '%s ALL=(ALL) NOPASSWD: ALL\n' "$USER_NAME" > "$SUDOERS"
chmod 440 "$SUDOERS"
visudo -cf "$SUDOERS" >/dev/null 2>&1 || { echo "PHASE=sudoers ok=no"; exit 1; }
echo "PHASE=sudoers ok=yes (temporary, removed on exit)"

# --- 3. run the official installer as the admin user, non-interactively -----
# Download the installer to a FILE and run it, rather than `bash -c "$(curl)"`
# under sudo: passing the script inline while sudo also parses leading VAR=val
# assignments mangles the argv (the `-c` gets lost and the `#!/bin/bash` line is
# treated as a filename -> rc 127). Running a file, and setting env via `env`
# (not `sudo VAR=val`), is the pattern this harness already learned the hard way.
echo "PHASE=install start"
INSTALL_URL="https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh"
INSTALL_SH=/tmp/portzero-brew-install.sh
curl -fsSL "$INSTALL_URL" -o "$INSTALL_SH" || { echo "brew_download=FAIL"; echo "PHASE=install ok=no"; exit 1; }
[ -s "$INSTALL_SH" ] || { echo "brew_download=EMPTY"; echo "PHASE=install ok=no"; exit 1; }
chmod 0755 "$INSTALL_SH"
# -H sets HOME (and sudo sets USER/LOGNAME) to the target user; `env` carries the
# unattended-install flags in without tripping sudo's VAR=val argv parsing.
sudo -u "$USER_NAME" -H env NONINTERACTIVE=1 CI=1 /bin/bash "$INSTALL_SH"
rc=$?
rm -f "$INSTALL_SH"
if [ "$rc" -ne 0 ]; then echo "brew_install_rc=$rc"; echo "PHASE=install ok=no"; exit 1; fi

# --- 4. verify ---------------------------------------------------------------
if sudo -u "$USER_NAME" "$BREW_BIN" --version >/dev/null 2>&1; then
    echo "brew_path=$BREW_BIN"
    echo "brew_version=$(sudo -u "$USER_NAME" "$BREW_BIN" --version 2>/dev/null | head -1)"
    echo "PHASE=install ok=yes"
else
    echo "PHASE=install ok=no (brew not runnable after install)"
    exit 1
fi
