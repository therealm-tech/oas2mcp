#!/usr/bin/env bash
# Bring up everything the end-to-end suite needs, run it, tear it down.
#
# Keycloak is *not* started here: it is a service container (GitHub Actions
# `services:`, or `docker run` locally — see the README). Everything else is a
# plain process, which keeps the moving parts to a minimum and means the suite
# behaves the same on a laptop and on a runner.
#
# oas2mcp is the one exception: set OAS2MCP_IMAGE to run that container image
# instead of the local binary.
set -euo pipefail

KEYCLOAK_URL="${KEYCLOAK_URL:-http://localhost:8080}"
KEYCLOAK_REALM="${KEYCLOAK_REALM:-oas2mcp}"
KEYCLOAK_ADMIN="${KEYCLOAK_ADMIN:-admin}"
KEYCLOAK_ADMIN_PASSWORD="${KEYCLOAK_ADMIN_PASSWORD:-admin}"
API_PORT="${API_PORT:-8443}"
MCP_PORT="${MCP_PORT:-8765}"
OAS2MCP="${OAS2MCP:-./target/debug/oas2mcp}"
OAS2MCP_IMAGE="${OAS2MCP_IMAGE:-}"
PYTHON="${PYTHON:-python3}"
# renovate: datasource=docker depName=alpine/socat
RELAY_IMAGE="alpine/socat:1.8.1.1@sha256:7f9a06753033f2b7de18edc2353f2c15153413d95a039163c6db270fc7a6c3b0"

# The port the image listens on, from its own `BIND_ADDR`.
IMAGE_PORT=8000
run_id="oas2mcp-e2e-$$"
relay="$run_id-relay"
server="$run_id-server"

if [ -n "$OAS2MCP_IMAGE" ]; then
  # A name only Docker's DNS server knows, so the image has to resolve it.
  api_host=sandbox-api.e2e.test
  api_bind=0.0.0.0
else
  api_host=localhost
  api_bind=127.0.0.1
fi
API_URL="https://$api_host:$API_PORT"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
cd "$root"

logs="$(mktemp -d)"
pids=()

cleanup() {
  local status=$?
  if [ -n "$OAS2MCP_IMAGE" ]; then
    docker logs "$server" >"$logs/oas2mcp.log" 2>&1 || true
    docker logs "$relay" >"$logs/relay.log" 2>&1 || true
    docker rm --force "$server" "$relay" >/dev/null 2>&1 || true
    docker network rm "$run_id" >/dev/null 2>&1 || true
  fi
  for pid in "${pids[@]:-}"; do
    if [ -n "$pid" ]; then
      kill "$pid" 2>/dev/null || true
    fi
  done
  if [ "$status" -ne 0 ]; then
    for log in "$logs"/*.log; do
      [ -e "$log" ] || continue
      echo "::group::$(basename "$log")"
      cat "$log"
      echo "::endgroup::"
    done
  fi
  rm -rf "$logs"
}
trap cleanup EXIT

# wait_for <name> <attempts> <command...>
wait_for() {
  local name=$1 attempts=$2
  shift 2
  for _ in $(seq 1 "$attempts"); do
    if "$@" >/dev/null 2>&1; then
      echo "  $name is up"
      return 0
    fi
    sleep 1
  done
  echo "  $name never came up" >&2
  return 1
}

container_is_healthy() {
  [ "$(docker inspect --format '{{.State.Health.Status}}' "$server")" = healthy ]
}

# Run oas2mcp from the image. It shares the network namespace of a relay on a
# bridge network: `localhost` there forwards to the host's Keycloak, which keeps
# the issuer every token names, and the API's name is the relay's DNS alias.
start_image() {
  local keycloak_port=${KEYCLOAK_URL##*:}
  docker network create "$run_id" >/dev/null
  docker run --detach --name "$relay" --network "$run_id" \
    --network-alias "$api_host" \
    --add-host host.docker.internal:host-gateway \
    --publish "127.0.0.1:$MCP_PORT:$IMAGE_PORT" \
    --entrypoint sh "$RELAY_IMAGE" -c "
      socat TCP-LISTEN:$keycloak_port,fork,reuseaddr TCP:host.docker.internal:$keycloak_port &
      exec socat TCP-LISTEN:$API_PORT,fork,reuseaddr TCP:host.docker.internal:$API_PORT" \
    >/dev/null
  # The image's own HEALTHCHECK command, only probed more often.
  docker run --detach --name "$server" --network "container:$relay" \
    --health-interval 1s \
    --volume "$mount:/etc/oas2mcp:ro" \
    "$OAS2MCP_IMAGE" "$@" >/dev/null
  wait_for oas2mcp 60 container_is_healthy

  # The real, effective, saved and filesystem UIDs of the server process, read
  # from beside it: the image has nothing to ask with.
  local uids
  uids=$(docker run --rm --pid "container:$server" --entrypoint awk "$RELAY_IMAGE" \
    '/^Uid:/ { print $2, $3, $4, $5 }' /proc/1/status)
  if [ "$uids" != "65532 65532 65532 65532" ]; then
    echo "  oas2mcp runs as UIDs '$uids' instead of 65532" >&2
    return 1
  fi
  echo "  oas2mcp runs as UID 65532"
}

echo "== waiting for Keycloak"
wait_for Keycloak 120 curl -sf "$KEYCLOAK_URL/realms/master/.well-known/openid-configuration"

echo "== generating the keys"
# Fresh keys per run, never committed: the signing key's public half goes into the
# realm and the JWKS, the TLS ones serve the sandbox API under a throwaway CA.
# Everything lives in $logs, which the cleanup removes.
"$PYTHON" "$here/keys.py" "$here/keycloak/realm-oas2mcp.json" "$logs" oneaccess-key \
  "$api_host" localhost

echo "== importing the realm"
# Through the admin API rather than a mounted file: a service container starts
# before the repository is checked out, so there is nothing to mount at that
# point. This also keeps the realm in the repo as the single source of truth.
admin_token=$(curl -sf -X POST \
  "$KEYCLOAK_URL/realms/master/protocol/openid-connect/token" \
  -d grant_type=password -d client_id=admin-cli \
  -d "username=$KEYCLOAK_ADMIN" -d "password=$KEYCLOAK_ADMIN_PASSWORD" |
  "$PYTHON" -c 'import json,sys; print(json.load(sys.stdin)["access_token"])')

# Idempotent: a rerun against a live Keycloak replaces the realm.
curl -s -o /dev/null -X DELETE "$KEYCLOAK_URL/admin/realms/$KEYCLOAK_REALM" \
  -H "authorization: Bearer $admin_token"
status=$(curl -s -o "$logs/import.log" -w '%{http_code}' -X POST "$KEYCLOAK_URL/admin/realms" \
  -H "authorization: Bearer $admin_token" -H 'content-type: application/json' \
  --data-binary "@$logs/realm.json")
if [ "$status" != "201" ]; then
  echo "  realm import failed with HTTP $status" >&2
  cat "$logs/import.log" >&2
  exit 1
fi
echo "  realm $KEYCLOAK_REALM imported"

echo "== starting the sandbox API"
OIDC_ISSUER="$KEYCLOAK_URL/realms/$KEYCLOAK_REALM" \
OIDC_AUDIENCE=sandbox-api \
PUBLIC_URL="$API_URL" \
  "$PYTHON" -m uvicorn main:app --app-dir "$here/api" --host "$api_bind" --port "$API_PORT" \
  --ssl-certfile "$logs/api-cert.pem" --ssl-keyfile "$logs/api-key.pem" \
  >"$logs/api.log" 2>&1 &
pids+=("$!")
wait_for "the sandbox API" 60 curl -sf --cacert "$logs/ca.pem" "https://localhost:$API_PORT/healthz"

echo "== starting oas2mcp"
# What oas2mcp reads from disk. The image runs as UID 65532, which can read
# neither a private directory nor the 0600 key: it gets readable copies.
mount="$logs/mount"
mkdir "$mount"
cp "$logs/ca.pem" "$logs/jwks.json" "$logs/signing-key.pem" "$mount/"
chmod 755 "$mount"
chmod 644 "$mount"/*

if [ -n "$OAS2MCP_IMAGE" ]; then
  files=/etc/oas2mcp
else
  files="$mount"
fi

# The document fetch authenticates with a signed client assertion (§2.2); tool
# calls obtain a per-caller token with the jwt-bearer grant (§2.1), relaying the
# caller's own assertion.
args=(
  http
  --openapi-url "$API_URL/openapi.json"
  --ca-cert "$files/ca.pem"
  --openapi-oauth-token-url "$KEYCLOAK_URL/realms/$KEYCLOAK_REALM/protocol/openid-connect/token"
  --openapi-oauth-client-id oas2mcp-doc
  --openapi-oauth-private-key "$files/signing-key.pem"
  --openapi-oauth-signing-alg rs256
  --inbound-jwks-file "$files/jwks.json"
  --inbound-role-mapper 'admin:.*'
  --inbound-role-mapper 'reader:^(getPets|whoami)$'
  --inbound-expected-issuer https://oneaccess.example/
  --inbound-resource "http://127.0.0.1:$MCP_PORT/mcp"
  --inbound-role-mapper '*:^whoami$'
  --trace-claim sub
  --upstream-oauth-token-url "$KEYCLOAK_URL/realms/$KEYCLOAK_REALM/protocol/openid-connect/token"
  --upstream-oauth-client-id oas2mcp-upstream
  --upstream-oauth-client-secret upstream-secret
  --upstream-oauth-grant jwt-bearer-relay
  --log-filter info
)

if [ -n "$OAS2MCP_IMAGE" ]; then
  # The image binds every interface, which turns Host validation off unless the
  # allowed hosts are named.
  start_image "${args[@]}" --allowed-host "127.0.0.1:$MCP_PORT"
else
  "$OAS2MCP" "${args[@]}" --bind-addr "127.0.0.1:$MCP_PORT" >"$logs/oas2mcp.log" 2>&1 &
  pids+=("$!")
  wait_for oas2mcp 60 "$OAS2MCP" healthcheck --bind-addr "127.0.0.1:$MCP_PORT"
fi

echo "== running the suite"
KEYCLOAK_URL="$KEYCLOAK_URL" \
KEYCLOAK_REALM="$KEYCLOAK_REALM" \
MCP_URL="http://127.0.0.1:$MCP_PORT/mcp" \
SIGNING_KEY="$logs/signing-key.pem" \
  "$PYTHON" "$here/run.py"
