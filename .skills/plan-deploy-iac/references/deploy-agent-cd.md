# Deploy Agent And Continuous Deployment

## Deploy Agent Pattern

Use a deploy agent when the app needs a host-controlled deploy path (see the
substrate matrix in `infrastructure-policy.md`). The routine deploy should be
pull-based:

1. CI builds and pushes an immutable image tag.
2. CI updates Terraform and environment configuration.
3. CI calls the target environment's deploy agent.
4. The agent pulls the image and runs the host-local deploy command.

The agent must not redeploy itself through that path. Pin the agent image/version separately from the application image/version. Treat the agent token and host Docker socket as root-equivalent.

Required agent capabilities usually include:

- Health endpoint.
- Authenticated config/env update.
- Authenticated source or manifest update when the host needs repo files.
- Authenticated update endpoint that returns a job id immediately.
- Job polling with logs and terminal status.
- Explicit service selection where the agent is never a valid routine target.

## Agent Self-Updates

Agent updates should be push-based and manual. Add a `just` recipe that reaches each host through the approved admin path and updates only the agent after human intent is clear.

Example recipe shape:

```just
deploy-agent-push ENV VERSION:
    # validate ENV and VERSION (ENV is staging | production — never "prod")
    # connect to each host in ENV
    # set DEPLOY_AGENT_IMAGE_TAG={{VERSION}}
    # docker compose pull deploy-agent
    # docker compose up -d deploy-agent
    # verify /health and /version
```

CI should fail when agent deployment code changes and any target agent is not already running the expected version. The failure is resolved manually by running the push-update recipe, not by weakening the drift check.

Useful checks:

- Compare repo expected agent version, image digest, or protocol version with each agent's `/version`.
- Query all hosts in the environment.
- Fail closed if an agent cannot be reached, unless the task is explicitly a recovery operation.

## GitHub Actions CD

Match the continuous-delivery instruction modules. Canonical shape for cloud
products that use a deploy agent:

| Workflow `name:` | Trigger | What it does |
|------------------|---------|--------------|
| **Deploy Staging** | Push to the long-lived `staging` branch (and PRs into `staging` only as the instruction modules allow) | Plan/apply **staging** Terraform, build and push image, call the **staging** deploy agent, run staging smoke/E2E. |
| **Trigger Promote to Production** | `workflow_dispatch` only, behind the **`production`** GitHub Environment | Human-approved gate: choose ref (default: `staging` tip) and bump policy, then start **Promote to Production**. |
| **Promote to Production** | Started by the trigger (production-gated path) | Deploy the chosen ref to **production** infra via the production deploy agent (or App Platform apply), health-check, then stamp immutable `vX.Y.Z` when `bump` is not `none`. |

Rules:

- There is **no** `production`, `main`, or `master` git branch and **no** workflow that deploys production on branch push.
- GitHub Environments are named **`staging`** and **`production`** (never `prod`).
- Branch protection and Actions jobs for the long-lived line of development target **`staging`** (and PRs into it). Production protection is the **`production`** environment gate on **Trigger Promote to Production**.
- Put Terraform plans in PRs so infrastructure changes are visible before they land on `staging`.
- Use concurrency groups per environment. Use immutable image tags, normally commit SHAs. Avoid `latest` for deploy identity.
- **Roll back** production by re-running **Trigger Promote to Production** / **Promote to Production** with `ref` set to an older tag/SHA and `bump: none`.
- Use **promote** workflow names for cloud; reserve **Trigger Stable Release** / **Stable Release** for downloadable products only.

For downloadable products, pair this with `continuous-delivery-downloadable` (Unstable Release on every `staging` push; gated Trigger Stable Release + Stable Release for signed stable builds).

## Failure Handling

Design deploy operations so success is observable even if the app restarts networking components mid-deploy. Prefer async jobs plus polling over a single long streamed HTTP response. Include diagnostics such as `docker compose ps`, recent logs, current image tags, and health check failures in failed job output.
