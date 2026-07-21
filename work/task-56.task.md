---
id: c480133e-58b7-483d-8950-e1a9dcc0d40b
slug: task-56
status: done
title: Change default tunnel domain to the *.tunnel.portzero.cloud namespace
created_at: 2026-07-03T19:11:53.693356Z
updated_at: 2026-07-03T19:11:53.693356Z
priority: critical
preferred-model: fable
---

## Context

Launch blocker found in the 2026-07-03 cross-repo review. The client's default template is {service}-{project}-{branch}.{username}.portzero.cloud (client/crates/domain/src/lib.rs DEFAULT_TEMPLATE) and validate_tunnel_domain requires *.<username>.portzero.cloud. The cloud control plane only treats *.{username}.tunnel.portzero.cloud as tunnel space: the API rejects anything else as a custom domain (402 unless Organization plan), Caddy only proxies *.tunnel.portzero.cloud to the edge (everything else 301s to portzero.net), and the wildcard cert can't cover two-label subdomains. Net: with the current client defaults, no cloud tunnel works at all. There are zero references to '.tunnel.' anywhere in this repo.

## Approach

Insert the tunnel scope into the default: DEFAULT_TEMPLATE → {service}-{project}-{branch}.{username}.tunnel.portzero.cloud, and update validate_tunnel_domain (and PZ_TUNNEL docs/examples like 'api.alice.portzero.cloud', notify.rs messaging, diagnostics '*.portzero.cloud' checks, tests) to the new base. Keep PZ_TUNNEL_BASE_DOMAIN override working (staging uses a different domain). Beware: the resulting hostname must still be a single DNS label per segment ≤63 chars. Coordinate with portzero-cloud task-36 (interop E2E test) and cut a client release once merged.