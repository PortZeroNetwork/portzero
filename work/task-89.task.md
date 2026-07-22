---
id: fb6cfb66-dbec-4be0-b887-24a4beb3509e
slug: task-89
status: todo
title: 'Native CLI OIDC consumption: portzero login --github-oidc'
created_at: 2026-07-22T13:03:17.960924351Z
updated_at: 2026-07-22T13:03:17.960924351Z
---

tunnel-action's start.sh currently hand-writes ~/.portzero/auth.json after the OIDC exchange (see tunnel-action task-80). Ship 'portzero login --github-oidc --team <slug>' that reads ACTIONS_ID_TOKEN_REQUEST_URL/TOKEN, does the exchange, and saves credentials via AuthConfig — then shrink start.sh to a wrapper. Mirrors login --github-repo (task-83 machinery).