class Portzero < Formula
  desc "Eliminate port conflicts in local dev environments with virtual NIC port forwarding"
  homepage "https://portzero.net"
  version "0.1.0"
  license "GPL-3.0-or-later"

  on_macos do
    on_arm do
      url "https://github.com/PortZeroNetwork/portzero/releases/download/v#{version}/portzero-darwin-arm64.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000" # arm64
    end
    on_intel do
      url "https://github.com/PortZeroNetwork/portzero/releases/download/v#{version}/portzero-darwin-amd64.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000" # x86_64
    end
  end

  def install
    bin.install "portzero"

    # macOS ships the two GUI programs as .app bundles, not loose binaries.
    # A bare Mach-O file has no Info.plist, so macOS has nowhere to read an icon
    # or an activation policy from: both showed the generic "exec" icon, and the
    # tray took a Dock tile despite having no window. Only running from inside a
    # bundle fixes that, so `bin` gets shims that exec into the bundle rather
    # than copies of the binaries (a copy would be unbundled again, and would
    # double the install size).
    #
    # The shims point at `opt_prefix`, not `prefix`: opt_prefix is the
    # version-stable path Homebrew re-points on upgrade, so the LaunchAgent
    # `sudo portzero setup` writes keeps working after `brew upgrade`.
    if File.exist?("PortZero.app")
      prefix.install "PortZero.app"
      prefix.install "PortZero Tray.app"

      {
        "portzero-app"  => "PortZero.app/Contents/MacOS/portzero-app",
        "portzero-tray" => "PortZero Tray.app/Contents/MacOS/portzero-tray",
      }.each do |name, target|
        (bin/name).write <<~SH
          #!/bin/sh
          exec "#{opt_prefix}/#{target}" "$@"
        SH
        chmod 0755, bin/name
      end
    else
      # Non-macOS tarballs (and pre-bundle builds) ship loose binaries.
      # System-tray companion: a small GUI showing daemon/tunnel health with
      # start/restart/stop controls.
      bin.install "portzero-tray" if File.exist?("portzero-tray")
      # Desktop app: a Tauri window for managing local tunnels/services.
      bin.install "portzero-app" if File.exist?("portzero-app")
    end
  end

  # No post_install hook on purpose. Homebrew runs post_install with HOME
  # pointed at a throwaway temp directory, so anything written to
  # ~/Library/LaunchAgents or ~/Library/Application Support lands in
  # /private/tmp/portzero-postinstall-*/ and is deleted with it. A formula that
  # generated the CA and installed the tray login agent here looked correct and
  # shipped neither. Both now happen in `sudo portzero setup`, which runs as the
  # real user with a real HOME.

  def caveats
    <<~EOS
      macOS: unsigned builds and Gatekeeper.
      These binaries are not yet Apple-signed or notarized. Homebrew-installed
      binaries are normally not quarantined, so they launch fine. If you ever
      copy one in from a browser download and macOS blocks it ("cannot be opened
      because the developer cannot be verified"), clear the flag with
      `xattr -d com.apple.quarantine <path>`, or right-click it and choose Open.

      To complete setup, run:

        sudo portzero setup

      This command documents each action before it runs, then:
        - installs the CA certificate to your system keychain so browsers trust
          *.portzero.local HTTPS
        - installs and starts the root LaunchDaemon
        - installs the scoped resolver (/etc/resolver/portzero.local) so *.portzero.local
          names resolve to the PortZero DNS server
        - pins portzero.local in /etc/hosts so your browser can reach the dashboard

      macOS mDNSResponder intercepts all *.local names before the PortZero resolver
      is consulted, so subdomains need the /etc/resolver entry and the management
      dashboard needs the static hosts entry.

      Setup opens the PortZero desktop app for you once the daemon answers. That
      app is the management UI — run an example from its Getting Started section.
      Reopen it any time with `portzero-app`, or from the tray's "Open PortZero".

      (There is still a browser dashboard at http://portzero.local, but the
      desktop app replaces it and is where new features land.)

      A system-tray companion ships with this formula. It shows daemon/tunnel
      health and offers start/restart/stop controls. `sudo portzero setup`
      registers it to start at login via
      ~/Library/LaunchAgents/cloud.portzero.tray.plist. To stop it:
        launchctl bootout gui/$(id -u)/cloud.portzero.tray

      Manual equivalents:
        sudo HOME="$HOME" portzero trust install
        sudo portzero autostart enable
        sudo mkdir -p /etc/resolver
        printf 'nameserver 127.0.0.1\nport 10053\n' | sudo tee /etc/resolver/portzero.local
        echo '10.254.0.2 portzero.local # portzero-local' | sudo tee -a /etc/hosts

      The running daemon also re-creates /etc/resolver/portzero.local automatically
      if it is ever removed, and notifies you when that happens. It periodically
      re-verifies the other setup steps too (CA trust, the LaunchDaemon, and the
      portzero.local hosts pin); if one regresses out-of-band it alerts you with a
      desktop notification pointing at `sudo portzero setup`.

      To stop/remove autostart: sudo portzero autostart disable
      Do not use `brew services`; it cannot pin HOME correctly for a root daemon.
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/portzero --version")
  end
end
