---
id: 0b3d581b-4412-4327-8c5d-781f5c2dca12
slug: doc-2
status: done
title: Document the review-apps pattern (orchestrator-neutral)
relations:
  contains:
  - d05fe04d-bdd7-4eae-9b55-f487c713ad58
ticket_type: doc
created_at: 2026-07-07T22:55:31.771396Z
updated_at: 2026-07-07T23:07:46.069229Z
preferred-model: sonnet
---

Documentation/blog post: per-PR preview environments using only tunnel naming —
no portzero-provided orchestrator, users bring their own (compose, scripts,
anything). Pattern: each PR's deployment sets `PZ_TUNNEL={branch}--myapp...`
(or a nested name under a wildcard custom domain); random published ports plus
discovery mean coexisting PRs on one host with zero port juggling; the URL is
knowable before deploy so a bot can comment it on the PR; teardown is the
user's concern.

Explicitly out of scope for the product: any deploy agent. Document that
boundary.

## Acceptance Criteria

- [x] Doc in portzero-local/docs (linked from Docs tab) and/or a blog post on portzero.net
- [x] End-to-end example with plain docker compose + a GitHub Actions workflow

## Notes (implementation)

- Doc: `docs/review-apps.md`, linked from `docs/README.md` under a new "Patterns" section.
- End-to-end example (docker compose + GitHub Actions workflow) is embedded in the doc,
  using a persistent self-hosted runner (review apps must outlive the CI job that deploys
  them — contrast with the ephemeral-job `tunnel-action` from task-65).
- The doc explicitly documents that a deploy agent is out of scope for the product (own
  "What's explicitly out of scope" section).
- A blog post on portzero.net was not produced — outside this repo's scope; the docs-tab
  link satisfies the acceptance criterion's "and/or".
- `{pr}` / `{run-id}` template tokens (task-64, owned by the core agent) are referenced as
  forthcoming shorthand; the example itself uses the manual GitHub Actions expression form
  so it works regardless of that ticket's landing order.
