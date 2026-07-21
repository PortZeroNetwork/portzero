#!/usr/bin/env bash
# Stops the portzero daemon and prints its log for diagnostics.
#
# On a GitHub-hosted runner the VM is destroyed after the job regardless, so
# this is mostly about (a) leaving a clean log trail on failure and (b) not
# leaking a running daemon across jobs on a self-hosted runner. Composite
# actions have no automatic post-job hook (only JavaScript/Docker actions
# support `post:`), so callers invoke this explicitly — see README.md.
set -uo pipefail

if command -v portzero >/dev/null 2>&1; then
  echo "::group::portzero status (before teardown)"
  portzero status || true
  echo "::endgroup::"

  portzero stop || true
fi

log_path="${HOME}/.portzero/daemon/daemon.log"
if [[ -f "$log_path" ]]; then
  echo "::group::daemon.log (last 200 lines)"
  tail -n 200 "$log_path" || true
  echo "::endgroup::"
fi

exit 0
