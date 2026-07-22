# Running Port Zero in a cloud coding-agent environment

Primary target: **Claude Code on the web** and similar headless Linux agent
sandboxes (also applies to other rootful containerized agent runners).

Use `scripts/cloud-agent-env-install.sh` to bring up **Local** (`*.portzero.local`)
tunnels inside such a session. Cloud tunnels need `portzero login` afterwards.

## Why a dedicated installer

The stock `https://portzero.net/install.sh` bootstraps the full desktop Linux
experience: tray + app, a systemd unit for the daemon, and the scoped
`.portzero.local` resolver wired into **systemd-resolved**. A cloud agent
sandbox has none of the surrounding platform:

- **No systemd** — PID 1 is not systemd and there is no user D-Bus, so the
  autostart unit and `systemd-resolved` hook cannot be used. The daemon and the
  resolver must be plain background processes.
- **No browser** — login must use the print-a-URL-and-poll flow (or
  `--interactive`), never a local callback.
- **But rootful with `/dev/net/tun`** — so the TUN overlay itself works once the
  binary carries `cap_net_admin,cap_net_bind_service`.

The one real gap is name resolution. With no systemd-resolved to route
`*.portzero.local` to the daemon's embedded DNS (`10.254.0.1:53`), subdomains do
not resolve even though the overlay is up. The installer fills this with a tiny
stdlib **split-DNS forwarder** on `127.0.0.1:53` that sends `*.portzero.local` to
the daemon and everything else to the real upstream, then points
`/etc/resolv.conf` at it (original saved as `/etc/resolv.conf.pz-backup`).

## Usage

```sh
sudo bash scripts/cloud-agent-env-install.sh
```

Idempotent — safe to run at the top of every session (e.g. from a Claude Code
`SessionStart` hook). Env knobs:

- `PORT_ZERO_VERSION` — pin a release tag (default: latest).
- `PZ_UPSTREAM_DNS` — override the upstream resolver (default: autodetected from
  the pre-existing `resolv.conf`).

Expose a service (set `PZ_TUNNEL` **before** launch, bind to port `0`):

```sh
PZ_TUNNEL=web.portzero.local:80 python3 -m http.server 0
curl --noproxy '*' http://web.portzero.local/
```

**Proxy gotcha:** cloud agent egress usually goes through an HTTPS proxy
(`HTTPS_PROXY`). Pass `--noproxy '*'` (or set `NO_PROXY=.portzero.local`) so
requests to internal tunnel names skip the proxy — otherwise the proxy tries to
resolve the name and returns 502.

## Cloud tunnels

For internet-reachable `*.<username>.tunnel.portzero.cloud` tunnels, run
`portzero login`. In a headless session the default flow prints a URL to open on
any device and polls for completion; there is no token env var, so the only
unattended alternative is pre-seeding `~/.portzero/auth.json`. Cloud tunnels also
require an active subscription. Whether the edge WebSocket data plane traverses a
given environment's proxy still needs to be verified per environment.
