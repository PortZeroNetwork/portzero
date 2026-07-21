---
name: scaffold-new-project
description: Bootstrap a new software project with the portfolio's standard conventions baked in. Use when the agent is asked to start a new project, create a new SaaS/product repo, set up a validation-tier landing repo, graduate a validated idea into a full build, or make an existing repo conform to portfolio conventions (justfile, portfolio.toml, guarantees, Terraform layout, staging branch without production branch, PostHog, Stripe, SendGrid).
---

# Scaffold New Project

## Goal

Every project in the portfolio has the same skeleton, so context-switching
between many small products is cheap, fleet tooling can discover and cost every
project automatically, and later skills (`$implement-billing-chassis`,
`$plan-deploy-iac`, `$sunset-project`) find the shape they expect.

## Pick The Tier First

- **Validation tier** — for unvalidated ideas (see `$validate-idea`): static
  landing page, PostHog project, SendGrid list, `portfolio.toml`. No backend,
  no database, no staging, no Stripe. Scaffold only steps 1–4 below.
- **Full tier** — for validated ideas or explicitly commissioned products:
  everything below.

## Workflow

1. Settle the language and stack once:
   - Ask the user which primary language to use if it is not obvious, then
     record it in the project-level `CLAUDE.md`/agent instructions. Standalone
     scripts are written in this language from then on (see the `just`
     instruction module).
   - Record other standing choices there too: framework, database, auth
     approach.
2. Create the repo skeleton:
   - Enable the `just` instruction module (`agent-toolbox enable just`) and put
     a `justfile` at the repo root as the only task runner entry point. Every
     recurring operation added later (dev, test, deploy, launch-state, seed)
     becomes a recipe. Scripts invoked by recipes use the primary backend
     language per that module.
   - `README.md` written with `$audience-first-docs` for the product's target
     user, not for the developer.
   - `docs/guarantees/` and `docs/specs/freeform/` seeded per
     `$guarantee-maintainer` (sibling trees under `docs/`, not audience folders;
     see `$documentation-layout` / the documentation-layout instruction).
3. Create `portfolio.toml` at the repo root. This is the machine-readable
   registry entry the fleet board discovers and costs projects by:

   ```toml
   [project]
   slug = "myapp"                # unique across the portfolio
   name = "My App"
   status = "validating"         # idea | validating | building | launched | sunsetting | archived
   tier = "validation"           # validation | full
   created = "2026-07-07"
   decision_deadline = ""        # validate-idea decision date, if validating
   budget_monthly_usd = 0        # per-environment cost ceiling enforced in CI

   [domains]
   production = "myapp.example"
   staging = "staging.myapp.example"

   [digitalocean]
   project_production = "myapp-production"
   project_staging = "myapp-staging"
   tag = "product:myapp"

   [stripe]
   mode = "none"                 # none | sandbox | live

   [vendors]                     # which shared portfolio vendors this project consumes
   sendgrid = true
   posthog = true
   ```

4. Wire shared portfolio vendors (these are portfolio overhead, not new
   per-project accounts — reuse the existing accounts):
   - PostHog: one project per product; env-injected keys; capture from the
     first page.
   - SendGrid: one list/segment named by slug; confirmed opt-in from day one
     (every project maintains an email list of users).
   - Domain/DNS records managed in Terraform once infra exists; validation-tier
     pages prefer a subdomain of an existing portfolio domain.

   Validation tier stops here.

5. Set up infrastructure and delivery per `$plan-deploy-iac` and the enabled
   continuous-delivery instruction modules:
   - Long-lived **`staging`** git branch only — never `production`, `main`, or
     `master`. Production is promote-addressed via **Trigger Promote to
     Production** / **Promote to Production** (cloud) or **Trigger Stable
     Release** / **Stable Release** (downloadable).
   - GitHub Environments and Terraform workspaces named **`staging`** and
     **`production`** (never `prod`); state in Spaces; plans as PR comments.
   - Cloud staging from day one for cloud products (cheapest practical scale,
     same substrate class as production). Local machines are **local dev**, not
     staging.
   - One DigitalOcean project per environment, named `<slug>-production` /
     `<slug>-staging`, with every resource tagged `product:<slug>` and
     `env:<environment>` so actual cost is always attributable.
   - Choose runtime substrate via the matrix in `$plan-deploy-iac`
     `references/infrastructure-policy.md` (App Platform default; droplets +
     deploy agent only for named PaaS gaps).
   - GitHub Actions CI/CD from the start: Deploy Staging on `staging` push;
     production only through gated promote (cloud) or stable-release
     (downloadable) workflows — never branch-push to production.
6. Set up billing per `$implement-billing-chassis` when the product will
   charge: `plans.json`, sandbox Stripe in staging with key-mode validation,
   entitlement module, webhook endpoint, PostHog revenue events.
7. Add the operational floor before first production release:
   - `just launch-state` recipe (per `$plan-deploy-iac` references).
   - The experiment framework per `$implement-growth-experiments`: an (initially
     empty) `experiments.toml` registry and the PostHog event conventions, so
     pricing/ad experiments are runnable from day one and readable on the fleet
     board.
   - Seed script generating representative fake data for staging (staging never
     contains real customer data).
   - Uptime check once production is live; deploy annotations into PostHog.
8. Finish by verifying the skeleton: `just --list` shows the recipes, CI is
   green on the initial commit, `portfolio.toml` parses, and the README passes
   a `$prelaunch-user-audit`-style skim for the target user.

## Guardrails

- Do not scaffold full-tier infrastructure for an unvalidated idea; that is the
  main source of idle portfolio cost. Graduation is a deliberate step.
- Do not create new vendor accounts when a shared portfolio account exists;
  new fixed fees need explicit user approval.
- Conventions here are defaults, not law: when the user overrides one, record
  the override in the project's `CLAUDE.md` so future sessions stop re-asking.
