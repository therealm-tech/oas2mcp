"""The sandbox API for the oas2mcp end-to-end suite.

It plays two roles at once:

- it *publishes* an OpenAPI document, behind a bearer token, so the
  `--openapi-oauth-*` document-fetch grant has something to authenticate against;
- it *attributes what it returns to the caller it saw*, which is what makes
  delegation observable: the test asserts the API served alice's pets, not a
  shared service account's.

Every token is verified against Keycloak for real — signature, issuer and
audience — so a token oas2mcp mangled or obtained for the wrong audience is
refused here rather than quietly accepted.
"""

import os

import jwt
from fastapi import Depends, FastAPI, HTTPException, Request
from jwt import PyJWKClient

ISSUER = os.environ["OIDC_ISSUER"]
# Tokens must be addressed to this API, not merely signed by the realm.
AUDIENCE = os.environ.get("OIDC_AUDIENCE", "sandbox-api")
# The URL oas2mcp uses to reach us, advertised in the document's `servers`.
PUBLIC_URL = os.environ.get("PUBLIC_URL", "http://127.0.0.1:8000")

app = FastAPI(
    title="Sandbox",
    version="1.0.0",
    description="Fixture API for the oas2mcp end-to-end suite.",
    servers=[{"url": PUBLIC_URL}],
    # The document is served from a protected route below, not the default
    # public one, so the fetch has to authenticate.
    openapi_url=None,
    docs_url=None,
    redoc_url=None,
)

_jwks: PyJWKClient | None = None


def jwks() -> PyJWKClient:
    """The realm's key set, fetched once and cached."""
    global _jwks
    if _jwks is None:
        _jwks = PyJWKClient(f"{ISSUER}/protocol/openid-connect/certs")
    return _jwks


def caller(request: Request) -> dict:
    """Verify the bearer token and return its claims."""
    header = request.headers.get("authorization", "")
    scheme, _, token = header.partition(" ")
    if scheme.lower() != "bearer" or not token:
        raise HTTPException(status_code=401, detail="a bearer token is required")
    try:
        key = jwks().get_signing_key_from_jwt(token).key
        return jwt.decode(
            token,
            key,
            algorithms=["RS256"],
            audience=AUDIENCE,
            issuer=ISSUER,
            options={"require": ["exp"]},
        )
    except Exception as err:  # noqa: BLE001 - the reason is the useful part here
        raise HTTPException(status_code=401, detail=f"invalid token: {err}") from err


def owner(claims: dict) -> str:
    """Who this call is attributed to.

    `preferred_username` for a delegated token — the end user Keycloak resolved
    from the assertion — falling back to the client id when the token was
    obtained by the server acting as itself.
    """
    return claims.get("preferred_username") or claims.get("azp") or "unknown"


@app.get("/openapi.json", include_in_schema=False)
def openapi_document(_: dict = Depends(caller)) -> dict:
    """The OpenAPI document, behind the same token check as everything else."""
    return app.openapi()


@app.get("/whoami", operation_id="whoami", tags=["identity"])
def whoami(claims: dict = Depends(caller)) -> dict:
    """Report the identity this call arrived as.

    For a delegated token `sub` and `azp` differ: the client is oas2mcp, the
    subject is the user it acted for.
    """
    return {
        "ownerId": owner(claims),
        "sub": claims.get("sub"),
        "azp": claims.get("azp"),
        "aud": claims.get("aud"),
        "roles": claims.get("roles"),
    }


@app.get("/pets", operation_id="getPets", tags=["pets"])
def get_pets(claims: dict = Depends(caller)) -> dict:
    """List the caller's pets, attributed to whoever the token names."""
    who = owner(claims)
    return {
        "ownerId": who,
        "pets": [
            {"id": 1, "name": "Rex", "owner": who},
            {"id": 2, "name": "Whiskers", "owner": who},
        ],
    }


@app.post("/pets", operation_id="createPet", tags=["pets"])
def create_pet(pet: dict, claims: dict = Depends(caller)) -> dict:
    """Create a pet. A write tool, so the role mapper has something to refuse."""
    return {"id": 3, "owner": owner(claims), **pet}


@app.get("/healthz", include_in_schema=False)
def healthz() -> dict:
    """Unauthenticated, so the harness can wait on it."""
    return {"status": "ok"}
