#!/usr/bin/env bash
# Installs the portzero daemon and waits for the requested tunnel(s).
#
# v1 scope: local (.portzero.local) tunnels only, on Linux hosted runners.
# TUN + capability setup needs a real kernel and (passwordless) sudo, which
# rules out container-based jobs (`jobs.<id>.container: ...`) — see
# ../README.md#limitations.
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "::error::tunnel-action v1 only supports Linux runners (e.g. ubuntu-latest). See tunnel-action/README.md#limitations." >&2
  exit 1
fi

if [[ -f /.dockerenv ]] || grep -qE '(docker|containerd|kubepods)' /proc/1/cgroup 2>/dev/null; then
  echo "::warning::This job appears to be running inside a container (jobs.<id>.container). tunnel-action v1 does not support container-based jobs — the daemon needs to create a TUN device on the runner's own kernel. See tunnel-action/README.md#limitations." >&2
fi

version="${PORTZERO_VERSION:-latest}"
if [[ "$version" == "latest" ]]; then
  install_url="https://github.com/PortZeroNetwork/portzero-local/releases/latest/download/linux-install.sh"
else
  install_url="https://github.com/PortZeroNetwork/portzero-local/releases/download/v${version}/linux-install.sh"
fi

echo "::group::Install portzero (${version})"
curl -fsSL --retry 3 --retry-delay 2 "$install_url" | sh
echo "::endgroup::"

# The installer already starts the daemon (systemd --user unit if available,
# otherwise a direct background start via `portzero start`) and installs the
# local CA + Linux capabilities using the passwordless sudo hosted runners
# provide. Nothing further to start here.
hash -r
if ! command -v portzero >/dev/null 2>&1; then
  # The installer picks /usr/local/bin when writable, else ~/.local/bin.
  # Make sure later steps in the job see whichever it picked.
  for candidate in /usr/local/bin/portzero "$HOME/.local/bin/portzero"; do
    if [[ -x "$candidate" ]]; then
      dirname "$candidate" >> "$GITHUB_PATH"
      export PATH="$(dirname "$candidate"):$PATH"
      break
    fi
  done
fi

echo "::group::portzero status"
portzero --version
portzero status || true
echo "::endgroup::"

mapfile -t tunnels < <(printf '%s\n' "${PORTZERO_TUNNELS:-}" | sed '/^[[:space:]]*$/d')
if [[ "${#tunnels[@]}" -eq 0 ]]; then
  echo "::error::tunnel-action: 'tunnels' input is empty. Provide one PZ_TUNNEL domain per line — the domain your own process/container already declared via PZ_TUNNEL (this action does not start it for you)." >&2
  exit 1
fi

healthy_flag=()
if [[ "${PORTZERO_HEALTHY:-true}" == "true" ]]; then
  healthy_flag=(--healthy)
fi

urls=()
for domain in "${tunnels[@]}"; do
  echo "::group::portzero wait ${domain}"
  if ! portzero wait "${domain}" "${healthy_flag[@]}" --timeout "${PORTZERO_TIMEOUT:-60}"; then
    echo "::endgroup::"
    echo "::error::tunnel-action: '${domain}' did not become ready within ${PORTZERO_TIMEOUT:-60}s. Confirm a process/container with PZ_TUNNEL=${domain} is running before this step (or before a later step that this one is not waiting past)." >&2
    exit 1
  fi
  url="$(portzero url "${domain}")"
  echo "Resolved: ${url}"
  urls+=("${url}")
  echo "::endgroup::"
done

{
  echo "urls<<PORTZERO_TUNNEL_ACTION_EOF"
  printf '%s\n' "${urls[@]}"
  echo "PORTZERO_TUNNEL_ACTION_EOF"
} >> "$GITHUB_OUTPUT"

if [[ "${#urls[@]}" -eq 1 ]]; then
  echo "url=${urls[0]}" >> "$GITHUB_OUTPUT"
fi
