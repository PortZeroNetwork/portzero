---
id: 3f87f639-6bd2-48a2-84c0-ffbfa134f7b6
slug: task-82
status: todo
title: Linux release binaries require glibc 2.38+ (fail on ubuntu-22.04)
created_at: 2026-07-22T10:35:30.649424299Z
updated_at: 2026-07-22T10:35:30.649424299Z
---

Found by portzero-full-example CI matrix: the v0.0.3 linux-amd64 binary fails on ubuntu-22.04 with "GLIBC_2.38/2.39 not found" because releases build on ubuntu-latest (24.04).

Fix (staged): release.yml now builds x86_64-unknown-linux-gnu on ubuntu-22.04 (glibc 2.35), covering all supported Ubuntu LTS. Takes effect on the next release cut — until then, 22.04 CI legs stay expected-fail.

Longer-term option if older distros matter: musl or zig-cc builds.