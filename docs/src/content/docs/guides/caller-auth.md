---
title: Authenticating callers
description: Verify the caller's JWT, gate tools by role, and let MCP clients discover the authorization server.
sidebar:
  order: 3
---

The [operation filters](/oas2mcp/guides/operations/) are global: every MCP client sees the same tools. To tie
access to the **caller's own JWT**, give oas2mcp a JWKS (`--inbound-jwks-url`,
fetched once at startup, or `--inbound-jwks-file`): the incoming request's
`Authorization: Bearer` JWT is then verified against it. A verified caller may
use every tool; a caller with no token or an invalid/expired one gets only the
[public tools](#public-tools), if any (it may list every tool with
[anonymous discovery](#anonymous-discovery)) — or, with `--inbound-resource`, is refused
with a `401` that tells it where to log in (see
[below](#letting-mcp-clients-find-the-authorization-server)).

When callers have different privileges, add one or more `--inbound-role-mapper`
entries of the form `role:operation_regex`: a tool is then visible (in
`tools/list`) and callable (in `tools/call`) only when one of the caller's roles
maps to a regex matching its operation name — the `operationId` before
`--rename`, like the filters. The roles are read from the
`--inbound-role-claim` claim (default `roles`; an array of strings or a
whitespace-separated string), and a caller whose roles match no mapping gets
the public tools only.

```bash
oas2mcp http \
  --openapi-url https://api.example.com/openapi.json \
  --bind-addr 0.0.0.0:8000 \
  --inbound-jwks-url https://idp.example.com/.well-known/jwks.json \
  --inbound-role-claim roles \
  --inbound-role-mapper 'admin:.*' \
  --inbound-role-mapper 'reader:^get'
# admins get every tool; readers only the ones whose name starts with "get".
```

This needs the caller's JWT, which only the `http` transport
exposes — under `stdio`/`sse` no token is available, so only the public tools
are exposed. The signature is verified with the key family advertised by the JWK
(an algorithm-substitution downgrade such as `HS256` against a public key is
rejected), and the token's `exp` is enforced. Invalid regexes are rejected at
startup. With multiple entries set through the environment variable, separate
them with newlines (e.g. `INBOUND_ROLE_MAPPER=$'admin:.*\nreader:^get'`).

## Tracing the caller's JWT claims

Once JWTs are verified for role-based access, you can echo selected claims into
the logs to see *who* made each call. Pass one or more `--trace-claim` with the
claim names you care about; each one that the token actually carried is emitted
on the tool-call log line as a single `jwt.claims` field (a JSON object that
keeps every value's original shape — strings, numbers, arrays):

```bash
oas2mcp http \
  --openapi-url https://api.example.com/openapi.json \
  --bind-addr 0.0.0.0:8000 \
  --inbound-jwks-url https://idp.example.com/.well-known/jwks.json \
  --inbound-role-mapper 'admin:.*' \
  --trace-claim sub \
  --trace-claim email \
  --trace-claim tenant_id
# logs, per call: jwt.claims={"sub":"u-123","email":"a@b.com","tenant_id":42}
```

The claims come from the same verified JWT used for role mapping, so
`--trace-claim` only takes effect when a JWKS is configured. Claims go to the logs only — never to metric labels — so a
high-cardinality claim such as `sub` can't blow up your metrics backend.
With multiple names set through the environment variable, separate them with
newlines (e.g. `TRACE_CLAIMS=$'sub\nemail'`).

## Scoping the tokens you accept

Verifying a signature answers "did my identity provider sign this?", not "was
this meant for me". Those are different questions, and only the second one keeps
a token minted for another service out:

```bash
oas2mcp http \
  --bind-addr 0.0.0.0:8000 \
  --inbound-jwks-url https://idp.example.com/.well-known/jwks.json \
  --inbound-role-mapper 'admin:.*' \
  --inbound-expected-audience oas2mcp \
  --inbound-expected-issuer https://idp.example.com/
```

- **`--inbound-expected-audience` is the one that matters.** Without it, every token
  your JWKS can verify is accepted — including one your provider issued for a
  different service entirely, with whatever roles it happens to carry. `aud` is
  what scopes a token to one audience; checking it is what stops it being replayed
  here. oas2mcp warns at startup while it is unset.
- **`--inbound-expected-issuer` is defence in depth.** The JWKS already pins who
  signed the token, so this mainly catches a key deliberately shared across
  logical issuers — a staging and a production realm behind one key set, say. It
  also firms up delegation, where the issuer is half of the identity a delegated
  upstream token is cached under.
- **Setting either makes the claim mandatory.** A token that simply omits `aud`
  is not addressed to us any more than one addressed elsewhere, so it is refused
  too — otherwise the check would be bypassable by leaving the claim out.
- **A token with no `exp` is always refused**, configured or not. A bearer
  credential that never expires is not something to accept quietly.

Both are opt-in rather than on by default, because switching them on
unconditionally would reject the tokens of every deployment that predates them.
That is a migration concern, not a recommendation: set them.

If tokens are rejected intermittently — right after being issued, or just before
expiring — suspect the clocks before the config, and widen `--inbound-clock-skew`.

## Letting MCP clients find the authorization server

By default a caller without a token is not turned away: it gets an empty tool
list, and nothing tells it where a token would come from. An MCP client that
implements the [MCP authorization spec](https://modelcontextprotocol.io/specification/latest/basic/authorization)
can do the login itself, provided the server points it at the authorization
server. Set `--inbound-resource` to the URL clients reach the endpoint under:

```bash
oas2mcp http \
  --bind-addr 0.0.0.0:8000 \
  --inbound-jwks-url https://idp.example.com/realms/main/protocol/openid-connect/certs \
  --inbound-expected-issuer https://idp.example.com/realms/main \
  --inbound-expected-audience oas2mcp \
  --inbound-resource https://mcp.example.com/mcp
```

Every request to `/mcp` then needs a valid bearer token. Without one the answer
is:

```http
HTTP/1.1 401 Unauthorized
WWW-Authenticate: Bearer resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource/mcp"
```

and the metadata it points at, served without authentication, names the
`--inbound-expected-issuer` values as the authorization servers:

```json
{
  "resource": "https://mcp.example.com/mcp",
  "authorization_servers": ["https://idp.example.com/realms/main"],
  "bearer_methods_supported": ["header"]
}
```

The client reads the authorization server's own metadata from there, runs the
OAuth flow, and retries with the token. An invalid or expired token gets the same
`401`, with `error="invalid_token"` added to the challenge.

- **The issuers are the authorization servers.** Advertising any other server
  would send clients to fetch tokens this one then refuses, so at least one
  `--inbound-expected-issuer` is required.
- **The metadata is served at two paths**: the one derived from the resource URL
  (`/.well-known/oauth-protected-resource/mcp`, per RFC 9728 §3) and the bare
  `/.well-known/oauth-protected-resource` clients fall back to. Behind a reverse
  proxy that rewrites paths, route both to oas2mcp.
- **A valid token with no matching role is let through**, and sees an empty tool
  list: authentication decides whether the request is accepted, the roles decide
  what it can use.
- **The client must be able to register with the authorization server.** How it
  gets a client ID (dynamic registration, a pre-registered public client, …) is
  settled between the client and the authorization server; oas2mcp takes no
  part in it.

## Public tools

Some tools can be open to anyone — a catalogue lookup, a status check — while
the rest needs a login. Map them to the reserved role `*` in
`--inbound-role-mapper`:

```bash
oas2mcp http \
  --bind-addr 0.0.0.0:8000 \
  --inbound-jwks-url https://idp.example.com/realms/main/protocol/openid-connect/certs \
  --inbound-expected-issuer https://idp.example.com/realms/main \
  --inbound-role-mapper '*:^get_public_' \
  --inbound-resource https://mcp.example.com/mcp
```

A caller without a token sees and can call the public tools only; an
authenticated caller gets them on top of what its roles grant — and, when `*`
entries are the only ones, every tool, as without a mapper at all. A role
literally named `*` in your identity provider grants nothing more than being
anonymous. With
`--inbound-resource`, an anonymous client is no longer turned away on connection:
`initialize`, `tools/list` and calls to public tools go through, and the `401`
challenge comes when it calls a tool that is not public. An *invalid* token is
still challenged on any request, so a client learns its token needs replacing.

- **Some clients only log in when the connection itself is refused.** Against
  such a client, public tools mean it stays anonymous and sees the public tools
  alone. Map no tool to `*` when every client must log in.
- **A public tool has no caller identity to delegate.** With
  `--upstream-oauth-grant jwt-bearer` acting per caller, an anonymous call to a
  public tool fails with an upstream token error; oas2mcp warns about the
  combination at startup. A shared upstream identity (`client-credentials`, or
  a fixed `--upstream-oauth-subject`) serves anonymous calls fine.

## Anonymous discovery

A service that catalogues what the server offers — a gateway, a registry —
needs the whole tool list without holding a user token.
`--inbound-anonymous-discovery` gives it that: a `tools/list` without a valid
token returns every tool. Nothing else widens: an anonymous `tools/call` is
still limited to the public tools (refused with `tool … needs a bearer token`,
or challenged with a `401` under `--inbound-resource`), and a caller with a
verified token still lists only what its roles grant.

The trade-offs are the public tools' ones: the tool names, descriptions and
input schemas are readable by anyone who reaches `/mcp`, and under
`--inbound-resource` a client that only logs in when its connection is refused
stays anonymous, listing tools it cannot call.
