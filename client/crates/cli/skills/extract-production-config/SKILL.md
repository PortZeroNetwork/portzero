---
name: portzero-extract-production-config
description: >-
  Extract everything needed to configure production hosting for this project on
  ANY platform-as-a-service, using Port Zero's runtime truth (discovered
  services, tunnel domains, health paths, observed dependency edges, exercised
  routes) plus the project's compose files. Use when the user asks to "graduate"
  a Port Zero dev setup to production, deploy to a PaaS, or produce a production
  config from what is running locally. This skill only EXTRACTS and INTERPRETS —
  it contains no platform-specific templates. Look up the target PaaS's own docs
  for the exact config format.
---

# Extract a production config from a Port Zero dev setup

You are helping graduate a project from local development (where Port Zero
tunnels expose services by name) to production hosting on a platform-as-a-service
(PaaS). Your job is to **extract and interpret** the facts that any PaaS needs.
You already know — or can look up — the specific config format for the PaaS the
user names. **This skill deliberately contains no per-platform templates or
emitters.** Do not invent one.

## Golden rules

1. **Port Zero knows the runtime truth; the compose file knows the wiring; the
   user knows the production facts.** Combine all three. Never fabricate values.
2. **`PZ_*` env vars express ingress for local dev and disappear in production.**
   They are not production config. Translate what they *mean*, then drop them.
3. **Do not invent a Port Zero-side registry for production facts.** Secrets,
   scaling, regions, custom domains, and environment variables live in the
   user's own systems (e.g. GitHub Environments, the PaaS dashboard). Ask the
   user or read their existing config; never store these in Port Zero.
4. **Emit config only in the target PaaS's own format**, which you look up
   yourself. This skill stops at the extracted facts.

## Step 1 — Read the runtime truth from the daemon

Prefer the Port Zero MCP server (`portzero mcp`) if it is connected; otherwise
run `portzero inspect` and read its text output. From the MCP server call:

- `list_services` — every discovered process/container with `PZ_TUNNEL`, its
  forwarded port, source (process pid + cwd, or container id/name), and health
  path.
- `list_tunnels` — the tunnel domains (local + cloud), URLs, and health paths.
- `observed_edges` — who-talks-to-whom between tunnels (service dependencies).
- `exercised_routes` — the HTTP routes actually hit per tunnel (a smoke-test
  inventory).

## Step 2 — Interpret the runtime truth

Map the observed facts to production concepts:

| Observed fact | Production meaning |
|---------------|--------------------|
| A container/process **had a tunnel** | It is a **public HTTP surface**. Its tunnel port is the port to route external ingress to. |
| A container **without a tunnel** | It is **internal** (e.g. a database, a queue). It gets a private address, not public ingress. |
| A **health path** (`PZ_HEALTH_PATH`) | The production **health/readiness check** path. This value survives the move — reuse it verbatim. |
| An **observed edge** `A → B` | `A` depends on `B`. `B` must be reachable from `A` in production (private networking / service discovery). |
| **Exercised routes** | A ready-made **smoke-test list** for the deployed service. |

**Observability caveat (important):** Port Zero only observes traffic addressed
via tunnel names. Container-to-container traffic over compose-internal DNS (e.g.
one service reaching another as `http://db:5432` on the shared compose network)
never passes through the daemon and will **not** appear in `observed_edges`.
Treat the edge list as a *lower bound* on dependencies. Recover the internal
wiring from the compose file (Step 3) and confirm with the user.

## Step 3 — Interpret the compose / process config

Read `docker-compose.yml` (or the equivalent process launch config):

- Each `service` is a deployable unit. Note its image/build, the container port
  it listens on (`target` in the ports mapping, or the app's bind port), and its
  non-`PZ_*` environment variables (those ARE production config — carry them
  over, but treat secrets as references, not literals).
- **Strip every `PZ_*` variable** (`PZ_TUNNEL`, `PZ_TUNNEL_HTTP_PORT`,
  `PZ_TUNNEL_PORTS`, `PZ_HEALTH_PATH`, `PZ_TUNNEL_NO_PROBE`). They are ingress
  hints for local dev only. You have already captured what they mean in Step 2
  (public vs internal, the port, the health path).
- Services linked only by compose-internal DNS (e.g. `depends_on`, or a
  hardcoded `http://db:5432`) are internal dependencies the daemon could not
  observe — include them.

## Step 4 — Gather the production facts you cannot infer

These are the user's business. Ask for them or read their existing setup; do not
guess and do not store them in Port Zero:

- Secrets and credentials (database URLs, API keys) — reference them by name.
- Scaling / instance sizing, regions, replica counts.
- Custom domains and TLS.
- Any environment-specific values (staging vs prod) — e.g. GitHub Environments.

## Step 5 — Produce the config in the target platform's format

Now, and only now, look up the target PaaS's documentation and emit its config
in its own format. Populate it from the extracted facts:

- public services → the PaaS's web/service definitions, routing external ingress
  to the captured port;
- internal services → the PaaS's private services / add-ons;
- health path → the PaaS's health-check field;
- dependencies (edges + compose links) → the PaaS's service-to-service
  networking;
- non-`PZ_*` env → environment config, with secrets as references;
- exercised routes → a post-deploy smoke-test step.

Present the extracted facts to the user first (a short table of services, which
are public vs internal, ports, health paths, and dependencies) so they can
confirm before you write platform config.
