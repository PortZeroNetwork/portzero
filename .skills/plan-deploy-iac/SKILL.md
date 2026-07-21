---
name: plan-deploy-iac
description: Plan, implement, or review production and staging infrastructure-as-code for SaaS products. Use when designing Terraform-managed cloud infrastructure, DigitalOcean deployments, GitHub Actions continuous deployment, GitHub environments and branch controls, staging/production parity, deploy-agent pull/push release flows, Let's Encrypt rollout safety, Stripe-backed pricing from plans.json, or operational commands that assess launch/data-risk state before destructive infrastructure or billing changes.
---

# Plan And Deploy IaC

## Environments, branches, and promotion

- There are two major **environments**: **production** (ALWAYS spelled out, NEVER call it "prod") and **staging**. Each has a GitHub Environment, a Terraform workspace, and attributable cloud resources. Production has **no git branch**.
- There must **always** be a long-lived **`staging`** git branch. There must **never** be a `production`, `main`, or `master` branch.
- **Staging** is always **cloud-hosted** for cloud products. Never call a machine on the developer's laptop "staging" — that is the **local dev environment**. See the substrate matrix in `references/infrastructure-policy.md`.
- Push (or merge) to **`staging`** triggers GitHub Actions that run two phases for the staging environment: IaC (e.g. `terraform apply` in the staging workspace) and CD (build/push image, deploy, smoke/E2E).
- **Production** for cloud products deploys only through gated **Trigger Promote to Production** → **Promote to Production** (`workflow_dispatch` + `production` GitHub Environment): deploy the chosen ref (normally the `staging` tip), health-check it, and stamp an immutable `vX.Y.Z` when `bump` is not `none`. Never auto-promote to production from a branch push or PR merge. Downloadable products use **Trigger Stable Release** / **Stable Release** instead — see `continuous-delivery-downloadable`. Align names with the enabled CD module; do not use “Stable Release” wording for cloud Actions jobs.

## Infrastructure defaults

- Prefer Digital Ocean for all cloud infrastructure.
- If a resource can be managed by Terraform, then it should be.
- Keep Terraform state in Digital Ocean Spaces. Use GitHub Environments named **`production`** and **`staging`** to hold secrets that need to be inputs to terraform and are not stored in terraform state. Never name them `prod`.
- Put Terraform plans in pull requests as comments.
- Prefer using an App Platform if possible, such as Digital Ocean App Platform. When the app cannot run on a PaaS, use VMs with a custom deploy agent (see substrate matrix and `references/deploy-agent-cd.md`).
- Staging should use the exact same resource types as production, except when doing so is expensive; this is a lever that the developer will provide feedback on.
- Staging should always use the cheapest scale of each resource; staging should not attempt to perform load testing.
- Make first-time TLS rollout use Let's Encrypt staging ACME in every environment, including production, until routing and certificate automation are proven. Move to Let's Encrypt production only after Let's Encrypt staging has been verified to be fully working, to avoid rate limits while bringing ACME up.
- Prefer using Stripe for billing.
  - Staging must always use Stripe sandbox keys (terraform apply in the staging workspace must fail if a live key is used)
  - Production must always use real Stripe keys (terraform apply in the production workspace must fail if a sandbox key is used).
- Use SendGrid for transactional email.
- Use PostHog for product analytics, feature flags, and experiments. Provision one PostHog project per product, inject its keys through Terraform/environment configuration like other vendor secrets, and send a deploy annotation to PostHog from the CD pipeline so metric changes can be correlated with releases.
- Organize resources so actual cost is always attributable: one DigitalOcean project per product per environment (named `<slug>-production` / `<slug>-staging`), every resource tagged `product:<slug>` and `env:<environment>`, and a `portfolio.toml` at the repo root describing the product's slug, tier, domains, DO projects, and shared-vendor usage. Fleet cost tooling depends on this. See `references/portfolio-economics.md`.
- Distinguish shared portfolio overhead (SendGrid, PostHog, the PortZero edge, wildcard domains — fixed fees with included allowances, paid once for all projects) from per-project cost (this product's DO resources, its domain, its Stripe fees). Never create a duplicate shared-vendor account for one project without explicit approval.
- Because staging sends real emails, never include real customer data in staging. Instead, evaluate data in production for things like NULL, missing values, incorrectly-typed entries, etc. and generate / maintain a seed script that creates representative data.
- Always maintain an email list of users.
- Always use GitHub Actions for CI / CD.

## References

Read these when the task touches their area:

- `references/infrastructure-policy.md` — provider/substrate matrix, staging-vs-production parity, Terraform ownership, TLS rollout, cost-reduction order.
- `references/deploy-agent-cd.md` — deploy-agent capabilities, pull-based routine deploys, push-based agent self-updates, GitHub Actions CD shape (staging branch + Trigger/Promote to Production), failure handling.
- `references/pricing-and-launch-state.md` — plans.json as source of truth, the `just launch-state` command, risk levels, Stripe price migration rules.
- `references/portfolio-economics.md` — project tiers, cost attribution, shared overhead vs per-project cost, DigitalOcean cost estimation in CI.
