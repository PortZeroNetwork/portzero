<!--
  Canonical, published copy of this notice lives at portzero.net/docs, sourced
  from portzero-cloud's cloud/landing/blog/src/data/docs/. Keep the two in sync:
  when the client changes what it transmits or stores, update both this file and
  the portzero.net/docs page.
-->

# Port Zero privacy notice (client)

This notice describes exactly what the Port Zero client on your machine sends to
Port Zero servers, and what it stores locally. It covers the `portzero` command
and its background daemon only. It is not a substitute for the
[Terms of Service](https://portzero.net/terms).

Port Zero Local (`*.portzero.local`) tunnels are entirely on your machine and do
**not** transmit anything to Port Zero servers. The data described under
"What the client transmits" is sent **only** when you have logged in
(`portzero login`) and are using **Cloud** tunnels (`*.tunnel.portzero.cloud`).

## What the client transmits to Port Zero servers

When the daemon connects to the cloud edge server on your behalf (Cloud tunnels
only), it sends:

- **Account auth token** — proves the connection belongs to your account.
- **`machine_id`** — your machine's hostname plus a random suffix, used to tell
  your machines apart. It is generated in memory each run and is not stored on
  disk.
- **Operating system** — the OS the client is running on (e.g. `linux`,
  `macos`, `windows`).
- **Client version** and the protocol version.

For each Cloud tunnel you expose, the client additionally sends **route
metadata** so you can review, in the dashboard, exactly what a tunnel points at
before it is made publicly reachable:

- **Hostname** of the machine running the daemon.
- **Working directory** (`cwd`) of the process behind the tunnel.
- **Executable path** and short **process name** of that process.
- **Full command line** (`argv`) of that process.

This metadata is best-effort — any field may be absent depending on operating
system permissions.

Finally, while a Cloud tunnel is up, the client forwards **the tunneled traffic
itself** (the HTTP requests/responses or raw TCP bytes for that tunnel) through
Port Zero's edge server to reach your process, exactly as any reverse tunnel
does.

Port Zero Local tunnels transmit none of the above: discovery, DNS, and
forwarding all happen on your own machine.

## What the client stores locally

The client keeps state under `~/.portzero/`:

- `~/.portzero/auth.json` — your account credentials (email, auth token,
  account id, username). Written with owner-only permissions.
- `~/.portzero/config.toml` — your local daemon preferences (not personal data).
- `~/.portzero/cost/store.json` — agent-session cost **metadata** (token counts,
  model ids, dollar figures, commit SHAs, timestamps), only for repositories you
  turned cost tracking on for. It never holds prompt, code, or session content.
- `~/.portzero/daemon/` — daemon runtime state:
  - `observations.json` — observed tunnel edges and exercised routes.
  - `daemon.log` — the daemon's log output.
  - `routes.json`, `overlay.json`, `issues.json`, `auto_open.json` — discovered
    routes and overlay/notification state.
  - `cloud_state.json`, `cloud_route_status.json` — cloud connection and
    per-tunnel review status.
  - `diagnostics.json` — the last `portzero doctor` diagnostics report.
  - `daemon.pid` — the running daemon's process id.

## Erasing your local data

Logging out removes only your credentials:

```sh
portzero logout            # removes ~/.portzero/auth.json
```

To erase **all** local personal data listed above (credentials, cost metadata,
observations, logs, and daemon state), stop the daemon and run:

```sh
portzero stop
portzero purge
```

`portzero purge` prints exactly which files it removed and where. It refuses to
run while the daemon is still running, so it never deletes state files out from
under a live daemon. It does not delete anything on Port Zero servers — to
delete your account and server-side data, use the dashboard at
[app.portzero.cloud](https://app.portzero.cloud) or contact support via
[portzero.net](https://portzero.net).
