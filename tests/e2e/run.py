#!/usr/bin/env python3
"""End-to-end suite: oas2mcp against a real Keycloak and a real API.

What the unit tests cannot cover is whether a real authorization server accepts
what we send. This drives the whole chain from the diagram — an assertion from an
external provider, relayed to Keycloak as an RFC 7523 §2.1 grant, exchanged for an
internal token, spent against an API that attributes the result to the end user.

The external provider ("OneAccess" in production) is not a service here: Keycloak
is configured with its public key directly, so nothing has to be fetched, and this
script mints assertions with the matching private key. That leaves Keycloak as the
only container, which is what lets the whole thing run as a single GitHub Actions
service.

Usage: run.py  (expects Keycloak, the API and oas2mcp already up — see harness.py)
"""

import json
import os
import sys
import time
import urllib.error
import urllib.request

import jwt

KEYCLOAK = os.environ.get("KEYCLOAK_URL", "http://localhost:8080")
REALM = os.environ.get("KEYCLOAK_REALM", "oas2mcp")
MCP = os.environ.get("MCP_URL", "http://127.0.0.1:8765/mcp")
SIGNING_KEY = os.environ.get("SIGNING_KEY", "tests/fixtures/test_rsa_key.pem")
# Must match the `issuer` of the realm's identity provider.
ONEACCESS_ISSUER = os.environ.get("ONEACCESS_ISSUER", "https://oneaccess.example/")
# RFC 7523 §3: the assertion names the authorization server that consumes it.
# Keycloak accepts its realm issuer or its token endpoint, and *only one* audience.
ASSERTION_AUDIENCE = f"{KEYCLOAK}/realms/{REALM}"

_failures: list[str] = []
_passed = 0


def check(name: str, condition: bool, detail: str = "") -> bool:
    """Record one assertion, keep going either way."""
    global _passed
    if condition:
        _passed += 1
        print(f"  ok   {name}")
    else:
        _failures.append(f"{name}{f' — {detail}' if detail else ''}")
        print(f"  FAIL {name}{f' — {detail}' if detail else ''}")
    return condition


def assertion(subject: str, *, roles=("admin",), audience=None, lifetime=300, jti=None) -> str:
    """Mint what OneAccess would hand the caller."""
    with open(SIGNING_KEY) as handle:
        key = handle.read()
    now = int(time.time())
    claims = {
        "iss": ONEACCESS_ISSUER,
        "sub": subject,
        "aud": audience or ASSERTION_AUDIENCE,
        "iat": now,
        "exp": now + lifetime,
        # Keycloak refuses a replayed assertion, so every mint is unique.
        "jti": jti or f"e2e-{subject}-{now}-{os.urandom(4).hex()}",
        "roles": list(roles),
    }
    return jwt.encode(claims, key, algorithm="RS256", headers={"kid": "oneaccess-key"})


def mcp(method: str, params: dict | None = None, token: str | None = None) -> dict:
    """One JSON-RPC call to the MCP endpoint."""
    body = json.dumps(
        {"jsonrpc": "2.0", "id": 1, "method": method, "params": params or {}}
    ).encode()
    request = urllib.request.Request(MCP, data=body, method="POST")
    request.add_header("content-type", "application/json")
    request.add_header("accept", "application/json, text/event-stream")
    if token:
        request.add_header("authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.loads(response.read())
    except urllib.error.HTTPError as err:
        return {"error": {"message": f"HTTP {err.code}: {err.read().decode()[:200]}"}}


def call_tool(name: str, token: str, arguments: dict | None = None) -> dict:
    """Call a tool and return the parsed result plus its text payload."""
    reply = mcp("tools/call", {"name": name, "arguments": arguments or {}}, token)
    result = reply.get("result")
    if result is None:
        return {"rpc_error": reply.get("error", {}).get("message", "no result")}
    text = "".join(block.get("text", "") for block in result.get("content", []))
    payload = None
    # The tool result is "HTTP <status>\n\n<body>"; pull the body out when JSON.
    if "\n\n" in text:
        try:
            payload = json.loads(text.split("\n\n", 1)[1])
        except json.JSONDecodeError:
            payload = None
    return {"isError": bool(result.get("is_error") or result.get("isError")), "text": text, "payload": payload}


def tool_names(token: str) -> list[str]:
    reply = mcp("tools/list", {}, token)
    return sorted(t["name"] for t in reply.get("result", {}).get("tools", []))


def main() -> int:
    print("\n=== the document fetch authenticated with a signed assertion (§2.2) ===")
    # oas2mcp could only build its tool set by fetching a protected document, and
    # it could only fetch it with a token obtained via `private_key_jwt`. So a
    # non-empty tool set *is* the assertion that §2.2 worked against Keycloak.
    alice = assertion("oneaccess-alice")
    names = tool_names(alice)
    check("the protected OpenAPI document was fetched", names != [], f"tools: {names}")
    check("every operation became a tool", names == ["createPet", "getPets", "whoami"], f"tools: {names}")

    print("\n=== delegation: the API sees the end user, not the service account ===")
    result = call_tool("getPets", alice)
    check("alice's call succeeds", not result.get("isError") and result.get("payload"), result.get("text", result.get("rpc_error", ""))[:200])
    if result.get("payload"):
        check(
            "the API attributed the call to alice",
            result["payload"].get("ownerId") == "alice",
            f"ownerId={result['payload'].get('ownerId')}",
        )

    bob = assertion("oneaccess-bob", roles=("reader",))
    result = call_tool("getPets", bob)
    if check("bob's call succeeds", not result.get("isError") and result.get("payload")):
        check(
            "the API attributed the call to bob",
            result["payload"].get("ownerId") == "bob",
            f"ownerId={result['payload'].get('ownerId')}",
        )

    print("\n=== the per-caller token cache, proven by Keycloak's replay protection ===")
    # Keycloak refuses a replayed assertion. Relaying the *same* caller token twice
    # therefore only works if oas2mcp cached the upstream token and did not go back
    # to the token endpoint — which is exactly what this asserts. A regression in
    # the cache shows up here as an auth failure, not as a slowdown.
    again = call_tool("getPets", alice)
    check(
        "a second call with the same assertion is served from cache",
        not again.get("isError"),
        again.get("text", "")[:200],
    )

    print("\n=== role-based tool access, from the relayed assertion's claims ===")
    check("alice (admin) sees every tool", tool_names(alice) == ["createPet", "getPets", "whoami"])
    check("bob (reader) sees only the read tools", tool_names(bob) == ["getPets", "whoami"], f"{tool_names(bob)}")
    denied = call_tool("createPet", bob, {"body": {"name": "Sneaky"}})
    check("bob cannot call the write tool", "rpc_error" in denied, str(denied)[:160])

    print("\n=== an external identity nobody linked is refused ===")
    # carol exists in Keycloak but is not linked to the identity provider, so the
    # grant has no local identity to issue a token for.
    carol = assertion("oneaccess-carol", roles=("reader",))
    result = call_tool("getPets", carol)
    check(
        "an unlinked subject gets no upstream token",
        result.get("isError") and "upstream OAuth token" in result.get("text", ""),
        result.get("text", result.get("rpc_error", ""))[:200],
    )

    print("\n=== an assertion addressed elsewhere is refused ===")
    # Keycloak validates the assertion's `aud` against itself. This is the check
    # that stops an assertion minted for another server being replayed here.
    #
    # A subject of its own, deliberately: alice already has a cached upstream
    # token, so a bad assertion bearing her name would be served from cache and
    # never reach Keycloak at all. The cache is keyed on the caller identity, not
    # on the assertion, which is the right call — but it means this check only
    # means anything on a cold entry.
    wrong = assertion("oneaccess-dave", roles=("reader",), audience="https://somewhere-else.example/")
    result = call_tool("getPets", wrong)
    check(
        "an assertion for another audience is rejected",
        result.get("isError") or "rpc_error" in result,
        str(result)[:200],
    )

    print("\n=== no caller identity at all ===")
    result = call_tool("getPets", "")
    check("an unauthenticated call is refused", "rpc_error" in result or result.get("isError"), str(result)[:160])

    print(f"\n{'=' * 62}")
    if _failures:
        print(f"{_passed} passed, {len(_failures)} FAILED")
        for failure in _failures:
            print(f"  - {failure}")
        return 1
    print(f"{_passed} passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
