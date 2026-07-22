---
id: f1ed2080-9317-4764-a596-c76156310977
slug: task-92
status: todo
title: 'daemon: discovery loop stalls minutes before re-registering a restarted process (stale dead-pid service)'
created_at: 2026-07-22T17:39:53.288166870Z
updated_at: 2026-07-22T17:55:03.564836822Z
---

Reproduced during task-91 verification (2026-07-22): after killing and restarting the demo web process, the discovery loop went ~6 minutes without re-registering it despite logging 'Scanning every 2s'; the dead pid's service and stale backend lingered meanwhile. Also seen earlier the same day as a removed container lingering in a tunnel-name conflict until daemon restart. Browser auto-open is spawn-detached, so the stall is elsewhere in the loop iteration.

Repro: full-example repo, start DemoWeb with PZ_TUNNEL=<name>.portzero.local, kill it, restart it, watch `portzero inspect` / MCP list_services.

Origin: task-91 findings (left open there).

---

More evidence (2026-07-22 evening, while adding the full-example `mcp-local-obs` walkthrough step; new-daemon build with the task-91 fix):

- **Stale registration blocks name re-use.** Restarting an app under a tunnel name used minutes earlier fails hard: `portzero status` keeps showing the DEAD pid as the owner of `web.pzdemo.portzero.local`, and `portzero wait <name>` times out at 90s for the new process. Hit this twice in a row; only a daemon restart cleared it. This makes any restart-under-the-same-name workflow (walkthroughs, dev iteration) flaky.
- **One run showed a subtler split-brain**: right after a daemon restart, `portzero wait` (health) succeeded for the newly discovered process, but external by-name HTTP connects from another process silently failed (walkthrough HttpClient probes never arrived; nothing tapped). Overlay DNS still resolved the VIP fine. Suggests the VIP→backend forwarding can go stale/half-initialized separately from health checking.
- **`portzero stop` can report "Daemon stopped" while the `portzero start --foreground` process keeps running** (status then still shows the old PID); needed SIGKILL. Also seen at 16:46: a spawned daemon "did not confirm startup" yet was fully up per its log, while `portzero status` said stopped.
