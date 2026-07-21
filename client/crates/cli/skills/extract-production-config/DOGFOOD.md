# Dogfood run — extract-production-config skill

This is a real run of the `portzero-extract-production-config` skill against one
of our own example apps: `portzero-examples/nodejs-typescript/docker`. It records
the extraction and the production config it produced, as evidence that the skill
is usable end-to-end.

## The app (inputs)

`docker-compose.yml`:

```yaml
services:
  app:
    build: .
    environment:
      PZ_TUNNEL: ${PZ_TUNNEL:-nodejs-typescript-docker.portzero.local:80}
    ports:
      - target: 8080
        published: "0"
        host_ip: 127.0.0.1
        protocol: tcp
```

`app.ts` binds `0.0.0.0:8080` and serves plain HTTP. `Dockerfile` runs
`node app.ts`, `EXPOSE 8080`.

## Step 1–2 — Runtime truth (from `portzero inspect` / MCP)

With the stack running, `list_services` / `list_tunnels` report one service:

| Service | Tunnel? | Port | Health path | Source |
|---------|---------|------|-------------|--------|
| `app`   | yes → `nodejs-typescript-docker.portzero.local` | 8080 (`PZ_TUNNEL` canonical `:80` → VIP 80, container 8080) | none (`PZ_HEALTH_PATH` not set) | container `nodejs-typescript-docker-app-1` |

- `observed_edges`: none — this is a single service with no tunnel-to-tunnel
  calls.
- `exercised_routes`: whatever your tests hit (e.g. `GET /`). Empty until traffic
  flows.

Interpretation:

- The `app` container **had a tunnel** → it is a **public HTTP surface**; route
  external ingress to its container port **8080**.
- There are **no untunnelled containers** → no internal-only services (no DB).
- **No health path** was declared → recommend the user add one
  (`PZ_HEALTH_PATH=/`) or accept the platform default.

## Step 3 — Compose interpretation

- One deployable unit: `app` (build from Dockerfile, listens on 8080).
- **Strip `PZ_TUNNEL`** — it is a local-dev ingress hint. Its meaning ("public,
  container port 8080") is already captured above.
- No `depends_on` / internal DNS links → no hidden internal dependencies.
- No non-`PZ_*` env vars to carry over.

## Step 4 — Production facts (user's business)

Nothing secret here (the example is stateless with no DB/API keys). For a real
app these would come from the user's GitHub Environments / PaaS dashboard, not
from Port Zero.

## Step 5 — Produced config (concrete target: Render)

The skill itself emits nothing platform-specific; this is the **result** of
applying it, choosing Render as the target and reading Render's own docs for the
format. A different PaaS would use the same extracted facts in its own format.

```yaml
# render.yaml — produced from the extracted facts above.
services:
  - type: web                 # "public HTTP surface" (it had a tunnel)
    name: nodejs-typescript-docker
    runtime: docker
    dockerfilePath: ./Dockerfile
    plan: starter
    envVars:
      - key: PORT             # app listens on 8080; Render routes ingress here
        value: "8080"
    healthCheckPath: /        # recommend PZ_HEALTH_PATH=/ so this is explicit
    # No private services: the extraction found no untunnelled containers and no
    # observed/compose dependencies.
```

Post-deploy smoke test = the exercised-routes inventory (e.g. `GET /`).

## Notes

- Observability caveat held: with a single service there are no tunnel-to-tunnel
  edges to miss. For multi-service apps, remember `observed_edges` is a lower
  bound — recover compose-internal links from the compose file.
- The skill produced **no** portzero-side registry of production facts; secrets
  and scaling stay in the user's own systems.
