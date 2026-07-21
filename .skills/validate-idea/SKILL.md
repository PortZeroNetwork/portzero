---
name: validate-idea
description: Validate demand for a product idea before writing product code, using the cheapest credible test. Use when the agent is asked to validate an idea, set up a smoke test, build a pre-launch landing page, run a fake-door or waitlist test, gauge willingness to pay before building, or decide whether an idea should graduate to a real build or be parked. Produces a validation plan, a minimal instrumented landing surface, and an explicit graduate/park decision rule.
---

# Validate Idea

## Goal

Answer "should this be built at all?" for the smallest possible cost in money and
time. The portfolio goal is many ideas live at once with near-zero idle cost, so
validation must not create infrastructure, vendor accounts, or maintenance burden
that outlives a failed test.

## Core Workflow

1. Write the demand hypothesis before building anything:
   - Narrow target user (use the audience-definition method from `$audience-first-docs`).
   - The pain, the promised outcome, and the price point being tested.
   - The acquisition channel the test will actually use (existing email list,
     a community, search, paid, personal network). A landing page with no
     traffic plan is not a test.

2. Pick the cheapest credible validation surface, in order of preference:
   - An offer email to an existing portfolio email list segment.
   - A static landing page with email capture (waitlist).
   - A fake-door pricing page: real plan grid and a "Start" button that leads to
     a waitlist form instead of checkout.
   - A concierge/manual version of the service sold to 1–3 customers by hand.

3. Keep validation-tier infrastructure near zero:
   - Static site only. No backend, no database, no staging environment.
   - Host on the cheapest static option (DigitalOcean App Platform static site
     or equivalent). Do not create a dedicated Terraform stack for a validation
     page unless one already exists for the portfolio's shared static hosting.
   - Prefer a subdomain of an existing portfolio domain first; buy a dedicated
     domain only when brand credibility is part of what is being tested, or
     after the idea validates.
   - Register the idea in `portfolio.toml` with `status = "validating"` and
     `tier = "validation"` so the fleet board tracks it (see `$scaffold-new-project`).

4. Instrument with PostHog from the first visitor:
   - One PostHog project per product idea.
   - Capture page view, CTA click, email submitted, and (for fake-door tests)
     plan selected with the plan name and displayed price.
   - If testing price points or headlines, use a PostHog experiment/feature flag
     to split variants rather than deploying separate pages.

5. Route captured emails into the portfolio SendGrid account:
   - One list or segment per idea, clearly named with the idea slug.
   - Use confirmed opt-in. These addresses are the launch asset if the idea
     graduates and must carry clean consent records.

6. Set the decision rule up front, then time-box:
   - Example: "200 unique visitors from the named channel within 4 weeks; graduate
     at ≥5% email conversion or ≥3 fake-door plan selections at the target price;
     otherwise park."
   - Calibrate thresholds to the price point: a $99/month B2B idea needs far fewer,
     stronger signals (replies, calls, prepayments) than a $9/month prosumer idea
     needs visitors.
   - Record the rule and the deadline in the repo (or in `portfolio.toml` notes)
     so the decision is made against pre-committed criteria, not vibes.

7. Decide and act at the deadline:
   - Graduate: run `$scaffold-new-project` for the full tier, email the waitlist,
     and carry the validated price into `$recommend-saas-pricing-strategy`.
   - Park: run `$sunset-project` at the parked tier — leave a static "not available
     yet" page or remove it, keep the list and PostHog data, stop all recurring
     spend, set `status = "archived"` or `"idea"` in `portfolio.toml`.
   - An ambiguous result with a cheap, specific next test may buy one extension.
     Never extend twice without new information.

## Guardrails

- Fake-door tests must not take payment for a product that does not exist. If a
  card is charged (preorder/concierge), the obligation is real: deliver or refund.
- Say "join the waitlist" honestly at the moment of the fake door; do not fake a
  checkout failure.
- Do not import purchased or scraped email addresses. Consent gathered here is
  reused at launch, so keep it clean (confirmed opt-in, clear sender identity).
- Do not let validation infrastructure grow. If the test seems to need a backend,
  the test is wrong — simplify the offer or use concierge delivery instead.
