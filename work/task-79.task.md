---
id: 8350f04a-899e-4f11-9042-8ab345e5c788
slug: task-79
status: todo
title: 'installer: set -e silently swallows prerelease-resolution errors'
created_at: 2026-07-18T13:33:33.426573273Z
updated_at: 2026-07-18T13:33:33.426573273Z
---

In the release linux-install.sh, resolve_prerelease_tag's exit code is checked via 'case $?' after 'pre_tag="$(resolve_prerelease_tag)"' — but the script runs under set -e, so a non-zero return (e.g. GitHub API rate-limit on shared CI runner IPs) aborts the script before the case runs, with no error message. Symptom: installer prints 'Platform: linux-amd64' then exits 1 silently (seen in portzero-cloud Deploy Staging client-interop E2E, run 29645837550, 2026-07-18). Fix: call it as 'if pre_tag="$(resolve_prerelease_tag)"; then …' or guard with set +e so the intended error messages ('Could not reach the GitHub Releases API…') actually print. Stable-channel installs are unaffected (they never call this function). Consider also having CI pass an authenticated token or a pinned PORTZERO_VERSION to avoid unauthenticated API rate limits entirely.