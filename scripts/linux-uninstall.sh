#!/bin/sh
set -e

# portzero Linux uninstaller.
#
# Usage:
#   curl -fsSL https://github.com/PortZeroNetwork/portzero-local/releases/latest/download/linux-uninstall.sh | sh
#
# Removes the Linux setup performed by scripts/linux-install.sh where possible.
# Env: PORTZERO_INSTALL_DIR adds an install directory to the binary search path.

if [ "$(uname -s)" != "Linux" ]; then
    echo "error: scripts/linux-uninstall.sh can only be run on Linux." >&2
    exit 1
fi

# Undocumented override. When portzero was installed from a .deb/.rpm this
# script hands removal to the system package manager (see below); --force skips
# that detection and deletes the files by hand instead. Intended as an escape
# hatch when the package database is broken — not for everyday use.
force=0
for arg in "$@"; do
    case "$arg" in
        --force) force=1 ;;
    esac
done

info() { printf 'info: %s\n' "$1"; }
warn() { printf 'warn: %s\n' "$1" >&2; }

has_cmd() {
    command -v "$1" >/dev/null 2>&1
}

sudo_if_needed() {
    if [ "$(id -u)" -eq 0 ]; then
        "$@"
    else
        sudo "$@"
    fi
}

remove_file() {
    path="$1"
    if [ ! -e "$path" ]; then
        return 0
    fi

    info "Removing $path"
    if rm -f "$path" 2>/dev/null; then
        return 0
    fi
    sudo_if_needed rm -f "$path" 2>/dev/null || warn "Could not remove $path"
}

portzero_candidates() {
    if [ -n "${PORTZERO_INSTALL_DIR:-}" ]; then
        printf '%s\n' "${PORTZERO_INSTALL_DIR%/}/portzero"
    fi
    if has_cmd portzero; then
        command -v portzero
    fi
    printf '%s\n' \
        "${HOME}/.local/bin/portzero" \
        "${HOME}/.cargo/bin/portzero" \
        "/usr/local/bin/portzero" \
        "/usr/bin/portzero" |
        awk 'NF && !seen[$0]++'
}

portzero_uninstall_candidates() {
    if [ -n "${PORTZERO_INSTALL_DIR:-}" ]; then
        printf '%s\n' "${PORTZERO_INSTALL_DIR%/}/portzero-uninstall"
    fi
    if has_cmd portzero-uninstall; then
        command -v portzero-uninstall
    fi
    printf '%s\n' \
        "${HOME}/.portzero/bin/portzero-uninstall" \
        "${HOME}/.local/bin/portzero-uninstall" \
        "/usr/local/bin/portzero-uninstall" \
        "/usr/bin/portzero-uninstall" |
        awk 'NF && !seen[$0]++'
}

portzero_tray_candidates() {
    if [ -n "${PORTZERO_INSTALL_DIR:-}" ]; then
        printf '%s\n' "${PORTZERO_INSTALL_DIR%/}/portzero-tray"
    fi
    if has_cmd portzero-tray; then
        command -v portzero-tray
    fi
    printf '%s\n' \
        "${HOME}/.local/bin/portzero-tray" \
        "${HOME}/.cargo/bin/portzero-tray" \
        "/usr/local/bin/portzero-tray" \
        "/usr/bin/portzero-tray" |
        awk 'NF && !seen[$0]++'
}

portzero_app_candidates() {
    if [ -n "${PORTZERO_INSTALL_DIR:-}" ]; then
        printf '%s\n' "${PORTZERO_INSTALL_DIR%/}/portzero-app"
    fi
    if has_cmd portzero-app; then
        command -v portzero-app
    fi
    printf '%s\n' \
        "${HOME}/.local/bin/portzero-app" \
        "${HOME}/.cargo/bin/portzero-app" \
        "/usr/local/bin/portzero-app" \
        "/usr/bin/portzero-app" |
        awk 'NF && !seen[$0]++'
}

run_portzero_best_effort() {
    args="$1"
    for candidate in $(portzero_candidates); do
        if [ -x "$candidate" ]; then
            # shellcheck disable=SC2086
            "$candidate" $args >/dev/null 2>&1 && return 0
        fi
    done
    return 1
}

first_existing_portzero() {
    for candidate in $(portzero_candidates); do
        if [ -x "$candidate" ]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    return 1
}

remove_hosts_pin() {
    if ! grep -q '# portzero-local' /etc/hosts 2>/dev/null; then
        return 0
    fi

    info "Removing portzero.local pin from /etc/hosts"
    tmp="${TMPDIR:-/tmp}/portzero-hosts.$$"
    trap 'rm -f "$tmp"' EXIT HUP INT TERM

    grep -v '# portzero-local' /etc/hosts > "$tmp" || true
    sudo_if_needed install -m 0644 "$tmp" /etc/hosts 2>/dev/null ||
        warn "Could not update /etc/hosts"
    rm -f "$tmp"
    trap - EXIT HUP INT TERM
}

# Detect whether the portzero binary at $1 is tracked by a system package
# manager. Prints "deb" or "rpm" and returns 0 when owned; returns 1 otherwise.
package_manager_owner() {
    bin="$1"
    if has_cmd dpkg && dpkg -S "$bin" >/dev/null 2>&1; then
        printf 'deb\n'
        return 0
    fi
    if has_cmd rpm && rpm -qf "$bin" >/dev/null 2>&1; then
        printf 'rpm\n'
        return 0
    fi
    return 1
}

# Remove the portzero package through the system package manager so its on-disk
# files AND its package-database entry go away together. Deleting package-owned
# files by hand (the --force flow) leaves the package "installed" but broken,
# forcing the user to run apt/dnf themselves afterwards.
remove_via_package_manager() {
    owner="$1"
    case "$owner" in
        deb)
            if has_cmd apt-get; then
                info "portzero was installed from a .deb; removing it with apt-get."
                sudo_if_needed apt-get remove -y portzero && return 0
            fi
            info "portzero was installed from a .deb; removing it with dpkg."
            sudo_if_needed dpkg --remove portzero && return 0
            ;;
        rpm)
            if has_cmd dnf; then
                info "portzero was installed from an .rpm; removing it with dnf."
                sudo_if_needed dnf remove -y portzero && return 0
            elif has_cmd yum; then
                info "portzero was installed from an .rpm; removing it with yum."
                sudo_if_needed yum remove -y portzero && return 0
            fi
            info "portzero was installed from an .rpm; removing it with rpm."
            sudo_if_needed rpm -e portzero && return 0
            ;;
    esac
    return 1
}

info "Stopping the tray companion and desktop app, and removing launcher entries"
if has_cmd pkill; then
    pkill -x portzero-tray >/dev/null 2>&1 || true
    pkill -x portzero-app >/dev/null 2>&1 || true
fi
remove_file "${HOME}/.config/autostart/portzero-tray.desktop"
# The desktop-app launcher (usr/share/applications/portzero.desktop) is
# package-managed for .deb/.rpm installs; clean up any user-level copy a manual
# install may have dropped.
remove_file "${HOME}/.local/share/applications/portzero.desktop"

info "Stopping daemon and removing autostart service"
run_portzero_best_effort "autostart disable" || true
run_portzero_best_effort "stop" || true
if has_cmd systemctl; then
    systemctl --user disable --now portzero-daemon.service >/dev/null 2>&1 || true
fi
remove_file "${HOME}/.config/systemd/user/portzero-daemon.service"
if has_cmd systemctl; then
    systemctl --user daemon-reload >/dev/null 2>&1 || true
fi

if bin_path="$(first_existing_portzero 2>/dev/null)"; then
    info "Removing local CA certificate from system trust store"
    sudo_if_needed env HOME="$HOME" "$bin_path" trust uninstall >/dev/null 2>&1 ||
        warn "Could not remove the local CA from every trust store"

    if has_cmd setcap; then
        info "Removing Linux capabilities from $bin_path"
        sudo_if_needed setcap -r "$bin_path" >/dev/null 2>&1 || true
    fi
else
    warn "portzero binary not found; skipping CLI-driven cleanup"
fi

remove_hosts_pin
remove_file /etc/polkit-1/rules.d/50-portzero-resolved.rules

# Remove the portzero and tray binaries. When they came from a .deb/.rpm,
# deleting them by hand would desync the package database and leave the package
# half-installed (still listed by dpkg/rpm, but non-functional), so the user
# would still have to run apt/dnf to finish the job. Hand off to the package
# manager instead — unless --force was given, which restores the old manual
# file-by-file removal as an escape hatch.
owner=""
if [ "$force" -eq 0 ] && owned_bin="$(first_existing_portzero 2>/dev/null)"; then
    owner="$(package_manager_owner "$owned_bin" 2>/dev/null || true)"
fi

if [ -n "$owner" ]; then
    if remove_via_package_manager "$owner"; then
        info "Removed the portzero package via the system package manager."
    else
        warn "Could not remove the portzero package automatically."
        case "$owner" in
            deb) warn "Remove it manually: sudo apt remove portzero" ;;
            rpm) warn "Remove it manually: sudo dnf remove portzero" ;;
        esac
    fi
else
    for candidate in $(portzero_candidates); do
        remove_file "$candidate"
    done

    for candidate in $(portzero_tray_candidates); do
        remove_file "$candidate"
    done

    for candidate in $(portzero_app_candidates); do
        remove_file "$candidate"
    done
fi

# The uninstall helper (~/.portzero/bin/portzero-uninstall) is never part of a
# .deb/.rpm — only the curl installer drops it — so always clean it up.
for candidate in $(portzero_uninstall_candidates); do
    remove_file "$candidate"
done

info "portzero uninstalled."
