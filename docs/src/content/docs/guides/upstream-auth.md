---
title: Authenticating to the upstream API
description: Static and forwarded headers, OAuth2 grants, and acting on behalf of the caller.
sidebar:
  order: 2
---

## Static and forwarded headers

`--header` attaches a fixed `Name: Value` header to every upstream request — a
bearer token, an API key. To forward each MCP client's own `Authorization` (and
a tenant header) to the upstream API instead of a single shared token (`http`
only):

```bash
oas2mcp http \
  --openapi-url https://api.example.com/openapi.json \
  --bind-addr 0.0.0.0:8000 \
  --forward-header Authorization \
  --forward-header X-Tenant-Id
```

A static `--header` of the same name takes precedence over a forwarded one.
Header names are matched case-insensitively. With multiple values set through
the environment variable, separate them with newlines (e.g.
`FORWARD_HEADERS=$'Authorization\nX-Tenant-Id'`).

## OAuth2 client credentials

`--header 'Authorization: Bearer …'` works, until the token expires. To
authenticate the **proxied tool calls** with a token that renews itself, point
`--upstream-oauth-token-url` at your provider: every call then carries a bearer
obtained from a `client_credentials` grant, cached and refreshed shortly before
expiry.

```bash
oas2mcp \
  --openapi-url https://api.example.com/openapi.json \
  --upstream-oauth-token-url https://idp.example.com/oauth/token \
  --upstream-oauth-client-id "$CLIENT_ID" \
  --upstream-oauth-client-secret "$CLIENT_SECRET" \
  --upstream-oauth-scope read:pets \
  --upstream-oauth-token-audience 'https://api.example.com'
```

This is configured independently of `--openapi-oauth-*`: the document and the
API may live behind different providers, with different credentials. Both
support the same two client-authentication modes, so
`--upstream-oauth-private-key` gives you [`private_key_jwt`](/guides/document/#authenticating-with-a-signed-assertion-instead-of-a-secret) here too.

## One source per header

Three things can set the upstream `Authorization`: a static `--header`, the
`--upstream-oauth-*` token, and `--forward-header Authorization`. oas2mcp refuses
to start when two of them are configured, since one would always mask the other
and sit in the configuration doing nothing. The same goes for any header both
set with `--header` and forwarded with `--forward-header`.

If the token cannot be obtained, the tool call **fails** and no request reaches
the API: proxying it unauthenticated would surface as a puzzling `401` from the
upstream rather than the real cause. The failure is logged with the provider's
own diagnosis and counted as `outcome="auth_error"` in the metrics, kept
distinct from an upstream error so a broken credential is not mistaken for a
broken API.

## Acting on behalf of the caller

`client_credentials` gets one token for the server itself, so every tool call
reaches the API as the same principal. The upstream audit log shows one identity,
and the API can no longer apply per-user authorization — the only gate left is
`--inbound-role-mapper`, which filters *tool names*, not data. A `reader:^get`
rule lets `getAllCustomers` through for the intern as readily as for the CFO.

The `jwt-bearer` grant (RFC 7523 §2.1) fixes that: oas2mcp presents a signed
assertion naming the caller, and the provider issues a token *for that user*.

```bash
oas2mcp http \
  --openapi-url https://api.example.com/openapi.json \
  --bind-addr 0.0.0.0:8000 \
  --inbound-jwks-url https://idp.example.com/.well-known/jwks.json \
  --upstream-oauth-token-url https://idp.example.com/oauth/token \
  --upstream-oauth-client-id "$CLIENT_ID" \
  --upstream-oauth-private-key /etc/oas2mcp/upstream-key.pem \
  --upstream-oauth-grant jwt-bearer \
  --upstream-oauth-subject-claim email
```

The caller's JWT is verified against the JWKS, the named claim becomes the
assertion's `sub`, and the resulting upstream token is cached **per caller**.
`sub` is the default claim, but many providers mint an opaque identifier the
upstream authorization server does not recognise — hence `email` above.

Three properties worth knowing, because they are the difference between
delegation and a security hole:

- **No fallback.** A call with no verified identity is refused, and counted as
  `auth_error`. Quietly falling back to the client's own token would hand the
  least-authorized caller the broadest identity the server has, turning a
  configuration slip into a privilege escalation. For the same reason the server
  **refuses to start** if the grant delegates but no call could ever carry an
  identity (no JWKS, or a transport with no client headers).
- **Tokens are cached per `(issuer, subject)`, not per subject.** A `sub` is only
  unique *within* an issuer, so two providers both minting `sub: alice` would
  otherwise share one entry — and one tenant would receive another's token. The
  cache is bounded (10k entries, evicting whatever expires soonest), because it
  grows with your active user count.
- **A delegated token is never cached past the caller's own `exp`.** Otherwise
  revoking a user leaves a usable upstream token behind until the *upstream*
  token expires, which can be much later.

### Choosing the mode

| You want | Flags |
| --- | --- |
| One shared service identity | `--upstream-oauth-grant client-credentials` (the default) |
| A named service account | `--upstream-oauth-grant jwt-bearer --upstream-oauth-subject svc@example.com` |
| Per-caller delegation | `--upstream-oauth-grant jwt-bearer` (subject from the caller's claim) |
| Relay the caller's own token | `--upstream-oauth-grant jwt-bearer-relay` |

`jwt-bearer` needs `--upstream-oauth-private-key` to sign its assertion: a shared secret
cannot sign one, and oas2mcp says so at startup rather than failing every call.
The same key signs both the client assertion (§2.2) and the grant assertion
(§2.1) — it is loaded once.

**The `jwt-bearer` key is a powerful credential.** The provider must be
configured to trust oas2mcp to assert those subjects, which makes that key, in
effect, "speak as anyone". Keep the provider's trust configuration as narrow as
it goes, scope the upstream token to the minimum, and treat the key accordingly.

`jwt-bearer-relay` signs nothing and needs no key: the caller's verified JWT is
relayed as the assertion, so the provider trusts *their* issuer rather than us.
It is the cleanest option when it works, but it requires the caller's token to be
addressed (`aud`) to the authorization server, which most identity providers do
not do by default. When that is not the case, the mechanism you actually want is
RFC 8693 token exchange, which oas2mcp does not implement.

> Client authentication is still required alongside the `jwt-bearer` grant
> (`--upstream-oauth-client-secret` or `--upstream-oauth-private-key`).
> Providers that accept an assertion-only grant with no client authentication are
> not supported.
