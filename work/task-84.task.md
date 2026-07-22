---
id: 26e2a097-e502-473d-b252-3dcc79f1edf0
slug: task-84
status: todo
title: portzero wait --healthy fails TLS on local tunnels (rustls webpki roots ignore OS trust store)
created_at: 2026-07-22T10:40:49.881517974Z
updated_at: 2026-07-22T10:40:49.881517974Z
---

Found by portzero-full-example CI: `portzero wait web.pzdemo.portzero.local --healthy` failed for 60s with the daemon logging repeated 'TLS handshake: received fatal alert: BadCertificate'. The CLI's reqwest is built with rustls-tls (webpki roots only), so the local CA that portzero setup installs into the OS trust store is never consulted by the health probe.

Fix: wait.rs now appends the daemon's local CA (LocalCa::ca_cert_path) to the probe client's root store. Cloud tunnels unaffected (root is appended, webpki roots still validate public certs).