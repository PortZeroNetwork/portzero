# Pricing And Launch State

## plans.json As Source Of Truth

Use a structured plan file, commonly `plans.json`, as the source for:

- Public plan names and descriptions.
- Stripe product identifiers and metadata.
- Recurring and one-time prices.
- Trial rules, annual discounts, coupons, and promotions if the product supports them.
- Feature entitlements, seats, usage limits, overage policy, and billing intervals.
- Migration/grandfathering metadata when plans evolve.

Keep the schema product-generic. Do not hard-code a specific product's limits such as bandwidth, projects, seats, or storage into the skill. Let the target repo define product-specific entitlement dimensions.

Terraform should read the plan file and create or update Stripe catalog objects. Stripe prices are effectively append-only after use, so changes to amount, currency, interval, or tax behavior should normally create a new price and keep old prices available for existing customers.

## Launch-State Command

Add a `just` command that answers "how launched are we?" before destructive infrastructure, data, or billing changes.

Recommended command shape:

```just
launch-state ENV="production":
    # ENV is staging | production — never "prod"
    # read plans.json
    # query app database for real accounts and protected data
    # query Stripe for subscriptions, payments, customers, and price usage
    # print risk level and per-price usage
```

The command should report:

- Whether real user accounts exist.
- Whether real customer data exists or may exist.
- Whether Stripe has live customers, subscriptions, invoices, charges, payment intents, checkout sessions, or purchases.
- For each price in the current plan file: Stripe price id, product id, live/test mode, active flag, whether it has ever been used, current subscription count, historical purchase count, and recommended action.
- Whether each planned change is safe to apply, needs a new price, needs grandfathering, or needs manual review.

Use sandbox Stripe for staging and live Stripe for production. Make the mode obvious in output.

## Risk Levels

Use a simple risk classification:

- `unlaunched`: no real users and no live purchases. Destructive changes may still need care, but no customer migration is implied.
- `soft-launched`: real accounts or test customers exist, but no live paid purchases. Protect user data; billing catalog changes are less constrained.
- `launched`: live purchases, active subscriptions, or real customer commitments exist. Avoid destructive changes, preserve used Stripe prices, and plan grandfathering.

## Migration Rules

- Never delete a used price as the primary migration path.
- Prefer creating a new price and leaving existing subscriptions on old prices unless the user requests an explicit migration.
- If entitlements change for existing customers, document grandfathering behavior and enforcement code paths.
- Keep public pricing, checkout creation, Stripe Terraform, and entitlement enforcement in sync from the same plan data.
