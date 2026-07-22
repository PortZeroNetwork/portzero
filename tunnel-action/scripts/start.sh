#!/usr/bin/env bash
# Installs the portzero daemon and waits for the requested tunnel(s).
#
# Supported hosts: Linux hosted runners (e.g. ubuntu-latest) and Linux
# container-based jobs (`jobs.<id>.container: ...`) that expose /dev/net/tun
# and run as root — see ../README.md#container-jobs. Cloud tunnels are
# authenticated per-job via the GitHub OIDC exchange when `oidc-team` is set —
# see ../README.md#cloud-tunnels-via-github-oidc.
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "::error::tunnel-action only supports Linux runners (e.g. ubuntu-latest) or Linux container jobs. See tunnel-action/README.md#limitations." >&2
  exit 1
fi

in_container=false
if [[ -f /.dockerenv ]] || grep -qE '(docker|containerd|kubepods)' /proc/1/cgroup 2>/dev/null; then
  in_container=true
fi

version="${PORTZERO_VERSION:-latest}"

# Extract a top-level string field from a small JSON document on stdin.
# Good enough for tokens/JWTs (base64url + dots, never embedded quotes).
extract_json_string() {
  sed -n 's/.*"'"$1"'"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1
}

# ---------------------------------------------------------------------------
# Container-job prerequisites: the daemon needs a TUN device and root (or the
# NET_ADMIN/NET_BIND_SERVICE caps), and minimal container images often lack
# curl/python3/ca-certificates that the install path below depends on.
# ---------------------------------------------------------------------------
if [[ "$in_container" == true ]]; then
  missing_privs=""
  [[ -e /dev/net/tun ]] || missing_privs="/dev/net/tun "
  [[ "$(id -u)" -eq 0 ]] || missing_privs="${missing_privs}root-uid"
  if [[ -n "$missing_privs" ]]; then
    echo "::error::tunnel-action: this job runs in a container without the privileges the daemon needs (missing: ${missing_privs}). Fix your job definition: container: { options: --device /dev/net/tun --cap-add NET_ADMIN --cap-add NET_BIND_SERVICE } and keep the container's default root user (do not pass --user in container.options). See tunnel-action/README.md#container-jobs." >&2
    exit 1
  fi

  pkgs_missing=()
  command -v curl >/dev/null 2>&1 || pkgs_missing+=(curl)
  command -v python3 >/dev/null 2>&1 || pkgs_missing+=(python3)
  [[ -f /etc/ssl/certs/ca-certificates.crt ]] || pkgs_missing+=(ca-certificates)
  if [[ "${#pkgs_missing[@]}" -gt 0 ]]; then
    if command -v apt-get >/dev/null 2>&1; then
      echo "::group::Install container prerequisites (${pkgs_missing[*]})"
      export DEBIAN_FRONTEND=noninteractive
      apt-get update -y
      apt-get install -y --no-install-recommends "${pkgs_missing[@]}"
      echo "::endgroup::"
    else
      echo "::error::tunnel-action: this container image is missing ${pkgs_missing[*]} and has no apt-get to install them. Use a Debian/Ubuntu-based image, or bake curl, python3, and ca-certificates into your image. See tunnel-action/README.md#container-jobs." >&2
      exit 1
    fi
  fi
fi

# ---------------------------------------------------------------------------
# Cloud-tunnel auth via GitHub OIDC (before the daemon first starts, so it
# picks up auth.json on boot and no restart is needed in the common case).
# ---------------------------------------------------------------------------
oidc_done=false
if [[ -n "${PORTZERO_OIDC_TEAM:-}" ]]; then
  echo "::group::Cloud-tunnel auth via GitHub OIDC (team: ${PORTZERO_OIDC_TEAM})"
  if [[ -z "${ACTIONS_ID_TOKEN_REQUEST_URL:-}" || -z "${ACTIONS_ID_TOKEN_REQUEST_TOKEN:-}" ]]; then
    echo "::endgroup::"
    echo "::error::tunnel-action: 'oidc-team' is set but this job cannot request a GitHub OIDC token (ACTIONS_ID_TOKEN_REQUEST_URL is unset). Add permissions: { id-token: write } to the job (or workflow) — without it GitHub does not hand the runner an OIDC endpoint. See tunnel-action/README.md#cloud-tunnels-via-github-oidc." >&2
    exit 1
  fi

  oidc_jwt="$(curl -fsSL --retry 3 --retry-delay 2 \
    -H "Authorization: Bearer ${ACTIONS_ID_TOKEN_REQUEST_TOKEN}" \
    "${ACTIONS_ID_TOKEN_REQUEST_URL}&audience=portzero.cloud" \
    | extract_json_string value)"
  if [[ -z "$oidc_jwt" ]]; then
    echo "::endgroup::"
    echo "::error::tunnel-action: failed to obtain a GitHub OIDC token from the runner's identity endpoint. Re-run the job; if it persists, confirm permissions: { id-token: write } is set and no enterprise policy blocks OIDC token requests." >&2
    exit 1
  fi
  echo "::add-mask::${oidc_jwt}"

  api_url="${PORTZERO_API_URL:-https://app.portzero.cloud/api}"
  exchange_url="${api_url%/}/auth/github-oidc/exchange"
  req_body="$(mktemp)"
  resp_body="$(mktemp)"
  printf '{"token":"%s","team":"%s","ttl_secs":3600}' \
    "$oidc_jwt" "$PORTZERO_OIDC_TEAM" > "$req_body"
  http_status="$(curl -sS --retry 3 --retry-delay 2 -o "$resp_body" -w '%{http_code}' \
    -X POST -H 'Content-Type: application/json' \
    --data "@${req_body}" "$exchange_url")" || http_status="000"
  rm -f "$req_body"
  if [[ "$http_status" != 2* ]]; then
    echo "Exchange response (HTTP ${http_status}):"
    cat "$resp_body" || true
    echo
    rm -f "$resp_body"
    echo "::endgroup::"
    echo "::error::tunnel-action: the OIDC credential exchange at ${exchange_url} failed (HTTP ${http_status}). Most commonly the team '${PORTZERO_OIDC_TEAM}' has no OIDC trust rule allowing this repository (${GITHUB_REPOSITORY:-unknown}) — add one in the team's portzero.cloud settings. Also check the team slug and 'api-url'. Docs: https://portzero.net/docs." >&2
    exit 1
  fi

  minted="$(extract_json_string token < "$resp_body")"
  rm -f "$resp_body"
  if [[ -z "$minted" ]]; then
    echo "::endgroup::"
    echo "::error::tunnel-action: the OIDC exchange returned success but no 'token' field was found in the response. This looks like an api-url pointing at something that is not the portzero.cloud API — check the 'api-url' input (default: https://app.portzero.cloud/api)." >&2
    exit 1
  fi
  echo "::add-mask::${minted}"

  mkdir -p "${HOME}/.portzero"
  auth_file="${HOME}/.portzero/auth.json"
  printf '{"email": "github-ci@ci.portzero.cloud", "token": "%s", "account_id": "github-oidc-ci", "username": ""}\n' \
    "$minted" > "$auth_file"
  chmod 600 "$auth_file"
  echo "Minted a short-lived cloud-tunnel credential for team '${PORTZERO_OIDC_TEAM}' (ttl 3600s) into ${auth_file}."
  oidc_done=true
  echo "::endgroup::"
fi

# ---------------------------------------------------------------------------
# Install the daemon.
# ---------------------------------------------------------------------------
if [[ "$in_container" == true ]]; then
  # Container path: the headless-container installer (binary + setcap +
  # split-DNS forwarder + daemon start). It installs to /usr/local/bin.
  echo "::group::Install portzero (container job, ${version})"
  if [[ "$version" != "latest" ]]; then
    export PORT_ZERO_VERSION="v${version}"
  fi
  # For subdirectory composite actions the whole repo tarball is materialized,
  # so the repo-root sibling script is normally present next to this action.
  installer="${GITHUB_ACTION_PATH:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}/../scripts/cloud-agent-env-install.sh"
  if [[ -f "$installer" ]]; then
    bash "$installer"
  else
    curl -fsSL --retry 3 --retry-delay 2 \
      "https://raw.githubusercontent.com/PortZeroNetwork/portzero/${GITHUB_ACTION_REF:-staging}/scripts/cloud-agent-env-install.sh" \
      | bash
  fi
  echo "::endgroup::"
  echo "/usr/local/bin" >> "$GITHUB_PATH"
  export PATH="/usr/local/bin:$PATH"
else
  # VM path: the public release installer (also installs the local CA and
  # Linux capabilities via the passwordless sudo hosted runners provide, and
  # starts the daemon).
  if [[ "$version" == "latest" ]]; then
    install_url="https://github.com/PortZeroNetwork/portzero/releases/latest/download/linux-install.sh"
  else
    install_url="https://github.com/PortZeroNetwork/portzero/releases/download/v${version}/linux-install.sh"
  fi

  echo "::group::Install portzero (${version})"
  curl -fsSL --retry 3 --retry-delay 2 "$install_url" | sh
  echo "::endgroup::"
fi

hash -r
if ! command -v portzero >/dev/null 2>&1; then
  # The VM installer picks /usr/local/bin when writable, else ~/.local/bin.
  # Make sure later steps in the job see whichever it picked.
  for candidate in /usr/local/bin/portzero "$HOME/.local/bin/portzero"; do
    if [[ -x "$candidate" ]]; then
      dirname "$candidate" >> "$GITHUB_PATH"
      PATH="$(dirname "$candidate"):$PATH"
      export PATH
      break
    fi
  done
fi

# The install paths above write auth.json before the daemon first starts, so
# it normally boots already authenticated. On a reused runner/container the
# daemon may have been running before auth.json was written — restart so it
# picks the new credential up.
if [[ "$oidc_done" == true ]] && portzero status 2>/dev/null | grep -q 'running'; then
  echo "Restarting the daemon so it picks up the freshly minted cloud credential."
  portzero restart || true
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
