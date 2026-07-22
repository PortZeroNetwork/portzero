---
id: 0aad4464-cec0-4267-afe7-1d1707b819cd
slug: task-80
status: todo
title: 'tunnel-action: OIDC cloud-tunnel auth + container-job support'
created_at: 2026-07-22T10:24:55.908469661Z
updated_at: 2026-07-22T10:24:55.908469661Z
---

Upgrade the tunnel-action composite action:

- New inputs: oidc-team (default ''), api-url (default 'https://app.portzero.cloud/api'). When oidc-team is set, start.sh exchanges the GitHub OIDC token (audience portzero.cloud) at POST {api}/auth/github-oidc/exchange for a short-lived tunnel token, writes ~/.portzero/auth.json (chmod 600), and restarts the daemon if already running. Token masked with ::add-mask::.
- Container-job support: replace the old warning with a working path. Requires root + /dev/net/tun (clear ::error:: with the exact container: options fix otherwise); installs via scripts/cloud-agent-env-install.sh (sibling path preferred, raw.githubusercontent fallback pinned to GITHUB_ACTION_REF); PORT_ZERO_VERSION honors the version input; apt-installs curl/python3/ca-certificates when missing.
- teardown.sh: also kills the split-DNS forwarder (/run/pzlocal-dns.pid) and restores /etc/resolv.conf from /etc/resolv.conf.pz-backup in the container path.
- README: new 'Cloud tunnels via GitHub OIDC' and 'Container jobs' sections; Limitations updated.

Scope: tunnel-action/ only. Follow-up to task-65.