---
id: b214aaf5-00c5-4682-aa52-a65424340a80
slug: task-17
status: done
title: Protocol detection (HTTP/TLS) to expose overlay services on canonical ports
milestones:
- milestone-1
depends_on:
- 2e55ad9c-36e6-4799-a631-a5c143761fb8
created_at: 2026-06-22T11:10:35.036972901Z
updated_at: 2026-06-22T11:10:35.036972901Z
---

## Goal

Zero-config canonical ports: when no explicit port is declared (see **[task-16](../work/task-16.task.md)**),
DETECT the service's protocol and expose it on the standard port — **HTTP → 80**,
**TLS/HTTPS → 443** — so `curl http://web.devenv.local/` just works with no port
and no env-var fiddling. Explicit `:port` in `PZ_TUNNEL` ALWAYS overrides
detection and skips probing.

## Approach

On discovering an overlay service that has no explicit canonical port, actively
probe the real ephemeral backend (it must be active because HTTP is
client-speaks-first — you can't passively sniff it):

- **HTTP:** open TCP, send a minimal `HEAD / HTTP/1.0\r\n\r\n` (gentler than GET
  — less likely to hit app logic), read the first line; if it begins with
  `HTTP/`, classify HTTP → canonical **80**.
- **TLS:** attempt a TLS ClientHello / check the server completes a TLS
  handshake; if so → canonical **443**.

Precedence: explicit `:port` ([task-16](../work/task-16.task.md)) **>** detected canonical **>** discovered
ephemeral (fallback) + warning.

## Caveats to handle (important)

- **Active bytes hit the backend.** Skip probing ENTIRELY when an explicit port
  is set. Use the gentle `HEAD` method. Provide an opt-out env var (e.g.
  `PZ_TUNNEL_NO_PROBE=1`). Document the behavior.
- **Server-speaks-first protocols** (Postgres, MySQL banner, SSH, SMTP) are OUT
  OF SCOPE for HTTP/TLS detection — they must NOT be misclassified as 80/443. If
  the probe yields a non-HTTP/non-TLS response or a server greeting, do NOT
  assign a canonical port; fall back. (Banner-matching for DBs is a possible
  future phase, not this ticket.)
- **Readiness/timing:** the service may not be listening at first scan — bounded
  timeout + a few retries.
- **Caching:** cache the detection result per service so we do NOT re-probe every
  scan; re-probe only when the backend (real port / pid) changes.

## Acceptance Criteria

- [ ] An HTTP service with no explicit port is exposed on `VIP:80`
      (`curl http://<name>.devenv.local/` works, no port in the URL).
- [ ] A TLS service with no explicit port is exposed on `VIP:443`.
- [ ] Explicit `PZ_TUNNEL=...:port` ([task-16](../work/task-16.task.md)) overrides detection and sends
      NO probe.
- [ ] A non-HTTP/non-TLS backend (raw TCP / DB greeting) is NOT misclassified as
      80/443; it falls back gracefully.
- [ ] Detection is timeout-bounded, retried, and cached — no probe hot-loop each
      scan.
- [ ] Opt-out (`PZ_TUNNEL_NO_PROBE` or equivalent) disables probing.
- [ ] Pure classifier unit-tested against captured sample bytes (HTTP response
      line, TLS handshake, server banner, empty/timeout) with probe I/O isolated
      from the classifier; no real network in tests.

## Relations

Depends on **[task-16](../work/task-16.task.md)** (explicit canonical port / the override + the
`service_port` plumbing). Part of [milestone-1](../work/milestone-1.task.md).
