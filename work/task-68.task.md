---
id: d7be2708-4cc3-4963-aac6-3afdff61dc08
slug: task-68
status: done
title: PaaS-agnostic extraction skill installed with portzero
relations:
  contains:
  - d05fe04d-bdd7-4eae-9b55-f487c713ad58
depends_on:
- bea44529-7acf-4375-87ae-b36173cc37ab
created_at: 2026-07-07T22:55:29.790464Z
updated_at: 2026-07-07T23:07:43.011932Z
preferred-model: opus
---

On install (or via a subcommand), portzero offers to install AI coding agent
skills into the user's project. The core skill teaches an agent how to extract
everything needed to configure production hosting on ANY platform-as-a-service
— it is explicitly NOT per-platform (the agent already knows/looks up specific
PaaS docs; the skill only covers what portzero knows):

- Read the daemon's MCP tools (runtime truth): which names had tunnels = the
  public HTTP surfaces and their ports; containers without tunnels = internal;
  observed edges = service dependencies; health paths; exercised routes = smoke-test list
- Interpret PZ_* env vars in compose files (they express ingress and disappear in production)
- Where production facts live is the user's business (e.g. GitHub environments)
  — the skill must not invent a portzero-side registry for them

## Acceptance Criteria

- [x] Skill installable via portzero; agent-tool-agnostic layout where feasible
      (`portzero skill install` writes an embedded SKILL.md; `--print` emits it to
      stdout for any agent tool, `--dir` chooses the location)
- [x] Contains extraction + interpretation guidance only; zero platform-specific emitters
      (guarded by a unit test that fails if a platform emitter name appears)
- [x] Dogfooded once: used to produce a real production config for one of our own apps
      (see DOGFOOD.md — applied to portzero-examples/nodejs-typescript/docker,
      produced a real render.yaml from the extracted facts)
