# End-to-end suite

Drives oas2mcp against a **real Keycloak** and a real API, to answer the one
question unit tests cannot: does an actual authorization server accept what we
send?

It exercises the whole RFC 7523 chain:

1. an external provider mints a signed assertion for an end user;
2. the caller presents it to oas2mcp as its bearer token;
3. oas2mcp relays it to Keycloak as a `jwt-bearer` authorization grant (§2.1);
4. Keycloak validates it, resolves the federated identity, and issues an
   **internal** token for the local user;
5. oas2mcp spends that token on the API, which attributes the result to the end
   user rather than to a shared service account.

The document fetch takes a separate path, authenticating with a JWT client
assertion (`private_key_jwt`, §2.2) — so both halves of the RFC are covered
against a real implementation.

## What runs where

| Piece | How it runs | Why |
| --- | --- | --- |
| Keycloak | container (GitHub Actions `services:`) | the thing under test needs a real one |
| The external provider | **not a service** | Keycloak holds its public key directly, so nothing is fetched; the suite mints assertions with the matching private key |
| sandbox API | `uvicorn` process | needs the repo, so it cannot be a service container |
| oas2mcp | `cargo build` binary | ditto |

Keeping Keycloak as the only container is deliberate: it is what lets the suite
run as a single GHA service with no image to publish and no compose file.

## Running it locally

Start Keycloak, then hand everything else to the harness:

```bash
docker run --rm -d --name kc-e2e -p 8080:8080 \
  -e KC_BOOTSTRAP_ADMIN_USERNAME=admin \
  -e KC_BOOTSTRAP_ADMIN_PASSWORD=admin \
  quay.io/keycloak/keycloak:26.7 start-dev

python3 -m venv .venv && .venv/bin/pip install -r tests/e2e/requirements.txt
cargo build
PYTHON=.venv/bin/python tests/e2e/harness.sh

docker rm -f kc-e2e
```

The harness imports the realm, starts the API and oas2mcp, runs the suite, and
dumps every log on failure. It is idempotent: rerun it against a live Keycloak
and the realm is replaced.

## The realm

[`keycloak/realm-oas2mcp.json`](keycloak/realm-oas2mcp.json) is imported through
the **admin API**, not mounted. A service container starts before the repository
is checked out, so there would be nothing to mount at that point — and this keeps
the file in the repo as the single source of truth.

What it configures, and why each piece matters:

- an identity provider of type `jwt-authorization-grant` for the external
  provider, holding its **public key directly** (`publicKeySignatureVerifier`)
  rather than a JWKS URL;
- users **linked** to that provider (`federatedIdentities`), the link being what
  lets Keycloak resolve an assertion's `sub` to a local user. `carol` is
  deliberately left unlinked, so the suite can prove an unknown external identity
  is refused;
- a `roles` protocol mapper, because Keycloak nests realm roles under
  `realm_access.roles` while `--oauth-role-claim` reads a top-level claim;
- an audience mapper putting `sandbox-api` on the issued tokens, which the API
  then insists on.

The client's `oauth2.jwt.authorization.grant.enabled` and
`.grant.idp` attributes are what allow the grant at all — without them Keycloak
refuses with *"JWT Authorization Grant is not supported for the requested
client"*.

## Constraints worth knowing

Discovered by getting them wrong first:

- **One audience, and only one.** Keycloak refuses an assertion carrying several
  `aud` values *even when all of them are valid*. The assertion must name the
  authorization server alone.
- **`aud` must be the realm issuer or the token endpoint URL** — and the issuer
  Keycloak computes depends on the host it was reached through. If oas2mcp talks
  to `http://localhost:8080`, the assertion's `aud` has to say the same thing.
- **Assertion reuse is refused** (`jwtAuthorizationGrantAssertionReuseAllowed:
  false`), which the suite turns into a feature: a second call with the same
  assertion can only succeed if oas2mcp served it from its per-caller cache. A
  cache regression shows up here as an auth failure rather than as a slowdown.
- **The cache is keyed on the caller identity, not on the assertion.** So a
  *different* assertion for an already-cached subject is served from cache without
  reaching Keycloak. That is the right trade-off — oas2mcp verified the caller's
  signature itself — but it means the "assertion addressed elsewhere" check only
  means anything on a cold entry, which is why it uses a subject of its own.
