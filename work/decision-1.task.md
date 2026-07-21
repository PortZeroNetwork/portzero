---
id: de2eadfe-b687-470a-80b5-547e013971df
slug: decision-1
status: done
title: 'ADR: One cross-product convention for staging/promote/release CI'
ticket_type: decision
created_at: 2026-07-14T00:00:00Z
updated_at: 2026-07-14T00:00:00Z
---

## Context

portzero-cloud (deployed service) and portzero-local (distributed CLI) both
use the Heroku-style tag-addressed release model, but with divergent names:
cloud had `deploy-control.yml` / a `production` environment / `ref`+`bump`
inputs, while local buried its promote inside `release.yml` behind a
`channel: release` dispatch gated by a `release` environment, with no `ref`
input and no rollback path. These repos are meant to be the templates copied
into every future product, so the naming must transfer without renaming.

## Options

1. Keep each repo's local vocabulary and document the mapping.
2. Standardize on cloud's vocabulary (Promote to Production, `production`
   environment, `ref`/`bump`/`channel` inputs) in both repos, splitting
   local's promote into its own workflow.
3. Standardize on local's vocabulary (`release` environment, channel-driven
   single workflow) in both repos.

## Decision

Option 2. The gated act is conceptually identical in both archetypes — a
user-visible release guarded by a required reviewer — so it gets one name
everywhere: workflow **Promote to Production** (`promote-production.yml`),
job `promote`, environment `production`, inputs `ref` + `bump`
(patch/minor/major/none). `Release` stays a tag-push-triggered build;
`channel: stable|edge` is the artifact-product extension (edge = ungated
prerelease; stable = re-publish an existing tag, giving artifact products a
real `bump: none` rollback). Canonical names, the trigger-safe tag push
snippet (PAT + strip checkout's `extraheader`), and the new-product setup
checklist live in `docs/release-conventions.md`, kept identical across repos.

## Consequences

- New products copy either repo without inventing names; the conventions doc
  is lift-and-drop.
- The `production` environment (required reviewer) must be created manually
  in this repo's Settings; the old `release` environment is retired.
- `bump: none` re-publish only works for tags cut after `channel: stable`
  exists (a dispatched run uses the workflow file at the dispatched tag);
  older tags are re-published by re-running their original Release run.
- Release history, signing gates (`refs/tags/v*`), and the prerelease channel
  semantics are unchanged.
