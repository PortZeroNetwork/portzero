---
id: af741f08-3311-48c3-a459-8cde3f518a90
slug: task-2
status: done
title: Implement scoped local DNS resolver configuration (macOS, Linux, Windows)
relations:
  contains:
  - milestone-1
created_at: 2026-06-21T11:12:11.235958Z
updated_at: 2026-06-21T11:12:11.235958Z
---

## Description
Do not hijack entire DNS. Implement scoped configuration so only *.devenv.local queries hit our embedded DNS server:

- macOS: write /etc/resolver/devenv.local (requires privileges)
- Linux: interact with systemd-resolved via D-Bus or resolvectl
- Windows: use Add-DnsClientNrptRule (PowerShell)

The daemon should set this up on start (or on first use), and clean up on shutdown. Handle privilege elevation if needed.

## Acceptance Criteria
- [ ] macOS scoped resolver works
- [ ] Linux scoped works (tested on systemd)
- [ ] Windows NRPT rule
- [ ] Reversible / clean teardown
- [ ] Only affects devenv.local
