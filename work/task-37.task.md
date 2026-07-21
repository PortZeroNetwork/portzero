---
id: b65e9bb0-5cfb-4d4a-90a0-b567aebfa3ac
slug: task-37
status: todo
title: 'macOS: stale utun interfaces + misrouted overlay route accumulate across restarts (task-18 analog)'
milestones:
- milestone-2
created_at: 2026-06-23T20:24:08.265610Z
updated_at: 2026-06-23T20:24:08.265610Z
---

## Context

Found during [[[task-24](../work/task-24.task.md)]]/[[[task-36](../work/task-36.task.md)]] macOS bring-up. After several daemon
restarts (especially `pkill -9`, which skips graceful teardown), the overlay
data path silently breaks: `hello.devenv.local` resolves to its VIP and the
service is correctly registered, but `curl` to the VIP times out (75s, no
connection).

Observed state after a few `-9` restarts:

```
utun4: inet 10.254.0.1 --> 10.0.0.255   (stale)
utun5: inet 10.254.0.1 --> 10.0.0.255   (stale)
utun6: inet 10.254.0.1 --> 10.0.0.255   (stale)
utun7: inet 10.254.0.1 --> 10.0.0.255   (live daemon)
netstat: 10.254/16 -> utun6             (route points at a STALE/dead interface)
```

Two distinct macOS bugs:

1. **Stale utun interfaces accumulate.** Each daemon creates a fresh `utunN`
   with `10.254.0.1`; on an unclean exit the interface lingers (and even
   `ifconfig utunN destroy` is rejected for kernel-control utuns). Nothing on
   startup detects/cleans leftovers. (Linux has [[[task-18](../work/task-18.task.md)]] for `deven0`; macOS
   has no analog — and the `delete_device_command` path is `None` on macOS by
   design since utun units are kernel-assigned.)
2. **The route isn't repointed.** `route_add` treats a pre-existing
   `10.254/16` route as success ("File exists"), so when an earlier (now dead)
   daemon left `10.254/16 -> utun6`, the new daemon on `utun7` never repoints
   it — traffic to the VIP goes to the dead `utun6` and is black-holed.

## Approach

- On startup AND on clean teardown, reconcile the overlay route to point at the
  CURRENT daemon's utun: delete any existing `10.254.0.0/16` route and re-add it
  for the freshly created interface (don't silently accept "File exists" when it
  may point elsewhere). Restore the route to the live device.
- Best-effort cleanup of leftover overlay interfaces on macOS where possible;
  where the kernel won't let us destroy a stuck utun, at least ensure the route
  targets the live one so stale interfaces are inert.
- Make graceful teardown reliably remove the route + (where possible) the
  interface — and note this is much less likely to leak now that Ctrl-C works
  ([[[task-32](../work/task-32.task.md)]]); the worst accumulation came from `pkill -9`.
- Consider detecting "multiple interfaces hold 10.254.0.1" and warning.

## Verify

- After several start/stop cycles (including an unclean kill), a fresh daemon's
  `10.254/16` route points at ITS utun and `curl http://hello.devenv.local:8080/`
  works without manual `route delete`.

Done when:

- [ ] New daemon repoints `10.254/16` to its own utun even if a stale route exists
- [ ] Startup logs/handles leftover `10.254.0.1` interfaces sensibly
- [ ] Graceful teardown removes the route (and interface where possible)
- [ ] Verified across restart cycles on macOS; Linux ([[[task-18](../work/task-18.task.md)]]) unaffected