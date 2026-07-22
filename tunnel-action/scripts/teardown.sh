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

# Container-job cleanup: the container install path (cloud-agent-env-install.sh)
# runs a split-DNS forwarder and repoints /etc/resolv.conf at it. Job-scoped
# containers are destroyed anyway, but undoing both keeps re-runs on a reused
# container deterministic.
dns_pidfile="/run/pzlocal-dns.pid"
if [[ -f "$dns_pidfile" ]]; then
  dns_pid="$(cat "$dns_pidfile" 2>/dev/null || true)"
  if [[ -n "$dns_pid" ]]; then
    kill "$dns_pid" 2>/dev/null || true
  fi
  rm -f "$dns_pidfile" || true
fi

resolv_backup="/etc/resolv.conf.pz-backup"
if [[ -f "$resolv_backup" ]]; then
  # /etc/resolv.conf is often a bind mount in containers; write in place
  # rather than replacing the file.
  cat "$resolv_backup" > /etc/resolv.conf 2>/dev/null || true
  rm -f "$resolv_backup" || true
fi

exit 0
