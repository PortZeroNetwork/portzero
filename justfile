set windows-shell := ["powershell.exe", "-NoProfile", "-Command"]
import? 'vmtest/justfile'

default:
    @just --list

# Refresh installer/getting-started.json from ../portzero-examples.
[unix]
examples-docs:
    CARGO_TARGET_DIR=/tmp/portzero-target cargo run -p portzero-daemon --bin generate-examples-docs

[windows]
examples-docs:
    $env:CARGO_TARGET_DIR = Join-Path $env:TEMP "portzero-target"
    cargo run -p portzero-daemon --bin generate-examples-docs

# Install the daemon and CLI from source, grant privileges, and start.
#
# Linux:  grants CAP_NET_ADMIN (one sudo prompt), installs a systemd user
#         service for autostart, then starts the daemon.
# macOS:  installs a root LaunchDaemon (one sudo prompt) that starts
#         immediately and on every boot. No separate `portzero start` needed.
# Windows: installs a scheduled task for autostart. Must be run from an
# Administrator terminal; instructions are printed if not elevated.
[script('powershell.exe', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File')]
[windows]
install:
    & ".\scripts\windows-install.ps1"

[unix]
install:
    #!/usr/bin/env bash
    set -euo pipefail
    has_cmd() { command -v "$1" >/dev/null 2>&1; }
    install_linux_certutil() {
      if has_cmd certutil; then
        return 0
      fi

      echo "→ Installing NSS certutil so browsers trust *.portzero.local..."
      if has_cmd apt-get; then
        sudo env DEBIAN_FRONTEND=noninteractive apt-get update >/dev/null 2>&1 \
          && sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y libnss3-tools >/dev/null 2>&1 \
          && return 0
      elif has_cmd dnf; then
        sudo dnf install -y nss-tools >/dev/null 2>&1 && return 0
      elif has_cmd yum; then
        sudo yum install -y nss-tools >/dev/null 2>&1 && return 0
      elif has_cmd zypper; then
        sudo zypper --non-interactive install mozilla-nss-tools >/dev/null 2>&1 && return 0
      elif has_cmd pacman; then
        sudo pacman -S --noconfirm --needed nss >/dev/null 2>&1 && return 0
      elif has_cmd apk; then
        sudo apk add nss-tools >/dev/null 2>&1 && return 0
      fi

      echo "warning: Could not install NSS certutil automatically." >&2
      echo "warning: Browsers with NSS stores may not trust *.portzero.local until you install libnss3-tools (Debian/Ubuntu) or nss-tools (Fedora/RHEL)." >&2
      return 1
    }
    install_linux_resolved_polkit_rule() {
      if [ ! -d /etc/polkit-1/rules.d ]; then
        echo "warning: polkit rules directory not found; systemd-resolved may prompt for DNS setup at login." >&2
        return 1
      fi

      local user
      user="$(id -un)"
      case "$user" in
        *[!A-Za-z0-9._-]*|'')
          echo "warning: Cannot install polkit rule for unsupported username: $user" >&2
          return 1
          ;;
      esac

      local rule_tmp
      rule_tmp="$(mktemp)"
      trap 'rm -f "$rule_tmp"' RETURN
      {
        printf '%s\n' '// Managed by portzero installer.'
        printf '%s\n' "// Allows the portzero user service for $user to attach scoped DNS settings"
        printf '%s\n' '// to its TUN link without interactive authentication on every login.'
        printf '%s\n' 'polkit.addRule(function(action, subject) {'
        printf '%s\n' "    if (subject.user !== \"$user\") {"
        printf '%s\n' '        return polkit.Result.NOT_HANDLED;'
        printf '%s\n' '    }'
        printf '%s\n' ''
        printf '%s\n' '    if (action.id === "org.freedesktop.resolve1.set-dns-servers" ||'
        printf '%s\n' '        action.id === "org.freedesktop.resolve1.set-domains" ||'
        printf '%s\n' '        action.id === "org.freedesktop.resolve1.revert") {'
        printf '%s\n' '        return polkit.Result.YES;'
        printf '%s\n' '    }'
        printf '%s\n' ''
        printf '%s\n' '    return polkit.Result.NOT_HANDLED;'
        printf '%s\n' '});'
      } > "$rule_tmp"

      sudo install -m 0644 "$rule_tmp" /etc/polkit-1/rules.d/50-portzero-resolved.rules
    }
    open_dashboard() {
      local url="http://portzero.local"

      case "$(uname -s)" in
        Darwin*)
          if has_cmd open; then
            open "$url" >/dev/null 2>&1 && return 0
          fi
          ;;
        Linux*)
          if has_cmd xdg-open; then
            xdg-open "$url" >/dev/null 2>&1 && return 0
          fi
          if has_cmd gio; then
            gio open "$url" >/dev/null 2>&1 && return 0
          fi
          for opener in gnome-open kde-open5 kde-open sensible-browser; do
            if has_cmd "$opener"; then
              "$opener" "$url" >/dev/null 2>&1 && return 0
            fi
          done
          ;;
      esac

      echo "warning: Could not open $url automatically." >&2
      return 1
    }

    cargo install --path client/crates/cli
    cargo_bin="$HOME/.cargo/bin/portzero"
    active="$(command -v portzero 2>/dev/null || true)"
    if [ -n "$active" ] && [ "$active" != "$cargo_bin" ]; then
        echo "Copying $cargo_bin -> $active"
        rm -f "$active" && cp "$cargo_bin" "$active"
    fi

    # System-tray companion: a small GUI that shows daemon/tunnel health, starts
    # the daemon if it isn't running, and offers start/restart/stop + HTTPS
    # control. Best-effort — a tray build failure never aborts the daemon install.
    echo "→ Building and installing the system-tray companion (portzero-tray)..."
    cargo install --path client/crates/tray \
      || echo "warning: portzero-tray build failed; continuing without the tray." >&2
    cargo_bin_tray="$HOME/.cargo/bin/portzero-tray"

    OS="$(uname -s)"
    case "$OS" in
      Linux*)
        echo "→ Granting CAP_NET_ADMIN + CAP_NET_BIND_SERVICE..."
        echo "  (CAP_NET_ADMIN creates the TUN device; CAP_NET_BIND_SERVICE lets the"
        echo "   embedded DNS server bind 10.254.0.1:53 so *.portzero.local resolves.)"
        sudo setcap 'cap_net_admin,cap_net_bind_service+eip' "$cargo_bin"
        echo "→ Installing systemd-resolved polkit rule..."
        install_linux_resolved_polkit_rule
        install_linux_certutil || true
        echo "→ Generating CA certificate..."
        portzero trust generate
        echo "→ Installing CA certificate to system trust store..."
        sudo HOME="$HOME" "$cargo_bin" trust install
        echo "→ Installing systemd user service for autostart..."
        portzero autostart enable
        # Pin the bare dashboard name in /etc/hosts. nss-mdns (the
        # `mdns4_minimal [NOTFOUND=return]` entry in /etc/nsswitch.conf) claims
        # 2-label *.local names like `portzero.local` and halts the lookup before
        # systemd-resolved is consulted, so the dashboard name would never resolve
        # even though the overlay DNS server answers it. The management dashboard
        # lives at a fixed VIP (10.254.0.2), so a static hosts entry — resolved by
        # `files`, ahead of mdns — is the robust fix. Multi-label service names
        # (e.g. app.portzero.local) are unaffected and resolve via the overlay DNS.
        echo "→ Pinning portzero.local in /etc/hosts (works around nss-mdns)..."
        if ! grep -q '# portzero-local' /etc/hosts 2>/dev/null; then
          echo '10.254.0.2 portzero.local # portzero-local' | sudo tee -a /etc/hosts >/dev/null
        fi
        echo "→ Starting daemon..."
        portzero start --no-browser
        open_dashboard || true
        if [ -x "$cargo_bin_tray" ]; then
          echo "→ Installing tray autostart (XDG) and launching it..."
          mkdir -p "$HOME/.config/autostart"
          {
            printf '%s\n' '[Desktop Entry]'
            printf '%s\n' 'Type=Application'
            printf '%s\n' 'Name=PortZero Tray'
            printf '%s\n' 'Comment=PortZero daemon status and controls'
            printf '%s\n' "Exec=$cargo_bin_tray"
            printf '%s\n' 'X-GNOME-Autostart-enabled=true'
            printf '%s\n' 'NoDisplay=false'
          } > "$HOME/.config/autostart/portzero-tray.desktop"
          if [ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]; then
            ( "$cargo_bin_tray" >/dev/null 2>&1 & )
          else
            echo "  (no desktop session detected; the tray will start at your next login)"
          fi
        fi
        ;;
      Darwin*)
        echo "→ Generating CA certificate for *.portzero.local HTTPS..."
        "$cargo_bin" trust generate
        echo "→ Installing CA certificate to system keychain..."
        # Pass HOME explicitly: sudo resets HOME to /var/root but the cert is in
        # the user's Library/Application Support/PortZero/ directory.
        sudo HOME="$HOME" "$cargo_bin" trust install
        echo "→ Installing root LaunchDaemon (required for utun/TUN access on macOS)..."
        # SUDO_USER is set by sudo; install_launchd reads it to pin HOME in the
        # plist so the daemon's state files land in the user's home rather than
        # /var/root — this makes 'portzero status' work without sudo.
        sudo "$cargo_bin" autostart enable
        # Pin portzero.local in /etc/hosts. macOS mDNSResponder claims authority
        # for all *.local names and intercepts them before /etc/resolver/ is
        # consulted, so portzero.local never reaches our embedded DNS server.
        # The management dashboard lives at a fixed VIP (10.254.0.2); a hosts
        # entry is resolved by the 'files' source before mDNS and fixes this.
        # Multi-label service names (e.g. app.portzero.local) resolve fine via
        # the /etc/resolver/portzero.local scoped resolver.
        echo "→ Pinning portzero.local in /etc/hosts (works around mDNS interception)..."
        if ! grep -q '# portzero-local' /etc/hosts 2>/dev/null; then
          echo '10.254.0.2 portzero.local # portzero-local' | sudo tee -a /etc/hosts >/dev/null
        fi
        echo "→ Opening dashboard..."
        open_dashboard || true
        echo "→ Daemon installed and started via LaunchDaemon."
        if [ -x "$cargo_bin_tray" ]; then
          echo "→ Installing tray LaunchAgent (per-user) and launching it..."
          mkdir -p "$HOME/Library/LaunchAgents"
          tray_plist="$HOME/Library/LaunchAgents/cloud.portzero.tray.plist"
          {
            printf '%s\n' '<?xml version="1.0" encoding="UTF-8"?>'
            printf '%s\n' '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">'
            printf '%s\n' '<plist version="1.0"><dict>'
            printf '%s\n' '  <key>Label</key><string>cloud.portzero.tray</string>'
            printf '%s\n' '  <key>ProgramArguments</key>'
            printf '%s\n' "  <array><string>$cargo_bin_tray</string></array>"
            printf '%s\n' '  <key>RunAtLoad</key><true/>'
            printf '%s\n' '  <key>KeepAlive</key><true/>'
            printf '%s\n' '</dict></plist>'
          } > "$tray_plist"
          launchctl unload "$tray_plist" 2>/dev/null || true
          launchctl load "$tray_plist" 2>/dev/null || true
        fi
        ;;
      MINGW*|MSYS*|CYGWIN*)
        echo "→ Installing scheduled task for autostart..."
        portzero autostart enable || {
          echo ""
          echo "  Failed — re-run 'just install' from an Administrator terminal,"
          echo "  or run these commands as Administrator:"
          echo "    portzero autostart enable"
          echo "    portzero start"
          exit 1
        }
        portzero start
        ;;
      *)
        echo "Unknown platform. Run 'portzero autostart enable' and 'portzero start' manually."
        ;;
    esac
    echo ""
    echo "✓ portzero installed. Open http://portzero.local in your browser."

# Uninstall the CLI, stop the daemon, and remove the autostart service.
[script('powershell.exe', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File')]
[windows]
uninstall:
    & ".\scripts\windows-uninstall.ps1"

[unix]
uninstall:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "→ Stopping daemon and removing autostart service..."
    portzero autostart disable 2>/dev/null || true
    portzero stop 2>/dev/null || true

    cargo_bin="$HOME/.cargo/bin/portzero"
    OS="$(uname -s)"
    case "$OS" in
      Linux*)
        echo "→ Removing CA certificate from system trust store..."
        sudo HOME="$HOME" "$cargo_bin" trust uninstall 2>/dev/null || true
        if [ -f "$cargo_bin" ]; then
          echo "→ Removing capabilities (CAP_NET_ADMIN, CAP_NET_BIND_SERVICE)..."
          sudo setcap -r "$cargo_bin" 2>/dev/null || true
        fi
        if grep -q '# portzero-local' /etc/hosts 2>/dev/null; then
          echo "→ Removing portzero.local pin from /etc/hosts..."
          sudo sed -i '/# portzero-local/d' /etc/hosts
        fi
        if [ -f /etc/polkit-1/rules.d/50-portzero-resolved.rules ]; then
          echo "→ Removing systemd-resolved polkit rule..."
          sudo rm -f /etc/polkit-1/rules.d/50-portzero-resolved.rules
        fi
        ;;
      Darwin*)
        echo "→ Removing root LaunchDaemon..."
        sudo "$cargo_bin" autostart disable 2>/dev/null || true
        echo "→ Removing CA certificate from system keychain..."
        sudo HOME="$HOME" "$cargo_bin" trust uninstall 2>/dev/null || true
        if grep -q '# portzero-local' /etc/hosts 2>/dev/null; then
          echo "→ Removing portzero.local pin from /etc/hosts..."
          sudo sed -i '' '/# portzero-local/d' /etc/hosts
        fi
        ;;
    esac

    cargo uninstall portzero-cli 2>/dev/null || true
    active="$(command -v portzero 2>/dev/null || true)"
    if [ -n "$active" ]; then
        echo "→ Removing $active"
        rm -f "$active"
    fi
    echo "✓ portzero uninstalled."

# Run the full unprivileged test suite (no root required).
# The privileged real-TUN/Wintun e2e (`real_tun_overlay`) skips cleanly here
# unless explicitly opted in with PORTZERO_REQUIRE_REAL_TUN_E2E=1.
[unix]
test:
    CARGO_TARGET_DIR=/tmp/portzero-target cargo test --workspace

[windows]
test:
    $env:CARGO_TARGET_DIR = Join-Path $env:TEMP "portzero-target"
    cargo test --workspace

# Run the CI e2e overlay step for this OS.
#
# Linux/macOS: opts in and runs under sudo so the real-TUN path can create the device.
# Windows: prepares wintun.dll, opts in, then runs the real Wintun overlay path.
# Use together with `just verify` for current-OS CI parity.
[unix]
e2e:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo_bin="$(command -v cargo)"
    sudo env \
      "PATH=$PATH" \
      "HOME=$HOME" \
      "CARGO_HOME=${CARGO_HOME:-$HOME/.cargo}" \
      "RUSTUP_HOME=${RUSTUP_HOME:-$HOME/.rustup}" \
      PORTZERO_REQUIRE_REAL_TUN_E2E=1 \
      "$cargo_bin" test -p portzero-daemon --test overlay_e2e real_tun_overlay -- --nocapture

[script('powershell.exe', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File')]
[windows]
e2e:
    $ErrorActionPreference = "Stop"
    $wintunDir = Join-Path (Get-Location) ".wintun"
    $wintunDll = Join-Path $wintunDir "wintun.dll"
    fltmc filters *> $null
    if ($LASTEXITCODE -ne 0) {
    Write-Error "just e2e requires an elevated Administrator PowerShell on Windows."
    exit 1
    }
    if (-not (Test-Path $wintunDll)) {
    New-Item -ItemType Directory -Force -Path $wintunDir | Out-Null
    $zip = Join-Path $wintunDir "wintun.zip"
    Invoke-WebRequest -Uri "https://www.wintun.net/builds/wintun-0.14.1.zip" -OutFile $zip -TimeoutSec 60
    Expand-Archive -Path $zip -DestinationPath $wintunDir -Force
    Copy-Item (Join-Path $wintunDir "wintun\bin\amd64\wintun.dll") $wintunDll -Force
    }
    $env:PATH = "$wintunDir;$env:PATH"
    $env:PORTZERO_REQUIRE_REAL_TUN_E2E = "1"
    cargo test -p portzero-daemon --test overlay_e2e real_tun_overlay -- --nocapture
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# =============================================================================
# Local checks & CI parity (run these to avoid wasting GitHub Actions minutes)
# =============================================================================

# Check formatting (fast, used by hooks).
fmt-check:
    cargo fmt -- --check

# Strict clippy (matches CI "Check & Test" job exactly). This is the one that
# recently failed on GitHub Actions.
[unix]
clippy:
    CARGO_TARGET_DIR=/tmp/portzero-target cargo clippy --workspace -- -D warnings -D clippy::cognitive_complexity

[windows]
clippy:
    $env:CARGO_TARGET_DIR = Join-Path $env:TEMP "portzero-target"
    cargo clippy --workspace -- -D warnings -D clippy::cognitive_complexity

# Clippy on tests + bins + examples + all features.
[unix]
clippy-all:
    CARGO_TARGET_DIR=/tmp/portzero-target cargo clippy --workspace --all-targets --all-features -- -D warnings -D clippy::cognitive_complexity

[windows]
clippy-all:
    $env:CARGO_TARGET_DIR = Join-Path $env:TEMP "portzero-target"
    cargo clippy --workspace --all-targets --all-features -- -D warnings -D clippy::cognitive_complexity

# Fast type check.
[unix]
check:
    CARGO_TARGET_DIR=/tmp/portzero-target cargo check --workspace

[windows]
check:
    $env:CARGO_TARGET_DIR = Join-Path $env:TEMP "portzero-target"
    cargo check --workspace

# Regenerate api/management-v1.yaml from the utoipa annotations on the
# management API handlers (client/crates/daemon/src/management). CI runs this
# and fails if the checked-in file has drifted, so the spec can't go stale.
[unix]
openapi:
    CARGO_TARGET_DIR=/tmp/portzero-target cargo run -p portzero-daemon --bin generate-openapi

[windows]
openapi:
    $env:CARGO_TARGET_DIR = Join-Path $env:TEMP "portzero-target"
    cargo run -p portzero-daemon --bin generate-openapi

# Enforce file-size/complexity budgets on client/*.rs (see docs/developers/complexity-budgets.md).
# Checks the whole tree by default; pass `--changed` to scope to staged files
# (used by the pre-commit hook, so it stays fast).
[unix]
complexity *ARGS:
    CARGO_TARGET_DIR=/tmp/portzero-target cargo run -p portzero-xtask --bin check-file-size-budget -- {{ARGS}}

[windows]
complexity *ARGS:
    $env:CARGO_TARGET_DIR = Join-Path $env:TEMP "portzero-target"
    cargo run -p portzero-xtask --bin check-file-size-budget -- {{ARGS}}

# -----------------------------------------------------------------------------
# Desktop app (client/crates/app, Tauri v2, binary `portzero-app`)
#
# The React/TypeScript frontend at client/crates/app/ui MUST be built before
# any cargo command touches the portzero-app crate — Tauri embeds ui/dist
# into the binary at build time via its build.rs, so a stale or missing
# ui/dist breaks `cargo build`/`clippy`/`test` for that crate, not just
# packaging. Linux additionally needs webkit2gtk/GTK/appindicator headers to
# even compile the crate — see `just app-linux-deps` below.
# -----------------------------------------------------------------------------

# Print (but do not run) the command to install portzero-app's Linux build
# dependencies. Not run automatically since it requires sudo — copy/paste it
# yourself once per machine.
[unix]
app-linux-deps:
    @echo "portzero-app (Tauri v2) needs these Linux packages to build:"
    @echo ""
    @echo "  sudo apt-get update"
    @echo "  sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev"
    @echo ""
    @echo "(Debian/Ubuntu package names; see AGENTS.md for the equivalent runtime-only packages.)"

# Build the app frontend (client/crates/app/ui) and typecheck it. Mirrors the
# frontend steps in .github/workflows/ci.yml so this is CI parity for the app
# crate, the same way `just verify` is for the rest of the workspace.
[unix]
app-frontend-check:
    #!/usr/bin/env bash
    set -euo pipefail
    cd client/crates/app/ui
    npm ci
    npm run typecheck --if-present
    npm run build

[windows]
app-frontend-check:
    Push-Location client/crates/app/ui
    npm ci
    npm run typecheck --if-present
    npm run build
    Pop-Location

# Build the frontend, then run portzero-app in dev mode. On Linux, run
# `just app-linux-deps` first if the build fails on missing webkit/GTK headers.
[unix]
app-dev:
    #!/usr/bin/env bash
    set -euo pipefail
    (cd client/crates/app/ui && npm ci && npm run build)
    cargo run -p portzero-app

[windows]
app-dev:
    Push-Location client/crates/app/ui
    npm ci
    npm run build
    Pop-Location
    cargo run -p portzero-app

# Build the frontend, then a release portzero-app binary. Mirrors the release
# workflow's build step (.github/workflows/release.yml) so this reproduces
# what CI ships.
[unix]
app-build:
    #!/usr/bin/env bash
    set -euo pipefail
    (cd client/crates/app/ui && npm ci && npm run build)
    cargo build --release -p portzero-app

[windows]
app-build:
    Push-Location client/crates/app/ui
    npm ci
    npm run build
    Pop-Location
    cargo build --release -p portzero-app

# Run the unprivileged local checks from the main CI job.
# Recommended before pushing. Follow with `just e2e` for current-OS CI parity.
# This still does not cover the other CI operating systems or release packaging.
verify:
    just app-frontend-check
    just fmt-check
    just clippy
    just test
    just complexity

# -----------------------------------------------------------------------------
# Git hooks setup (cross platform via lefthook)
#
# After cloning (or when you want to refresh on a new machine):
#     just install-hooks
#
# This enables:
#   - pre-commit : fmt check + file-size budget (changed files only)
#   - pre-push   : fmt + clippy (-D warnings) + unprivileged tests
#
# These are the same checks GitHub Actions runs. Catching clippy/test
# failures locally is much cheaper.
#
# IMPORTANT:
#   - Privileged tests (`just e2e`) are deliberately NOT included.
#     See explanation below and in docs/users/privileges.md.
#   - You can still bypass with `git push --no-verify` in emergencies.
# -----------------------------------------------------------------------------

# Install (or repair) git hooks using lefthook (cross-platform).
[unix]
install-hooks:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! command -v lefthook >/dev/null 2>&1; then
        echo "→ lefthook not found in PATH."
        echo ""
        echo "Install lefthook (one-time):"
        echo "  macOS:   brew install lefthook"
        echo "  Linux:   https://github.com/evilmartians/lefthook#install"
        echo "           (also available via many distro package managers)"
        echo ""
        echo "Then re-run this command:"
        echo "  just install-hooks"
        echo ""
        echo "After hooks are installed you will get pre-commit and pre-push checks."
        exit 1
    fi
    lefthook install
    echo ""
    echo "✓ lefthook git hooks installed."
    echo "   pre-commit : just fmt-check + just complexity --changed"
    echo "   pre-push   : just fmt-check + just clippy + just test"
    echo ""
    echo "Also run (recommended):"
    echo "  ticketry init     # sets up background ticket indexing hooks"
    echo ""
    echo "To skip hooks in a pinch:"
    echo "  git commit --no-verify"
    echo "  git push --no-verify"

[script('powershell.exe', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File')]
[windows]
install-hooks:
    $ErrorActionPreference = "Stop"
    if (-not (Get-Command lefthook -ErrorAction SilentlyContinue)) {
    Write-Host "→ lefthook not found on PATH."
    Write-Host ""
    Write-Host "Install lefthook (one-time per machine):"
    Write-Host "  winget: coming soon"
    Write-Host "  scoop install lefthook"
    Write-Host "  choco install lefthook"
    Write-Host "  or download the binary from:"
    Write-Host "  https://github.com/evilmartians/lefthook/releases"
    Write-Host ""
    Write-Host "After installing, re-run:"
    Write-Host "  just install-hooks"
    exit 1
    }
    lefthook install
    Write-Host ""
    Write-Host "✓ lefthook git hooks installed."
    Write-Host "   pre-commit : just fmt-check + just complexity --changed"
    Write-Host "   pre-push   : just fmt-check + just clippy + just test"
    Write-Host ""
    Write-Host "Also run (recommended):"
    Write-Host "  ticketry init     # sets up background ticket indexing hooks"
    Write-Host ""
    Write-Host "To skip hooks in a pinch:"
    Write-Host "  git commit --no-verify"
    Write-Host "  git push --no-verify"
