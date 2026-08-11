#!/usr/bin/env bash
# Bring up everything the end-to-end suite needs, run it, tear it down.
#
# Keycloak is *not* started here: it is a service container (GitHub Actions
# `services:`, or `docker run` locally — see the README). Everything else is a
# plain process, which keeps the moving parts to a minimum and means the suite
# behaves the same on a laptop and on a runner.
set -euo pipefail

KEYCLOAK_URL="${KEYCLOAK_URL:-http://localhost:8080}"
KEYCLOAK_REALM="${KEYCLOAK_REALM:-oas2mcp}"
KEYCLOAK_ADMIN="${KEYCLOAK_ADMIN:-admin}"
KEYCLOAK_ADMIN_PASSWORD="${KEYCLOAK_ADMIN_PASSWORD:-admin}"
API_URL="${API_URL:-http://127.0.0.1:8000}"
MCP_PORT="${MCP_PORT:-8765}"
OAS2MCP="${OAS2MCP:-./target/debug/oas2mcp}"
PYTHON="${PYTHON:-python3}"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
cd "$root"

logs="$(mktemp -d)"
pids=()

cleanup() {
  local status=$?
  for pid in "${pids[@]:-}"; do
    [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
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

wait_for() {
  local name=$1 url=$2 attempts=${3:-60}
  for _ in $(seq 1 "$attempts"); do
    if curl -sf -o /dev/null "$url"; then
      echo "  $name is up"
      return 0
    fi
    sleep 1
  done
  echo "  $name never came up at $url" >&2
  return 1
}

echo "== waiting for Keycloak"
wait_for Keycloak "$KEYCLOAK_URL/realms/master/.well-known/openid-configuration" 120

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
  --data-binary "@$here/keycloak/realm-oas2mcp.json")
if [ "$status" != "201" ]; then
  echo "  realm import failed with HTTP $status" >&2
  cat "$logs/import.log" >&2
  exit 1
fi
echo "  realm $KEYCLOAK_REALM imported"

echo "== writing the JWKS oas2mcp verifies caller tokens with"
# The external provider's public half. oas2mcp reads it from disk, so the suite
# needs no HTTP server standing in for the provider.
"$PYTHON" "$here/jwks.py" tests/fixtures/test_rsa_key.pem oneaccess-key > "$logs/oneaccess-jwks.json"

echo "== starting the sandbox API"
OIDC_ISSUER="$KEYCLOAK_URL/realms/$KEYCLOAK_REALM" \
OIDC_AUDIENCE=sandbox-api \
PUBLIC_URL="$API_URL" \
  "$PYTHON" -m uvicorn main:app --app-dir "$here/api" --host 127.0.0.1 --port 8000 \
  >"$logs/api.log" 2>&1 &
pids+=("$!")
wait_for "the sandbox API" "$API_URL/healthz"

echo "== starting oas2mcp"
# The document fetch authenticates with a signed client assertion (§2.2); tool
# calls obtain a per-caller token with the jwt-bearer grant (§2.1), relaying the
# caller's own assertion.
"$OAS2MCP" \
  --openapi-url "$API_URL/openapi.json" \
  --openapi-oauth-token-url "$KEYCLOAK_URL/realms/$KEYCLOAK_REALM/protocol/openid-connect/token" \
  --openapi-oauth-client-id oas2mcp-doc \
  --openapi-oauth-private-key tests/fixtures/test_rsa_key.pem \
  --openapi-oauth-signing-alg rs256 \
  --transport streamable-http --bind-addr "127.0.0.1:$MCP_PORT" \
  --oauth-jwks-file "$logs/oneaccess-jwks.json" \
  --oauth-role-mapper 'admin:.*' \
  --oauth-role-mapper 'reader:^(getPets|whoami)$' \
  --oauth-expected-issuer https://oneaccess.example/ \
  --trace-claim sub \
  --upstream-oauth-token-url "$KEYCLOAK_URL/realms/$KEYCLOAK_REALM/protocol/openid-connect/token" \
  --upstream-oauth-client-id oas2mcp-upstream \
  --upstream-oauth-client-secret upstream-secret \
  --upstream-oauth-grant jwt-bearer \
  --upstream-oauth-assertion caller \
  --log-filter info \
  >"$logs/oas2mcp.log" 2>&1 &
pids+=("$!")
wait_for oas2mcp "http://127.0.0.1:$MCP_PORT/mcp" 60 || true
# `/mcp` answers 405 to GET, which curl -f treats as a failure; a listening port
# is what matters, so give it a moment and let the suite report the truth.
sleep 2

echo "== running the suite"
KEYCLOAK_URL="$KEYCLOAK_URL" \
KEYCLOAK_REALM="$KEYCLOAK_REALM" \
MCP_URL="http://127.0.0.1:$MCP_PORT/mcp" \
  "$PYTHON" "$here/run.py"
