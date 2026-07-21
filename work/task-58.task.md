---
id: fd745619-30b3-4b86-92ef-53e146fea777
slug: task-58
status: todo
title: Self-heal scoped .portzero.local resolver + install it in setup
created_at: 2026-07-04T22:20:47.139059Z
updated_at: 2026-07-04T22:20:47.139059Z
---

The daemon now detects out-of-band removal of the scoped `*.portzero.local`
resolver (macOS `/etc/resolver/portzero.local`, Linux dnsmasq snippet) during
its periodic diagnostics cycle, recreates it, and fires a one-shot desktop
notification. `portzero setup` also installs the resolver explicitly so name
resolution works from install time, and the Homebrew caveats document the
manual `sudo` equivalent.

Root cause of the original report: a box migrated from the old `devenv.local`
naming had `/etc/resolver/devenv.local` but no `/etc/resolver/portzero.local`,
so `*.portzero.local` subdomains never reached the (working) DNS server.
