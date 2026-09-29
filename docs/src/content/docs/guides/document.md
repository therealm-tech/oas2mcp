---
title: Loading and reloading the document
description: Fetch the OpenAPI document from a URL, reload it on an interval, and authenticate the fetch.
sidebar:
  order: 1
---

When the document lives behind a URL — and especially when that API still
evolves — pass `--reload-every` to re-fetch it on an interval and rebuild the
tool set in place. If the URL is private, authenticate the fetch with
`--openapi-header` (this is the document URL's own auth, separate from the
upstream `--header`):

```bash
oas2mcp http \
  --openapi-url https://api.example.com/openapi.json \
  --openapi-header 'Authorization: Bearer <docs-token>' \
  --reload-every 5m \
  --bind-addr 0.0.0.0:8000
```

The interval accepts any `humantime` duration (`30s`, `5m`, `1h`, `90m`, …).
If a reload fails to fetch or parse, the error is logged and the previously
loaded tool set is kept, so a transient upstream blip never empties the server.
`--reload-every` is ignored when the document is loaded from a file.

When a reload changes the advertised tools (a tool added or removed, a new
description or input schema), the server sends
`notifications/tools/list_changed`, and it advertises `tools.listChanged` only
when the document is reloaded. A client receives the notification when it has
a channel to receive it on:

- a client using protocol `2026-07-28` receives it on its
  `subscriptions/listen` stream, on every transport;
- an older client receives it on `stdio`, `sse`, and `http --stream-responses`
  (on its `GET /mcp` stream). Plain `http` is stateless and keeps no stream
  open for an older client, which picks up the new tools on its next
  `tools/list` call.

## Fetching the document with the upstream credentials

When the API serves its own document, the credentials that call it can fetch
the document too. `--openapi-auth upstream` sends the document request with
`--header` and the `--upstream-oauth-*` token, so none of the `--openapi-header`
/ `--openapi-oauth-*` flags need repeating:

```bash
oas2mcp \
  --openapi-url https://api.example.com/openapi.json \
  --openapi-auth upstream \
  --upstream-oauth-token-url https://idp.example.com/oauth/token \
  --upstream-oauth-client-id "$CLIENT_ID" \
  --upstream-oauth-client-secret "$CLIENT_SECRET"
```

Setting an `--openapi-header` or `--openapi-oauth-*` flag alongside it is refused,
since it would never be read. So is an upstream token obtained per caller
(`jwt-bearer` without `--upstream-oauth-subject`, or `jwt-bearer-relay`): the
document fetch has no caller to act for.

## OAuth for the document fetch

A static `--openapi-header` bearer token works for a one-shot fetch, but on a
long-running server it eventually expires and the reloads start failing. For
that case, authenticate the document fetch with an OAuth2 `client_credentials`
grant: the server obtains a token from the provider, caches it, and refreshes
it automatically shortly before expiry — so the periodic reload keeps working
indefinitely.

```bash
oas2mcp http \
  --openapi-url https://api.example.com/openapi.json \
  --reload-every 1h \
  --openapi-oauth-token-url https://idp.example.com/oauth/token \
  --openapi-oauth-client-id "$CLIENT_ID" \
  --openapi-oauth-client-secret "$CLIENT_SECRET" \
  --openapi-oauth-scope read:openapi \
  --bind-addr 0.0.0.0:8000
```

Client authentication uses HTTP Basic against the token endpoint (RFC 6749).
The OAuth bearer takes precedence over any static `Authorization` set via
`--openapi-header`. This auth covers the **document fetch only**; upstream API
calls are configured separately (see [Authenticating to the upstream API](/guides/upstream-auth/#oauth2-client-credentials)).

### Authenticating with a signed assertion instead of a secret

Some providers will not issue a client secret at all, and some setups would
rather not have a long-lived shared secret sitting in the environment. Point
`--openapi-oauth-private-key` at a PKCS#8 PEM private key and the client
authenticates with a JWT assertion it signs per request — `private_key_jwt`,
RFC 7523 §2.2 — instead of Basic:

```bash
oas2mcp \
  --openapi-url https://api.example.com/openapi.json \
  --reload-every 1h \
  --openapi-oauth-token-url https://idp.example.com/oauth/token \
  --openapi-oauth-client-id "$CLIENT_ID" \
  --openapi-oauth-private-key /etc/oas2mcp/client-key.pem \
  --openapi-oauth-key-id client-key-2026 \
  --openapi-oauth-signing-alg es256 \
  --openapi-oauth-scope read:openapi
```

The assertion carries `iss` and `sub` set to the client id, `aud` set to the
token endpoint (override with `--openapi-oauth-assertion-audience` if your
provider expects its issuer identifier), `iat`/`exp` bounding a 60-second
window, and a fresh `jti` per request so the provider's replay cache has
something to work with. A new assertion is signed for every token request —
they are never cached alongside the token.

Register the **public** half of the key with the provider (as a JWKS entry or
an uploaded certificate, depending on the provider) and keep the private half
to yourself:

- The key is only ever read from a file. There is deliberately no environment
  variable for the key material itself.
- Keep the file owner-only (`chmod 400`). `oas2mcp` warns at startup when it is
  world-readable. Group access is tolerated silently, because that is how a
  non-root container reads a Kubernetes `Secret` volume (`defaultMode: 0440`
  with an `fsGroup`) — the Helm chart wires that up for you.
- Only asymmetric algorithms are offered. RFC 7523 §2.2 also permits a MAC, but
  an HMAC keyed on the client secret is no better than sending the secret, so
  `hs256` and friends are not accepted.
