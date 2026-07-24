#!/usr/bin/env bash
# Client interop test (macOS): installs a just-built portzero binary and
# drives a real tunnel against the real, already-running staging environment
# in portzero-cloud. Does not stand up any infrastructure itself — if
# staging isn't already up, this fails fast with a clear error instead of
# hanging or trying to provision anything.
set -euo pipefail

DOMAIN="${STAGING_DOMAIN:?Set STAGING_DOMAIN to the staging base domain}"
SEED_TOKEN="${TEST_LOGIN_SEED_TOKEN:?Set TEST_LOGIN_SEED_TOKEN to the staging test-seed-login bearer token}"
BINARY_PATH="${PORTZERO_PREBUILT_BINARY_PATH:?Set PORTZERO_PREBUILT_BINARY_PATH to the just-built portzero binary}"
EMAIL="${STAGING_CLIENT_E2E_EMAIL:-staging-client-e2e-macos@example.com}"
# Unique per platform/repo so concurrent E2E runs against shared staging
# (this job, the sibling Windows job below, and portzero-cloud's own E2E
# job) don't register the same tunnel domain. The seed-login below also
# registers this as the account's cloud username, which scopes the tunnel host.
USERNAME="${STAGING_CLIENT_E2E_USERNAME:-tunnel-e2e-macos}"
ACCOUNT_ID="${STAGING_CLIENT_E2E_ACCOUNT_ID:-staging-client-e2e-macos-account}"
VERIFY_CODE="${STAGING_CLIENT_E2E_CODE:-424242}"

API_URL="https://app.${DOMAIN}/api"
EDGE_URL="wss://edge.${DOMAIN}/tunnel"
AUTH_URL="${API_URL}/auth/verify"
SEED_URL="${API_URL}/auth/test-seed-login"
# Cloud tunnels live on the shared `.tunnel.<domain>` apex, and the host before
# it is ONE DNS label — `<name>--<cloud-username>` joined with `--`, never a dot
# — so the single `*.tunnel.<domain>` staging wildcard cert covers it (a dotted
# multi-label host has no cert and fails TLS). Scope is the seeded cloud username.
TUNNEL_DOMAIN="web--${USERNAME}.tunnel.${DOMAIN}"
EXPECTED_BODY="portzero-client-e2e-ok"

WORK_DIR="$(mktemp -d)"
HOME_DIR="${WORK_DIR}/home"
BIN_DIR="${WORK_DIR}/bin"
HTTP_DIR="${WORK_DIR}/http"
DAEMON_LOG="${HOME_DIR}/.portzero/daemon/daemon.log"

cleanup() {
  set +e
  if [ -x "${BIN_DIR}/portzero" ]; then
    HOME="${HOME_DIR}" \
      PZ_TUNNEL_API_URL="${API_URL}" \
      PZ_TUNNEL_EDGE_URL="${EDGE_URL}" \
      PZ_TUNNEL_BASE_DOMAIN="tunnel.${DOMAIN}" \
      "${BIN_DIR}/portzero" stop >/dev/null 2>&1
  fi
  if [ -n "${HTTP_PID:-}" ]; then
    # HTTP_PID is the backgrounded subshell's PID, not necessarily python3's:
    # some shells fork an extra level for the `cd; PZ_TUNNEL=... python3 ...`
    # sequence instead of exec'ing straight into python3, which would leave
    # the real server process an orphan after killing only the subshell.
    pkill -P "${HTTP_PID}" >/dev/null 2>&1
    kill "${HTTP_PID}" >/dev/null 2>&1
  fi
  if [ -f "${DAEMON_LOG}" ]; then
    echo "::group::portzero daemon log"
    tail -200 "${DAEMON_LOG}" || true
    echo "::endgroup::"
  fi
  rm -rf "${WORK_DIR}"
}
trap cleanup EXIT

mkdir -p "${HOME_DIR}" "${BIN_DIR}" "${HTTP_DIR}"

preflight_check() {
  status="$(curl -sS --connect-timeout 5 --max-time 10 -o /dev/null -w '%{http_code}' "https://app.${DOMAIN}/" || true)"
  if [ "${status}" != "200" ]; then
    echo "::error::Staging is not up (HTTP ${status:-000}) at https://app.${DOMAIN}/ — deploy staging in portzero-cloud before running the client interop test." >&2
    exit 1
  fi
  echo "Staging is up (HTTP ${status})."
}

install_client() {
  cp "${BINARY_PATH}" "${BIN_DIR}/portzero"
  chmod +x "${BIN_DIR}/portzero"
}

seed_auth_code() {
  curl -fsS \
    -H "Authorization: Bearer ${SEED_TOKEN}" \
    -H 'Content-Type: application/json' \
    -d "{\"email\":\"${EMAIL}\",\"username\":\"${USERNAME}\",\"account_id\":\"${ACCOUNT_ID}\",\"code\":\"${VERIFY_CODE}\"}" \
    "${SEED_URL}" >/dev/null
}

write_cli_auth() {
  local auth_json
  auth_json="$(curl -fsS \
    -H 'Content-Type: application/json' \
    -d "{\"email\":\"${EMAIL}\",\"code\":\"${VERIFY_CODE}\"}" \
    "${AUTH_URL}")"

  mkdir -p "${HOME_DIR}/.portzero"
  printf '%s' "${auth_json}" | jq \
    '{email: .email, token: .token, account_id: .account_id, username: .username}' \
    > "${HOME_DIR}/.portzero/auth.json"
  chmod 600 "${HOME_DIR}/.portzero/auth.json"
}

start_local_service() {
  # Serve the probe as a .txt (text/plain), NOT index.html (text/html). Cloud
  # tunnels on comments-enabled plans (professional/team/enterprise) have the
  # edge inject the feedback-comment widget `<script>` into HTML responses and
  # recompute content-length — correct product behavior, but it mutates the
  # body so an exact-match on an HTML page would never equal ${EXPECTED_BODY}.
  # A non-HTML body passes through the edge byte-identical, so this stays a
  # faithful test of the client's tunnel data path. Request /probe.txt below.
  printf '%s\n' "${EXPECTED_BODY}" > "${HTTP_DIR}/probe.txt"
  (
    cd "${HTTP_DIR}"
    # http.server's HTTPServer.server_bind() unconditionally calls
    # socket.getfqdn(host) to populate server_name — before it ever calls
    # listen() or prints the "Serving HTTP" banner. On GitHub's hosted macOS
    # runners that reverse-hostname lookup can stall for a long time (the
    # process stays alive, just stuck before the banner), which starved this
    # loop even with -u for unbuffered output. Nothing here needs a resolved
    # hostname — bound to 127.0.0.1 only — so stub it out before the server
    # ever touches DNS.
    PZ_TUNNEL="${TUNNEL_DOMAIN}:80" python3 -u -c '
import socket
socket.getfqdn = lambda *a, **kw: "localhost"
from http.server import SimpleHTTPRequestHandler, test
test(HandlerClass=SimpleHTTPRequestHandler, port=0, bind="127.0.0.1")
'
  ) > "${WORK_DIR}/http.log" 2>&1 &
  HTTP_PID="$!"

  for _ in $(seq 1 30); do
    if grep -q 'Serving HTTP' "${WORK_DIR}/http.log"; then
      return 0
    fi
    sleep 1
  done

  echo "Local HTTP server did not start" >&2
  cat "${WORK_DIR}/http.log" >&2 || true
  exit 1
}

start_portzero() {
  HOME="${HOME_DIR}" \
    PZ_TUNNEL_API_URL="${API_URL}" \
    PZ_TUNNEL_EDGE_URL="${EDGE_URL}" \
    PZ_TUNNEL_BASE_DOMAIN="tunnel.${DOMAIN}" \
    "${BIN_DIR}/portzero" start --no-browser
}

wait_for_route() {
  for i in $(seq 1 45); do
    if curl -fsS \
      -H "Authorization: Bearer $(jq -r .token "${HOME_DIR}/.portzero/auth.json")" \
      "${API_URL}/routes" | jq -e --arg domain "${TUNNEL_DOMAIN}" \
        '.[] | select(.domain == $domain)' >/dev/null; then
      echo "Route registered: ${TUNNEL_DOMAIN}"
      return 0
    fi
    echo "Waiting for route registration... (${i}/45)"
    sleep 2
  done

  echo "Route did not register: ${TUNNEL_DOMAIN}" >&2
  exit 1
}

approve_route() {
  curl -fsS \
    -X POST \
    -H "Authorization: Bearer $(jq -r .token "${HOME_DIR}/.portzero/auth.json")" \
    -H 'Content-Type: application/json' \
    -d '{}' \
    "${API_URL}/routes/${TUNNEL_DOMAIN}/approve" >/dev/null
}

curl_public_tunnel() {
  for i in $(seq 1 30); do
    body="$(curl -fsS --connect-timeout 5 --max-time 10 "https://${TUNNEL_DOMAIN}/probe.txt" || true)"
    if [ "${body}" = "${EXPECTED_BODY}" ]; then
      echo "Public tunnel returned expected response."
      return 0
    fi
    echo "Waiting for public tunnel response... (${i}/30)"
    sleep 2
  done

  echo "Public tunnel did not return expected response from https://${TUNNEL_DOMAIN}/probe.txt" >&2
  exit 1
}

preflight_check
install_client
"${BIN_DIR}/portzero" --version
seed_auth_code
write_cli_auth
HOME="${HOME_DIR}" PZ_TUNNEL_API_URL="${API_URL}" "${BIN_DIR}/portzero" whoami
start_local_service
start_portzero
wait_for_route
approve_route
curl_public_tunnel
