# Infrastructure Policy

## Provider And Substrate Matrix

Default cloud provider: **DigitalOcean**.

Pick the **runtime substrate** with this ordered matrix. Use the first row that
fits; do not invent a hybrid unless a named requirement forces it.

| Substrate | When to use | Typical shape |
|-----------|-------------|----------------|
| **DigitalOcean App Platform** | Default for cloud SaaS that fits a PaaS (HTTP services, managed build/deploy, no host-level needs). | Terraform-managed App Platform app per environment; no deploy agent. |
| **Droplets + deploy agent + Docker Compose** | Named PaaS gap: raw TCP/UDP, custom Caddy/wildcard TLS, Docker socket, host networking, privileged containers, unsupported sidecars, or another gap confirmed in current DO docs. | Smallest VMs that work; pull-based deploy agent for routine deploys; agent self-update is a separate push (see `deploy-agent-cd.md`). |
| **Managed data services** | Prefer managed Postgres (and managed Redis when persistence/ops justify it) for **both** staging and production when affordable; container Redis only when the trade-off is explicit. | Same engine in staging and production. |
| **Kubernetes** | Only when orchestration complexity is justified by scale or operational requirements — not the default. | — |
| **Local dev environment** | Developer laptop / workstation only. | Docker Compose or language tooling on the machine. **Never** call this staging. No production traffic, no staging domain, no "local staging." |
| **Validation static hosting** | Unvalidated ideas (`$validate-idea`). | Cheapest static host (e.g. App Platform static site); no full staging stack. |

**Cloud staging is mandatory for full-tier cloud products.** Staging always runs
in the cloud on the same *class* of substrate as production (App Platform with
App Platform, droplets+agent with droplets+agent), at the cheapest practical
scale. PortZero tunnels, home labs, and developer machines are **not** staging.

Compose on a droplet is an implementation detail of the VM substrate — it does
not make a laptop "staging." App Platform apps do not need a compose stack for
parity; keep deploy path, TLS mechanism, database engine, migrations, and
config schema aligned instead.

## Staging And Production

Staging should be as identical to production as possible at the cheapest practical scale. Accept differences in size, count, backup retention, and test-mode third-party accounts. Avoid differences in network topology, deploy path, TLS mechanism, database engine, DNS shape, migrations, or runtime roles unless the cost or vendor constraint is explicit.

Use separate domains for production and staging. Keep DNS records, OAuth callbacks, cookies, webhook endpoints, and TLS issuance orthogonal.

Use sandbox Stripe in staging. For other external effects:

- Prefer real integrations in restricted/test mode.
- Never import real customer email addresses into staging by default.
- Gate outbound email/SMS/webhooks so staging cannot contact real customers accidentally.
- Use clearly separate sender identities, API keys, webhook secrets, and OAuth apps where feasible.

## Terraform Ownership

Manage every reasonable resource in Terraform:

- DigitalOcean projects, VPCs, App Platform apps, Droplets, managed databases, firewalls, reserved IPs, Spaces buckets after bootstrap, and registry resources if supported.
- Cloudflare or other DNS records, including staging.
- GitHub environments named **`production`** and **`staging`** (never `prod`), branch protections/rulesets for **`staging`**, environment secrets/vars, deploy keys, and Actions variables when provider support exists.
- Stripe products, prices, tax codes, billing portal settings, and webhook endpoints when represented by provider support and product config.

One-time manual bootstrap is acceptable for state storage and initial credentials. Document the command and keep it out of normal operations.

Use remote Terraform state with locking. Treat state as containing secrets. Plans should be produced in pull requests for review before merge. Applies should run through GitHub Actions for **`staging`** branch updates and for production only via the gated **Trigger Promote to Production** / **Promote to Production** path (or an equivalent production environment-gated job). Local apply is an explicit repo exception only; if so, explain it and keep PR plans.

## TLS Rollout

Use Let's Encrypt staging ACME for first infrastructure bring-up in every environment, including production. Switch to Let's Encrypt production only after DNS, routing, wildcard handling, and certificate automation are proven. This avoids production rate limits while infrastructure is still being debugged.

## Cost Decision Rules

When staging cost is high, reduce cost in this order:

1. Smaller instance/database sizes.
2. Fewer replicas or workers.
3. Shorter retention and backup windows.
4. Lower traffic or synthetic load.
5. Sandbox/test-mode third-party accounts.
6. Architectural differences only when the savings are material and the reduced coverage is explicit.

Never "save money" by moving staging onto a developer machine.
