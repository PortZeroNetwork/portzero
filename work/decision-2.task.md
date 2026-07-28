---
id: 172b5f0f-f7e5-4d44-beb7-4680b8f0bb7c
slug: decision-2
status: done
title: 'ADR: Multi-user strategy — shared machines, non-consenting accounts, and who owns the overlay'
ticket_type: decision
created_at: 2026-07-28T00:00:00Z
updated_at: 2026-07-28T00:00:00Z
---

## Context

"Multiple users" means three different things for Port Zero, and only one of
them has a designed answer today:

1. **People consuming a tunnel without installing Port Zero.** Solved by
   design: Cloud tunnels (`…--<username>.tunnel.portzero.cloud`) are public
   DNS names served by the edge servers — consumers need only a browser or
   TCP client. Local tunnels are intentionally unreachable to them: the
   scoped resolver and virtual IPs exist only on the publishing machine.
2. **Multiple Port Zero users collaborating.** Solved by namespacing: cloud
   hostnames embed `--<username>`, and `.portzero.local` names are
   per-machine, so names never collide across users.
3. **Multiple OS accounts sharing one machine, only some of whom want Port
   Zero.** No story exists, and the [[task-29]] elevation decision (root
   LaunchDaemon on macOS, Administrator task on Windows) created a de-facto
   model nobody chose:
   - `autostart.rs` pins the LaunchDaemon's `HOME` to the *installing*
     user's home, so one machine-wide root daemon reads one user's
     `~/.portzero` config, credentials, and CA. The first admin install
     silently annexes the machine; a second account's install overwrites
     the plist.
   - Running as root, discovery reads **every** account's process
     environments (`sysctl KERN_PROCARGS2` in
     `discovery/process/platform.rs`). Another account's process that sets
     `PZ_TUNNEL` — e.g. from a repo's committed `.env` — gets tunneled
     without consent, and a cloud value publishes it **under the owner's
     account**.
   - Side effects are machine-wide: the utun device, routes,
     `/etc/resolver/portzero.local`, and (after `sudo portzero trust
     install`) a **system-trusted CA whose private key lives in one
     non-root user's home** (`~/.local/share/PortZero`). The CA is
     name-constrained to `portzero.local`, which caps the blast radius, but
     every account trusting key material one user's files control is still
     a real exposure.
   - Linux is accidentally cleaner: a per-user systemd user service holding
     only `CAP_NET_ADMIN` cannot read other accounts' `/proc/<pid>/environ`,
     so discovery is naturally owner-scoped — but two accounts starting
     daemons would contend for the TUN device and resolver, and nothing
     detects or explains that today.
   - A non-consenting account has no opt-out short of persuading an admin
     to uninstall.

This ADR decides the model for sense 3. Senses 1–2 are recorded above as
already-decided context.

## Options

1. **Single owner per machine, made explicit and safe (harden the status
   quo).** Keep one privileged daemon owned by the installing user, but:
   filter discovery to processes owned by the owner's UID (root can see
   everyone; it must not act on everyone); move the CA private key to a
   root-owned location so system-wide trust never rests on user-writable
   files; document that Port Zero is single-owner-per-machine and make a
   second account's install fail loudly with a pointer to the owner;
   other accounts keep passive benefits (`*.portzero.local` resolves, they
   can consume the owner's local tunnels) but are never scanned or
   published. Opt-out for other accounts = admin uninstall, now documented.
2. **Per-user daemons everywhere (generalize the Linux model).** Every
   account runs its own unprivileged daemon; machine-wide resources become
   per-user (own TUN device, own virtual IP range) or brokered (resolver
   must fan out to N daemons). No shared state, perfect consent — but macOS
   and Windows have no `setcap` equivalent ([[task-29]]), so each user needs
   an elevation path, and the scoped-resolver and trust stories multiply.
3. **One root broker daemon with per-user sessions.** A single privileged
   daemon owns the TUN/DNS/routes but holds no user identity; each account
   that opts in registers a session (own config, own cloud credentials) and
   the daemon discovers/tunnels only registered accounts' processes,
   publishing cloud tunnels under the session's identity. Cleanest
   long-term multi-user model; a substantial re-architecture of config,
   auth, and the management API.

## Decision

**Ratified 2026-07-28: Option 1 now, with Option 3 as the recorded
long-term direction if shared-machine demand materializes.**

Rationale: today's users are developers on machines they own alone; the
urgent problems are the consent and key-custody defects Option 1 fixes with
small, local changes (a UID filter in discovery, a root-owned CA path, an
install-time ownership check, docs). Option 2 multiplies the hardest
platform work (elevation, resolver, trust) by N users for a case we have no
demand signal on. Option 3 is the right shape if that demand appears, and
nothing in Option 1 forecloses it — the UID filter becomes "registered
sessions' UIDs", and root-owned key custody is a prerequisite for it anyway.

- [x] Decision ratified (owner sign-off, 2026-07-28)
- [x] Implementation tickets cut: [[task-94]] discovery UID filter;
      [[task-95]] root-owned CA key custody; [[task-96]] install-time
      single-owner check; [[task-97]] privacy-notice update
      (docs/users/privacy.md and its portzero.net/docs twin must state that
      the daemon reads process environments and how that is scoped) plus
      the shared-machine section in the published docs

## Consequences

- The daemon stops acting on processes it can merely see: discovery gains
  an owner-UID filter on macOS/Windows (Linux already behaves this way for
  free). A second account's `PZ_TUNNEL` becomes inert instead of silently
  publishing under the owner's identity.
- System-wide trust stops depending on user-writable key material; `portzero
  trust install` and the CA generation path move the key under a root-owned
  directory on platforms where the daemon runs privileged.
- "Port Zero is single-owner-per-machine" becomes documented, checked at
  install, and stated in the privacy notice — instead of being an accident
  of whichever account ran `sudo` first.
- Other accounts on the machine remain passive consumers of local tunnels;
  if that is ever unwanted, it is a follow-up toggle, not part of this
  decision.
- Option 3 (broker with per-user sessions) is deliberately deferred, not
  rejected; revisit when a real shared-machine or CI-host use case shows up.
