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
        uses: PortZeroNetwork/portzero-local/tunnel-action@staging
        with:
          tunnels: ci-${{ github.run_id }}--myapp.portzero.local

      - name: Run integration tests against the real HTTPS URL
        run: npm test
        env:
          BASE_URL: ${{ steps.tunnel.outputs.url }}

      - name: Teardown
        if: always()
        uses: PortZeroNetwork/portzero-local/tunnel-action@staging
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
  uses: PortZeroNetwork/portzero-local/tunnel-action@staging
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

## Outputs

| Output | Description |
|--------|-------------|
| `urls` | Newline-separated resolved URL per entry in `tunnels`, same order (from `portzero url`). |
| `url`  | Convenience: set only when `tunnels` had exactly one entry. |

## Teardown: why it's a second, explicit step

Composite actions (`runs.using: composite`) have no `post:` hook — only
JavaScript and Docker actions support one. So `mode: teardown` is a step you
add yourself, guarded by `if: always()`, rather than something this action
runs automatically when the job ends. On GitHub-hosted runners the VM is
destroyed after the job anyway, so teardown here is about a clean log trail on
failure (and not leaking a daemon across jobs if you run this on a
self-hosted, reused runner).

## Limitations (v1)

- **Linux hosted runners only** (e.g. `ubuntu-latest`). Local tunnels need a
  real TUN device and `CAP_NET_ADMIN`/`CAP_NET_BIND_SERVICE`
  (or root) — exactly what a normal Linux VM runner provides via
  passwordless `sudo`, and exactly what a container-based job cannot: see the
  next bullet.
- **Container-based jobs are not supported.** `jobs.<id>.container: ...` runs
  your steps inside a Docker container on the runner, which does not have
  permission to create a TUN device on the *host* kernel. Run this action
  directly on the VM (the default, no `container:` key) rather than inside a
  job container. This action detects the container case and prints a warning,
  but cannot work around it.
- **macOS/Windows runners are not covered by v1.** The daemon supports both
  platforms for local development, but this action has only been built and
  documented against `ubuntu-latest`.
- **Cloud tunnels are out of scope for this action (for now).** A
  `*.tunnel.portzero.cloud` domain works the same way from a developer's
  laptop, but wiring it into CI without a long-lived secret needs an OIDC
  credential exchange. The cloud-side half of that (GitHub Actions OIDC token
  exchange for short-lived scoped tunnel credentials) has since shipped on
  `portzero-cloud`; this action's client-side integration with it has not.
  This action does not implement it yet; once wired up, `tunnels:` entries
  with a `.tunnel.portzero.cloud` suffix will be able to use it without any
  change to this action's interface.

## Status

This action ships from `tunnel-action/` in the `PortZeroNetwork/portzero-local`
repo (this repo), merged to `staging`. It is usable today via
`uses: PortZeroNetwork/portzero-local/tunnel-action@staging` (or a release
tag once one exists). Mirroring it into a dedicated
`portzero/tunnel-action` repo for a shorter `uses:` line is a follow-up human
step, not done as part of this change — see `work/task-65.task.md`.
