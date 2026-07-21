# Portfolio Economics

The portfolio goal: many products live simultaneously, each mostly idle, with
recurring cost near zero until a product earns dedicated spend. Infrastructure
decisions optimize for the fleet, not the single project.

## Project Tiers

- **Validation tier** — static landing page, PostHog, SendGrid list, nothing
  else. No Terraform stack of its own, no staging environment, no database, no
  Stripe. See `$validate-idea`.
- **Full tier** — the complete `plan-deploy-iac` shape: cloud **staging** from
  day one (long-lived `staging` branch + Deploy Staging), production only via
  gated Trigger Promote to Production (no production branch). Entered deliberately via
  `$scaffold-new-project` after validation or on explicit request.

Graduation and demotion are recorded in `portfolio.toml` (`tier`, `status`).

## Overhead Vs Per-Project Cost

Shared portfolio overhead: fixed vendor fees paid once for all projects, each
with an included allowance — e.g., SendGrid ($/month including N emails),
PostHog (included events), shared edge/DNS products the fleet already pays for,
a wildcard/portfolio domain, the Spaces bucket holding Terraform state and
archives.

Per-project cost: the product's own DO resources (including **cloud staging**),
its dedicated domain, Stripe transaction fees, and any vendor allowance it
consumes *beyond* the shared free allowance.

Rules:

- A project's marginal cost excludes shared fixed fees; those are recovered at
  the portfolio level (see the fleet tool's overhead-recovery model).
- When a project's usage approaches a shared allowance ceiling (emails, events,
  bandwidth), that is a pricing/graduation signal, not a reason to silently
  upgrade the shared plan. Surface it.
- Never create a second account for a shared vendor to dodge attribution;
  attribute usage instead (per-project SendGrid categories/subusers, PostHog
  projects, DO tags).
- Keep staging cheap with size and retention (see cost-reduction order in
  `infrastructure-policy.md`). Do not eliminate cloud staging or redefine a
  local dev environment as staging.

## Cost Attribution Layout

So actual spend is always computable per product:

- One DigitalOcean project per product per environment: `<slug>-production`,
  `<slug>-staging`. Shared substrate lives in a `portfolio-shared` DO project.
- Tag every taggable resource `product:<slug>` and `env:<environment>`.
- Resource names prefixed `<slug>-<environment>-`.
- Terraform state keys namespaced by slug and workspace.
- `portfolio.toml` at each repo root declares slug, tier, status, domains, DO
  project names, Stripe mode, and which shared vendors the product uses.

## DigitalOcean Cost Estimation In CI

There is no InfraCost equivalent for DO, so the portfolio fleet tool provides
`do-cost`: it reads `terraform plan`/state JSON, maps DO resources to prices
(droplet and App Platform prices come from the DO API — `/v2/sizes`,
`/v2/apps/tiers/instance_sizes` — plus a curated table for the rest), and
prints estimated monthly cost.

- Run it in the same PR job that posts the Terraform plan comment; include the
  monthly delta ("+$12/mo") in the comment.
- Fail the job when a plan pushes the product's environment past its budget in
  `portfolio.toml` (`budget_monthly_usd`), requiring an explicit budget bump in
  the same PR.
