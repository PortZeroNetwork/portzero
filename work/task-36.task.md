---
id: cdd98720-0620-4919-941b-ba016618cdc4
slug: task-36
status: done
title: 'macOS lsof discovery missing -a: ORs selections, attributes ALL system ports to every pid (maps services to sshd:22)'
milestones:
- milestone-2
created_at: 2026-06-23T19:47:25.981500Z
updated_at: 2026-06-23T19:47:25.981500Z
---

## Context

Found by the [[[task-24](../work/task-24.task.md)]] end-to-end run on macOS. The overlay carried traffic
correctly (utun header fix works) but `curl http://hello.devenv.local:8080/`
reached an **SSH server on port 22**, not the Python backend:

```
$ curl --http0.9 -s http://hello.devenv.local:8080/
SSH-2.0-OpenSSH_9.9
Invalid SSH identification string.
```

`overlay.json` confirmed the mismapping: `hello.devenv.local → real_addr
"127.0.0.1:22"` (the backend should have been the python server's `:51994`).

Root cause: `discovery.rs::discover_ports_lsof` ran
`lsof -iTCP -sTCP:LISTEN -nP -p <pid>`. **lsof ORs selection criteria by
default**, so `-iTCP -sTCP:LISTEN` (all listening TCP, system-wide) is *unioned*
with `-p <pid>` — lsof returns EVERY listening socket on the machine, not just
that pid's. `scan_network_processes` then picks `find(bind == Public)`, and
`sshd`'s `*:22` is public, so the service backend resolves to port 22.

This is also the root cause of the legacy-monitor flood ([[[task-35](../work/task-35.task.md)]]):
`enumerate_system_listeners` calls the same function per pid, so every process
appears to listen on the whole system's ports (hence "Port 22 served by pid 606
/ 85521 / 98783" — different pids, same bogus ports).

Distinct from [[[task-34](../work/task-34.task.md)]] (the `(LISTEN)` token parse), which was also real and
is still needed.

## Fix (applied)

Add `-a` to the lsof invocation so the selections are ANDed:
`lsof -a -iTCP -sTCP:LISTEN -nP -p <pid>` — returns only that pid's listening
TCP sockets.

## Verify (privileged re-run by the user)

- `curl http://hello.devenv.local:8080/` returns the python body
  (`Hello from the port-zero local overlay!`), not an SSH banner.
- `overlay.json` shows `real_addr` = the python backend port (e.g. `:51994`).
- The `Port 22 …` legacy warnings stop (confirms [[[task-35](../work/task-35.task.md)]] is resolved too).

Done when:

- [x] `discover_ports_lsof` uses `-a` (only the target pid's ports)
- [x] Builds clean with `clippy -D warnings`
- [x] Privileged re-run: service maps to the real backend; curl returns the body
- [x] `Port 22` legacy spam gone (verify [[[task-35](../work/task-35.task.md)]])

## Verified (2026-06-23, macOS, as root)

After the `-a` fix, `overlay.json` mapped `hello.devenv.local → 127.0.0.1:52075`
(the real python backend, not `:22`), and once the stale route was cleared
([[[task-37](../work/task-37.task.md)]]) `curl http://hello.devenv.local:8080/` returned the python body.