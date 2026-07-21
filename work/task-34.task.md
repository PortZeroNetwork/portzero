---
id: f039a3ba-4461-44ec-8b0c-350ecc4f9be5
slug: task-34
status: done
title: macOS lsof port discovery parses (LISTEN) as the address — services never register
milestones:
- milestone-2
created_at: 2026-06-23T15:50:27.463637Z
updated_at: 2026-06-23T15:50:27.463637Z
---

## Context

Found by the [[[task-22](../work/task-22.task.md)]] re-run after [[[task-31](../work/task-31.task.md)]] + [[[task-33](../work/task-33.task.md)]]: with the DNS
server now reachable on `127.0.0.1:10053`, `dig @127.0.0.1 -p 10053
hello.devenv.local` returns **empty** and `status` shows **no routes** — the
discovered `.devenv.local` service never registers, even though `lsof` proves
the process is listening and the daemon logs `routing to overlay path … domain="hello.devenv.local"`.

Root cause: `discovery.rs::discover_ports_lsof` (~line 540) parses the wrong
token. `lsof -iTCP -sTCP:LISTEN -nP -p <pid>` prints the NAME column as:

```
Python  32520 loumtech  3u  IPv4 0x...  0t0  TCP  127.0.0.1:50706 (LISTEN)
```

The code does `parts.last()` to get the address, but the last
whitespace-delimited token is `"(LISTEN)"`, not `127.0.0.1:50706`. So
`"(LISTEN)".rsplit_once(':')` is `None`, no port is parsed, the function returns
empty, and `scan_network_processes` skips the process (discovery.rs:998-1001 →
`continue`). Net effect: **no VIP, no DNS record, no route.**

On Linux `discover_ports_lsof` is only a fallback (the `/proc` path is primary),
so this rarely bit. On **macOS it is the ONLY port-discovery path**, so overlay
discovery has never worked here. The same bug also corrupts
`enumerate_system_listeners` (the legacy-port monitor) on macOS.

## Approach

- Fix the parser to extract the `addr:port` token regardless of a trailing
  `(LISTEN)` (or other `(STATE)`) field. Robust approach: scan the line's tokens
  and pick the one that `rsplit_once(':')`s into a parseable `u16` port (skip
  `(LISTEN)` and non-address columns). Handle IPv4 (`127.0.0.1:50706`,
  `*:8080`, `0.0.0.0:8080`) and IPv6 (`[::1]:57889`, `[::]:8080`) forms, keeping
  the existing Public-vs-Loopback bind classification.
- This is **pure parsing** — add unit tests with realistic `lsof` lines
  (including the trailing `(LISTEN)`), so it's fully verifiable without root.

## Verify

- Unit tests on sample `lsof` output assert the port + bind are extracted.
- Privileged re-run ([[[task-22](../work/task-22.task.md)]] runbook): `dig @127.0.0.1 -p 10053
  hello.devenv.local` now returns the `10.254.x.y` VIP and `status` shows the
  overlay route. (Then the only remaining DNS question is macOS routing `.local`
  through `/etc/resolver` → [[[task-25](../work/task-25.task.md)]].)

Done when:

- [x] `discover_ports_lsof` extracts the address/port past a trailing `(LISTEN)`
- [x] Unit tests cover IPv4/IPv6 + the `(LISTEN)` suffix
- [x] Linux behaviour unchanged; builds clean with `clippy -D warnings`
- [x] Privileged re-run: service registers (`status` + `dig @127.0.0.1` show it)

Related broader hardening (single-pass lsof, libproc) stays in [[[task-26](../work/task-26.task.md)]].

## Verified (run 2026-06-23, macOS, as root)

With the parser fix (`6e0c366`):

```
$ dig @127.0.0.1 -p 10053 hello.devenv.local +short
10.254.0.2
$ port-zero status
DOMAIN              PORT  SOURCE
hello.devenv.local  8080  PID 91511
```

The service now registers, gets VIP `10.254.0.2`, and the DNS server answers.
Confirmed root cause and fix. (The remaining `curl` timeout is the data-path
header issue → [[[task-24](../work/task-24.task.md)]], not discovery.)