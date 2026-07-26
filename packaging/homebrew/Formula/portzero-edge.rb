class PortzeroEdge < Formula
  # Edge (prerelease) channel — builds cut off `staging` for testing installers.
  # NOT a supported release. Stable users install `portzero`, never this. The
  # release workflow (.github/workflows/release.yml) seeds this file into the
  # tap from packaging/homebrew/Formula/portzero-edge.rb and fills in the
  # version + sha256 on each prerelease. See docs/dev/prerelease-channel.md.
  desc "Prerelease (edge) build of portzero — for testing only"
  homepage "https://portzero.net"
  version "0.0.0"
  license "GPL-3.0-or-later"

  # Both formulae install the `portzero`/`portzero-tray`/`portzero-app`
  # binaries, so only one may be linked at a time. Testers `brew unlink
  # portzero` (or uninstall it) before installing edge, and vice-versa to
  # return to stable.
  conflicts_with "portzero", because: "both install the portzero binary"

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

    # Mirrors the stable formula: on macOS the GUI programs ship as .app bundles
    # (a loose binary has no Info.plist, so it gets the generic "exec" icon and
    # the tray gets a Dock tile it has no window for), and `bin` gets shims that
    # exec into the bundle via the version-stable opt_prefix. See
    # packaging/homebrew/Formula/portzero.rb for the full rationale.
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
      bin.install "portzero-tray" if File.exist?("portzero-tray")
      bin.install "portzero-app" if File.exist?("portzero-app")
    end
  end

  def post_install
    # Generate the local CA certificate (writes to ~/Library/Application Support/PortZero/).
    # Idempotent — existing certs are kept. Does not require elevated privileges.
    system "#{bin}/portzero", "trust", "generate"

    # Install a per-user LaunchAgent so the tray starts at login. Best-effort:
    # never fail the install, and only load it if a GUI session is present.
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
    <<~EOS
      This is a PRERELEASE (edge) build for testing. It connects to the same
      production portzero.cloud as stable. To return to stable:

        brew uninstall portzero-edge
        brew install portzero

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

      A system-tray companion is installed and set to start at
      login via ~/Library/LaunchAgents/cloud.portzero.tray.plist. It shows
      daemon/tunnel health and offers start/restart/stop controls. To stop it:
        launchctl unload ~/Library/LaunchAgents/cloud.portzero.tray.plist

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
