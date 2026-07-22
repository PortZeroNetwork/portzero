# PortZero Tunnel Action

Install the PortZero local daemon on a GitHub Actions runner, wait for one or
more `PZ_TUNNEL`-tagged processes/containers to become reachable, and expose
their URLs as step outputs — one YAML block, no manual daemon/CA setup in your
workflow.

This action is for **ephemeral, single-job tunnels**: a test suite that needs
a real (HTTPS-capable) URL for the duration of one CI job. It is not for
long-lived preview environments — see
[`docs/review-apps.md`](../docs/review-apps.md) for that pattern, which
targets a persistent host instead of a hosted runner that's destroyed at the
end of the job.

## Usage

```yaml
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5

      - name: Start the app under test
        run: |
          export PZ_TUNNEL="ci-${{ github.run_id }}--myapp.portzero.local:443"
          docker compose up -d --build

      - name: Open tunnel(s) and wait for readiness
        id: tunnel
        uses: PortZeroNetwork/portzero/tunnel-action@staging
        with:
          tunnels: ci-${{ github.run_id }}--myapp.portzero.local

      - name: Run integration tests against the real HTTPS URL
        run: npm test
        env:
          BASE_URL: ${{ steps.tunnel.outputs.url }}

      - name: Teardown
        if: always()
        uses: PortZeroNetwork/portzero/tunnel-action@staging
        with:
          mode: teardown
```

> The `uses:` line above points at this repo's `staging` branch, where this
> action currently lives (`tunnel-action/` at the repo root). Publishing a
> dedicated `portzero/tunnel-action` repo — so consumers get a shorter
> `portzero/tunnel-action@v1`-style reference — is a follow-up outside this
> change; see [Status](#status) below.

This action **does not start your app**. Your own step (or an earlier one)
sets `PZ_TUNNEL` and launches the process/container, exactly as it would on a
laptop — portzero has no service concept, so the tunnel domain you chose is the
only identity the daemon (and this action) ever sees.

### A full HTTPS example

Binding a tunnel to `:443` gets you an HTTPS URL automatically — PortZero
terminates TLS at the edge using its own locally-trusted CA, so your backend
can stay plain HTTP:

```yaml
- name: Start the app under test
  run: |
    export PZ_TUNNEL="ci-${{ github.run_id }}--myapp.portzero.local:443"
    node server.js &

- id: tunnel
  uses: PortZeroNetwork/portzero/tunnel-action@staging
  with:
    tunnels: ci-${{ github.run_id }}--myapp.portzero.local

- run: curl -fsS "${{ steps.tunnel.outputs.url }}"   # https://…, no -k needed
```

See `portzero-examples`' `.github/workflows/tunnel-action-integration-test.yml`
for a runnable version of this against one of the checked-in example apps.

## Inputs

| Input      | Default   | Description |
|------------|-----------|--------------|
| `mode`     | `start`   | `start` installs portzero and waits for `tunnels`. `teardown` stops the daemon and prints its log. |
| `tunnels`  | *(empty)* | One `PZ_TUNNEL` domain per line to wait for. Required when `mode: start`. |
| `healthy`  | `true`    | Pass `--healthy` to `portzero wait` (polls `PZ_HEALTH_PATH` when declared). |
| `timeout`  | `60`      | Seconds to wait per tunnel (`portzero wait --timeout`). |
| `version`  | `latest`  | portzero release to install, without a leading `v` (e.g. `0.4.0`), or `latest`. |
| `oidc-team` | *(empty)* | portzero.cloud team slug. When set, exchanges the job's GitHub OIDC token for a short-lived cloud-tunnel credential — see [Cloud tunnels via GitHub OIDC](#cloud-tunnels-via-github-oidc). |
| `api-url`  | `https://app.portzero.cloud/api` | portzero.cloud API base for the OIDC exchange. Only change for non-production cloud deployments. |

## Outputs

| Output | Description |
|--------|-------------|
| `urls` | Newline-separated resolved URL per entry in `tunnels`, same order (from `portzero url`). |
| `url`  | Convenience: set only when `tunnels` had exactly one entry. |

## Cloud tunnels via GitHub OIDC

`*.tunnel.portzero.cloud` tunnels need a portzero.cloud credential. Instead of
putting a long-lived token in a repository secret, set `oidc-team` and the
action exchanges the job's **GitHub OIDC token** (audience `portzero.cloud`)
for a **short-lived, team-scoped** tunnel credential (1 hour TTL), minted
fresh per job and never written to the log:

```yaml
jobs:
  test:
    runs-on: ubuntu-latest
    permissions:
      id-token: write   # required — lets the job request a GitHub OIDC token
      contents: read
    steps:
      - uses: actions/checkout@v5

      - name: Start the app under test
        run: |
          export PZ_TUNNEL="ci-${{ github.run_id }}--myapp.myteam.tunnel.portzero.cloud:443"
          docker compose up -d --build

      - id: tunnel
        uses: PortZeroNetwork/portzero/tunnel-action@staging
        with:
          oidc-team: myteam
          tunnels: ci-${{ github.run_id }}--myapp.myteam.tunnel.portzero.cloud
```

Two pieces of one-time setup:

1. **Workflow**: `permissions: id-token: write` on the job (or workflow).
   Without it GitHub does not give the runner an OIDC endpoint and the action
   fails with an error telling you exactly that.
2. **portzero.cloud**: the team named in `oidc-team` must have an **OIDC trust
   rule** allowing this repository. Add one in the team's settings on
   portzero.cloud; a `403`/`404` from the exchange with no trust rule is the
   most common first-run failure. Docs: <https://portzero.net/docs>.

The minted credential is masked in logs (`::add-mask::`), written to
`~/.portzero/auth.json` (mode `600`) before the daemon starts, and expires on
its own — nothing to revoke after the job.

## Container jobs

`jobs.<id>.container: ...` runs your steps inside a Docker container. The
daemon still needs a TUN device and the network capabilities, so grant them in
the job definition and keep the container's default **root** user:

```yaml
jobs:
  test:
    runs-on: ubuntu-latest
    container:
      image: debian:bookworm
      options: >-
        --device /dev/net/tun
        --cap-add NET_ADMIN
        --cap-add NET_BIND_SERVICE
    steps:
      - uses: actions/checkout@v5
      # ... same usage as above ...
```

What the action does differently in a container:

- Verifies `/dev/net/tun` exists and the step runs as root — if not, it fails
  with the exact `container.options` line to add (do **not** pass `--user`;
  the default root user is required).
- Installs missing `curl`/`python3`/`ca-certificates` via `apt-get` when the
  image lacks them (non-Debian images must bake these in).
- Installs the daemon via the repo's headless-container installer
  (`scripts/cloud-agent-env-install.sh`): binary to `/usr/local/bin`,
  file capabilities via `setcap`, a tiny split-DNS forwarder so
  `*.portzero.local` resolves inside the container, and a direct (no-systemd)
  daemon start. The `version` input is honored here too.
- `mode: teardown` additionally stops that DNS forwarder and restores
  `/etc/resolv.conf` from its backup, so re-runs on a reused container stay
  deterministic.

Outputs and the wait/health behavior are identical to the VM path.

## Teardown: why it's a second, explicit step

Composite actions (`runs.using: composite`) have no `post:` hook — only
JavaScript and Docker actions support one. So `mode: teardown` is a step you
add yourself, guarded by `if: always()`, rather than something this action
runs automatically when the job ends. On GitHub-hosted runners the VM is
destroyed after the job anyway, so teardown here is about a clean log trail on
failure (and not leaking a daemon across jobs if you run this on a
self-hosted, reused runner).

## Limitations

- **Linux only** (e.g. `ubuntu-latest`), either directly on the runner VM or
  in a container-based job. Local tunnels need a real TUN device and
  `CAP_NET_ADMIN`/`CAP_NET_BIND_SERVICE` (or root) — the VM path gets these
  via the runner's passwordless `sudo`; the container path needs them granted
  explicitly (see [Container jobs](#container-jobs)).
- **Container jobs need `--device /dev/net/tun`, the two `--cap-add`s, and
  the default root user.** Without them the action fails fast with the exact
  `container.options` line to add — it cannot create a TUN device it was
  never granted.
- **macOS/Windows runners are not covered.** The daemon supports both
  platforms for local development, but this action has only been built and
  documented against `ubuntu-latest`.
- **Cloud tunnels require `oidc-team`** (and its one-time trust-rule setup on
  portzero.cloud) — see
  [Cloud tunnels via GitHub OIDC](#cloud-tunnels-via-github-oidc). There is
  deliberately no input for a long-lived token: short-lived per-job OIDC
  credentials are the only supported path.

## Status

This action ships from `tunnel-action/` in the `PortZeroNetwork/portzero`
repo (this repo), merged to `staging`. It is usable today via
`uses: PortZeroNetwork/portzero/tunnel-action@staging` (or a release
tag once one exists). Mirroring it into a dedicated
`portzero/tunnel-action` repo for a shorter `uses:` line is a follow-up human
step, not done as part of this change — see `work/task-65.task.md`.
