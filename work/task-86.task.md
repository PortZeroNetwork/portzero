---
id: 3b48c23d-d682-45a4-91e8-03747ef8b529
slug: task-86
status: todo
title: 'Daemon TLS termination: nested .portzero.local domains get a cert with no matching SAN'
created_at: 2026-07-22T12:40:18.020127762Z
updated_at: 2026-07-22T12:40:18.020127762Z
---

Found by portzero-full-example CI: curl to https://web.pzdemo.portzero.local/ fails with 'no alternative certificate subject name matches' even with the local CA trusted. The daemon terminates TLS with a *.portzero.local certificate; a single-level wildcard cannot match nested names like web.pzdemo.portzero.local (RFC 6125). Fix: mint per-SNI leaf certificates (exact-name, or per-domain wildcard like *.pzdemo.portzero.local) at handshake time. Workaround shipped in the example repo: single-level tunnel names for HTTPS local tunnels.