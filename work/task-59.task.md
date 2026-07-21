---
id: c73967dd-9f09-49df-8408-07600fd4c0ad
slug: task-59
status: done
title: Periodic regression detection + notifications for all post-setup steps
created_at: 2026-07-04T22:30:56.977272Z
updated_at: 2026-07-04T22:30:56.977272Z
---

Extend the daemon self-healing/alarm treatment (task-58, scoped resolver) to
EVERY other post-setup step across macOS, Linux, and Windows.

The daemon already ran a diagnostics pass every ~30s that only *wrote a report*
(surfaced by `portzero status`) for most post-setup steps but never *notified*.
This closes that gap plus adds two missing periodic checks.

Changes:
- diagnostics.rs: new check_dashboard_hosts_pin (macOS+Linux) detects the
  10.254.0.2 portzero.local /etc/hosts pin being removed out-of-band; new
  check_autostart_installed (all platforms) detects the LaunchDaemon / systemd
  unit / scheduled task being removed. Added is_post_setup_regression()
  classifier + POST_SETUP_REGRESSION_IDS.
- discovery_loop.rs: notify_post_setup_regressions() turns the setup-related
  diagnostics (CA cert/trust, cap_net_admin, systemd-resolved, wintun,
  dev/net/tun, hosts pin, autostart) into ONE deduped desktop notification per
  episode, pointing at the concrete fix. Gated on overlay.is_some() so a dev
  running the daemon in the foreground is never nagged. macos_resolver_missing
  excluded (handled by the existing resolver self-heal to avoid double-notify).
- Homebrew caveats updated.

Auto-repair is kept to the resolver (task-58); other steps need root the
user-mode/user-service daemon lacks, so they alarm + point at `sudo portzero setup`.
