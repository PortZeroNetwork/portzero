---
id: 1e7503b4-311d-4ab0-9199-8fe571d534f1
slug: task-81
status: todo
title: 'portzero wait --healthy: TCP connect check for non-HTTP tunnels'
created_at: 2026-07-22T10:29:58.412660528Z
updated_at: 2026-07-22T10:29:58.412660528Z
---

Observed in portzero-full-example CI (System Test — Local Tunnels): `portzero wait db.pzdemo.portzero.local --healthy` probes http://db.pzdemo.portzero.local:5432/ against a raw-TCP Postgres tunnel and can never succeed, so tunnel-action's default healthy:true guarantees a 60s timeout for any-TCP tunnels.

Fix: when the tunnel declares no PZ_HEALTH_PATH and/or is not HTTP, --healthy should fall back to a TCP connect check against the tunnel domain:port instead of an HTTP GET. tunnel-action could then keep healthy:true as a safe default for all tunnel types.

Interim workaround shipped: the example repo's workflow passes healthy:false for the db tunnel.