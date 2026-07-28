---
id: 2bbeb58e-05d5-469a-aac8-fe23ed600e49
slug: task-95
status: todo
title: 'TLS: move the local CA private key to root-owned custody on privileged platforms'
created_at: 2026-07-28T00:00:00Z
updated_at: 2026-07-28T00:00:00Z
---

## Context

From [[decision-2]] (multi-user strategy, Option 1). Today the local CA —
including its **private key** — lives in the owner's data dir
(`tls/ca.rs::data_dir`, e.g. `~/.local/share/PortZero`), while `sudo portzero
trust install` puts the CA cert into the **system** trust store. Every
account on the machine then trusts key material that one non-root user's
files control. The name constraint to `portzero.local` caps the blast
radius but does not remove the exposure.

## Approach

On platforms where the daemon runs privileged (macOS root LaunchDaemon,
Windows Administrator service), generate and store the CA key under a
root-owned directory (e.g. `/Library/Application Support/PortZero` on macOS)
with `0600`-equivalent permissions; keep only the **cert** user-readable for
trust installation and inspection. On Linux the daemon is an unprivileged
user service and system-wide trust already requires an explicit `sudo
portzero trust install`, so decide and document whether the same move applies
there or the per-user location stays.

Include a migration: on first privileged start, if a legacy user-owned CA
exists, either relocate it (preserving browser trust) or regenerate and
re-run trust installation, with a clear user-facing message either way.

Done when:

- [ ] CA private key is created in, and only readable from, a root-owned
      location on macOS and Windows
- [ ] `portzero trust install` / `trust generate` and the daemon's
      `load_or_create` path agree on the new location
- [ ] Legacy user-owned CA is migrated or regenerated with a clear message
      (see `.instructions/user-facing-errors.md` for tone and content)
- [ ] Linux decision recorded here (move vs. stay per-user) and implemented
- [ ] `docs/users/privacy.md` storage paths updated (with its
      portzero.net/docs twin — see [[task-97]])
