---
id: 078b63c3-ab20-40fc-a3ba-0dce518777c6
slug: task-26
status: archived
title: Harden macOS process/port discovery (lsof accuracy + system-wide perf)
relations:
  contains:
  - milestone-2
depends_on:
- 2a8cf42b-859e-449a-802b-6f8ffa24caf0
created_at: 2026-06-23T12:51:59.846197Z
updated_at: 2026-06-23T12:51:59.846197Z
---

## Context

On Linux, discovery is precise and cheap: `/proc/<pid>/fd` gives socket inodes,
`/proc/net/tcp` maps them to listening ports, and `/proc/<pid>/environ` reads
`PZ_TUNNEL` directly (`discovery.rs`). macOS has neither, so the macOS
branch shells out to `lsof` for ports and `ps -wwwE` for env vars. Two concerns:

1. **Accuracy** — `discover_ports_lsof` parses `name` columns heuristically and
   has no inode-level scoping; `scan_process_env_macos` parses `ps` output,
   which truncates and is racy.
2. **Performance** — `enumerate_system_listeners` (legacy-port monitor) calls
   `discover_process_ports` per PID system-wide, i.e. **one `lsof` spawn per
   process**. On a busy Mac that's slow and noisy.

## Approach

Gate the depth of work on what [[[task-22](../work/task-22.task.md)]] actually shows is wrong — don't
over-engineer if accuracy is already fine. Likely items:

- Replace the per-PID `lsof` fan-out in `enumerate_system_listeners` with a
  **single** system-wide `lsof -nP -iTCP -sTCP:LISTEN` pass, grouped by PID.
- Consider `libproc` (`proc_pidinfo` / `proc_pidfdinfo`) to drop the `lsof`/`ps`
  subprocess dependency entirely on macOS — closer to the `/proc` model.
- Verify `PZ_TUNNEL` detection is reliable for the long, templated values
  the project uses (no `ps` truncation).

Done when:

- [ ] `.devenv.local` services are discovered and attributed to the right PID
      on macOS (verified via [[[task-22](../work/task-22.task.md)]])
- [ ] System-wide listener enumeration no longer spawns one process per PID
- [ ] `PZ_TUNNEL` env detection is reliable (not truncated by `ps`)
- [ ] No Linux-path behaviour change
