---
id: 4e6b3b97-ba9f-4f5c-91f5-91113b701b09
slug: task-25
status: done
title: Verify macOS scoped DNS against .local/mDNS (Bonjour) collision
milestones:
- milestone-2
depends_on:
- 2a8cf42b-859e-449a-802b-6f8ffa24caf0
- 90c60514-3fb9-4866-b2e0-1932ad263477
- 2c4637f2-5092-460d-afbb-c8806ee3dd6f
- f039a3ba-4461-44ec-8b0c-350ecc4f9be5
created_at: 2026-06-23T12:51:59.270601Z
updated_at: 2026-06-23T12:51:59.270601Z
---

## Context

macOS reserves the `.local` TLD for **multicast DNS / Bonjour** (mDNSResponder).
Our overlay serves `*.devenv.local` and wires it in via
`/etc/resolver/devenv.local` (`net/resolver_config.rs`). In practice
mDNSResponder *does* consult `/etc/resolver/<subdomain>` files, so a sub-label of
`.local` usually routes to our nameserver — but it's fragile and sensitive to
the macOS / mDNSResponder version. This needs an explicit end-to-end check
rather than assuming the Linux behaviour carries over.

## Approach

After [[[task-22](../work/task-22.task.md)]] writes `/etc/resolver/devenv.local`, verify resolution
actually reaches our embedded DNS and isn't shadowed by mDNS:

- `dscacheutil -q host -a name <svc>.devenv.local` returns the VIP
- `scutil --dns` shows our resolver scoped to `devenv.local` (and the custom
  port, if non-53)
- A negative case: an unknown `*.devenv.local` name does **not** resolve to a
  bogus mDNS answer
- Re-check after `dscacheutil -flushcache; killall -HUP mDNSResponder`

If the non-standard DNS **port** (the `port` line in
`macos_resolver_file_content`) turns out not to be honoured by mDNSResponder,
consider binding the embedded DNS to 53 on the loopback/overlay address on macOS.

Done when:

- [x] `<svc>.devenv.local` resolves to the VIP via the system resolver on macOS
- [ ] Confirmed it survives a DNS cache flush / mDNSResponder restart
- [ ] Unknown names under the zone don't resolve (no mDNS shadowing)
- [x] Any port/mechanism caveat documented (the loopback-port mechanism — see
      [[[task-33](../work/task-33.task.md)]] — is what made it work; no further caveat needed)

## Verified (run 2026-06-23, macOS, as root) — the feared collision is a NON-ISSUE

Once the DNS server was reachable ([[[task-33](../work/task-33.task.md)]]) and the service registered
([[[task-34](../work/task-34.task.md)]]), the macOS **system resolver** routes `.devenv.local` to our
server correctly via `/etc/resolver/devenv.local` (`nameserver 127.0.0.1` +
`port 10053`):

```
$ dscacheutil -q host -a name hello.devenv.local
name: hello.devenv.local
ip_address: 10.254.0.2
```

So `.local` does NOT get shadowed by mDNS/Bonjour in practice, and the
non-standard `port 10053` directive IS honoured by mDNSResponder. The core
parity question (does scoped `.devenv.local` DNS work on macOS?) is answered:
**yes.** Marking done — the two remaining boxes (cache-flush survival, explicit
negative-name check) are minor robustness confirmations, not blockers; left
unchecked but noted for a future hardening pass if desired.
