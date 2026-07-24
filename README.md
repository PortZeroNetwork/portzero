# Port Zero

Developers have all seen port conflict errors like this one:

```
Error: listen EADDRINUSE: address already in use :::3000
```

Port Zero is a developer tool that solves this problem once and for all. Port Zero has two parts:

- Port Zero Local solves this problem for traffic on a single computer, and is free & open source
- Port Zero Cloud solves this problem for devices on the LAN and over the Internet, and requires a subscription

Either way, you use Port Zero with your programs the same, whether they are a process or a Docker container. Configure all ports to 0 for any program you want Port Zero to manage; this tells the operating system to pick an available port at random. Then you start your programs with the `PZ_TUNNEL` environment variable. For example:

- If you specify `PZ_TUNNEL={branch}.mytodoapp.portzero.local:80`, that is a Local tunnel
- If you specify `PZ_TUNNEL={branch}--mytodoapp--<username>.tunnel.portzero.cloud:80`, that is a Cloud tunnel

The `PZ_TUNNEL` setting tells PortZero the domain name and port that clients should use.

With your program running, you can open `http://master.mytodoapp.portzero.local:80` in your browser. You might also be running a different version of your program in a separate git worktree. Port Zero supports this; `http://some-other-branch.mytodoapp.portzero.local:80` can be available at the same time without port conflicts. This doesn't just work for http; it works for *any* TCP protocol.

## How does this work?

Port Zero runs a background process on your local dev machine that scans for processes and Docker containers with the special `PZ_TUNNEL` environment variable. If `PZ_TUNNEL` ends in `--<username>.tunnel.portzero.cloud`, Port Zero opens a Cloud tunnel to `portzero.cloud` under that username's namespace. If on the other hand the `PZ_TUNNEL` contains `portzero.local`, Port Zero opens a Local tunnel and does four things:

1. Create a virtual network interface card (NIC) on your local machine if Port Zero has not already done so
2. Create a virtual IP address in this virtual NIC for that process
3. Create a virtual DNS record for that virtual IP address, based on the template specified in `PZ_TUNNEL`
4. Forward the port specified in `PZ_TUNNEL` on the virtual IP address to the randomly-assigned port on the actual process or Docker container

## Install

**macOS (Homebrew)**

```sh
brew tap PortZeroNetwork/portzero
brew install portzero
```

> **macOS: unsigned builds and Gatekeeper.** Port Zero binaries are not yet
> Apple-signed or notarized. Homebrew-installed binaries are normally *not*
> quarantined, so `brew install` launches fine. A tarball you download from a
> browser *is* quarantined — if macOS blocks it with "cannot be opened because
> the developer cannot be verified", clear the flag with
> `xattr -d com.apple.quarantine <path-to-binary>`, or right-click the binary
> in Finder and choose **Open** once to approve it.

**Linux**
```sh
curl -fsSL https://portzero.net/install.sh | sh
```

Offline, or want to pin a specific release? Run the release-asset installer directly instead:
```sh
curl -fsSL https://github.com/PortZeroNetwork/portzero/releases/latest/download/linux-install.sh | sh
```

Uninstall:
```sh
~/.portzero/bin/portzero-uninstall
```
Or run the release asset directly:
```sh
curl -fsSL https://github.com/PortZeroNetwork/portzero/releases/latest/download/linux-uninstall.sh | sh
```

**Windows**

Download `portzero-<version>-x86_64.msi` from the [latest GitHub release](https://github.com/PortZeroNetwork/portzero/releases/latest) and run it. (winget publishing is coming soon.)

The installer and daemon perform local setup automatically where supported.

## Getting started

After installing, see a working Local tunnel in one command:

```sh
portzero demo
```

This starts the daemon if needed, serves a tiny built-in page on port 0, and
opens `http://hello.portzero.local` in your browser once the tunnel is
reachable — no examples repo or runtime dependencies. Press Ctrl+C to stop.

Then start the daemon for day-to-day use:

```sh
portzero start
```

This launches the **PortZero app** — a native desktop app for checking
daemon and tunnel health, managing HTTPS, and running the bundled examples —
instead of opening a browser tab. Reopen it anytime from the system tray
icon's **Open PortZero** menu item, or run `portzero start --no-browser` to
start the daemon without launching the app.

The daemon still serves a browser-based dashboard at `http://portzero.local`
for debugging, but the app is the recommended way to use Port Zero day to
day. See [docs/developers/desktop-app.md](docs/developers/desktop-app.md) for
how the app is built, and [portzero.net/docs](https://portzero.net/docs) for
usage docs.

## Privacy

Port Zero Local (`*.portzero.local`) tunnels stay entirely on your machine.
Cloud tunnels (`*.tunnel.portzero.cloud`) transmit tunnel metadata and the
tunneled traffic to Port Zero's servers. For the exact list of what the client
sends and what it stores under `~/.portzero/`, see the
[privacy notice](docs/users/privacy.md) (canonical published copy at
[portzero.net/docs](https://portzero.net/docs)).

Log out to remove your credentials, or erase all local data with:

```sh
portzero stop
portzero purge
```

## Licensing

- **Port Zero Local** (tunnels using `*.portzero.local`): governed by the [GNU General Public License v3.0](LICENSE). See the [LICENSE](LICENSE) file.
- **Port Zero Cloud** features (tunnels using `tunnel.portzero.cloud`): governed by the [Terms of Service](https://portzero.net/terms).

By installing or using the software you agree to the terms applicable to the features you use.

## Contributing

We welcome contributions!
See [CONTRIBUTING.md](CONTRIBUTING.md) for the contribution process and development setup.

Internal development documentation lives under [`docs/developers/`](docs/developers/).
