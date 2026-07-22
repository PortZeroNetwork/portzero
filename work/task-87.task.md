---
id: 4ed473d0-3075-45e4-9b1e-4493891cc47a
slug: task-87
status: todo
title: 'Ubuntu 22.04: scoped resolver denied — polkit 0.105 has no rules.d (need .pkla)'
created_at: 2026-07-22T12:45:35.978524737Z
updated_at: 2026-07-22T12:45:35.978524737Z
---

Found by portzero-full-example CI (22.04 leg, post-v0.0.4): installer warns 'polkit rules directory not found', daemon logs 'busctl SetLinkDNS: exit status 1', and *.portzero.local resolution fails with EAI_AGAIN. Ubuntu 22.04 ships polkit 0.105 which reads .pkla files from /etc/polkit-1/localauthority, not JS rules from rules.d. Fix (staged): linux-install.sh now writes a .pkla fallback when rules.d is absent. Takes effect at the next release; 22.04 CI leg is expected-fail until then.