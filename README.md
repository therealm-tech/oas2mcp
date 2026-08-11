# oas2mcp

Load an [OpenAPI](https://www.openapis.org/) document at startup and expose
every operation it describes as a tool of a
[Model Context Protocol (MCP)](https://modelcontextprotocol.io/) server.

Each OpenAPI operation becomes one MCP tool. When a client calls the tool,
`oas2mcp` builds and sends the corresponding HTTP request to the upstream API
and returns the response. In other words, it turns any HTTP API that ships an
OpenAPI description into something an MCP-capable agent can drive — without
writing a line of glue code.

## Features

- **Input from a file or a URL** — the document is fetched/read at startup, in
  JSON or YAML. A non-public document URL can be authenticated with
  `--openapi-header`.
- **OpenAPI 3.0 and 3.1** — both revisions are read by the same code path.
  Schemas are passed through to MCP clients exactly as the document writes
  them, so 3.1's JSON Schema 2020-12 keywords (`type` arrays such as
  `[string, "null"]`, `const`, `prefixItems`, numeric `exclusiveMinimum`,
  boolean schemas, `$defs`) survive intact, as do 3.0's (`nullable`,
  `example`). 3.1's `components.pathItems`, `$ref` siblings and optional
  `paths` are supported too.
- **Periodic reload** — with `--reload-every`, a document loaded from a URL is
  re-fetched on an interval and the exposed tool set is rebuilt in place,
  without restarting the server. The fetch can authenticate via OAuth2
  `client_credentials` (auto-refreshed token), so reloads keep working on a
  long-running server where a static token would expire. The client
  authenticates with either a shared secret or a signed JWT assertion
  (`private_key_jwt`, RFC 7523 §2.2), for providers that will not issue a
  secret.
- **One tool per operation** — `operationId` becomes the tool name (falling
  back to `<method>_<path>`); path, query and header parameters become
  top-level tool arguments, and a JSON request body is passed as a `body`
  argument. Local `$ref`s are inlined into each tool's input schema, whatever
  they point at (`#/components/schemas/…`, `#/$defs/…`, …); a recursive schema
  collapses to a bare object rather than expanding forever.
- **Three transports** — the MCP server can be exposed over:
  - `stdio` — for a local subprocess MCP client.
  - `streamable-http` — the current remote transport, single `POST /mcp`
    endpoint. By default each request is answered with a single
    `application/json` body (stateless), which is the most interoperable mode
    — notably with strict proxies such as Envoy AI Gateway. Pass
    `--stream-responses` to reply with a `text/event-stream` (SSE) flow and keep
    stateful sessions instead.
  - `sse` — the legacy HTTP+SSE transport (deprecated by the MCP spec, kept
    for compatibility with older clients).
- **Auth passthrough** — attach arbitrary static headers (e.g. a bearer token)
  to every upstream request, or forward the MCP client's own request headers
  (e.g. `Authorization`) upstream per call (`streamable-http` only).
- **OAuth for the upstream API** — obtain the upstream `Authorization: Bearer`
  from an OAuth2 grant, refreshed automatically before it expires, instead of a
  static token that goes stale. Authenticates with a client secret or a signed
  JWT assertion (RFC 7523 §2.2), and is configured independently of the
  document-fetch grant.
- **Acting on behalf of the caller** — with the `jwt-bearer` grant (RFC 7523
  §2.1), obtain a *per-caller* upstream token from the identity in their verified
  JWT, so the upstream API sees who is really acting and applies its own
  authorization, instead of every call arriving as one shared service account.
- **Role-based tool access** — verify the caller's JWT against a JWKS and gate
  which tools they can see and call, mapping each `role` to a tool-name regex
  (`streamable-http` only).
- **JWT claim tracing** — with `--trace-claim`, echo selected claims from the
  verified token (e.g. `sub`, `email`, `tenant_id`) onto each tool-call log line
  to see who made each call, without inflating metric cardinality.
- **OpenTelemetry metrics** — count and time every tool call, labelled by tool
  and outcome (kept low-cardinality), exported over OTLP and/or a Prometheus
  `/metrics` endpoint.
- **Custom CA trust** — point `--ca-cert` at a PEM bundle to trust a private or
  corporate CA for every outbound TLS connection (upstream API, document fetch,
  OAuth, JWKS), on top of the built-in public roots.
- **Graceful shutdown** on `SIGTERM`/`SIGINT`.

## Install

Requires a recent Rust toolchain (edition 2024).

```bash
cargo build --release
# binary at target/release/oas2mcp
```

Or with Docker:

```bash
docker build -t oas2mcp .
```

## Usage

```text
oas2mcp [OPTIONS]
```

The OpenAPI source is required: pass exactly one of `--openapi-file` or
`--openapi-url`.

| Option            | Env              | Default          | Description                                                        |
| ----------------- | ---------------- | ---------------- | ------------------------------------------------------------------ |
| `--openapi-file`  | `OPENAPI_FILE`   | —                | Path to an OpenAPI document (JSON or YAML) on disk.                |
| `--openapi-url`   | `OPENAPI_URL`    | —                | URL of an OpenAPI document fetched at startup (and on each reload).|
| `--openapi-header`| `OPENAPI_HEADERS`| —                | `Name: Value` header sent when fetching `--openapi-url` (e.g. for a private document). Repeatable. |
| `--reload-every`  | `RELOAD_EVERY`   | —                | Re-fetch `--openapi-url` on this interval and rebuild the tool set (e.g. `30s`, `5m`, `1h`). Off by default; ignored for a file source. |
| `--openapi-oauth-token-url` | `OPENAPI_OAUTH_TOKEN_URL` | — | OAuth2 `client_credentials` token endpoint. Set → the document fetch uses an auto-refreshed bearer token. Requires `--openapi-oauth-client-id` plus one of the two credentials below. |
| `--openapi-oauth-client-id` | `OPENAPI_OAUTH_CLIENT_ID` | — | OAuth2 client ID for the document-fetch token.                     |
| `--openapi-oauth-client-secret` | `OPENAPI_OAUTH_CLIENT_SECRET` | — | OAuth2 client secret, sent over HTTP Basic. Prefer the env var so it stays out of the process list. Mutually exclusive with `--openapi-oauth-private-key`. |
| `--openapi-oauth-private-key` | `OPENAPI_OAUTH_PRIVATE_KEY_FILE` | — | Path to a PKCS#8 PEM private key. Set → the client authenticates with a signed JWT assertion (`private_key_jwt`, RFC 7523 §2.2) instead of a secret. Mutually exclusive with `--openapi-oauth-client-secret`. |
| `--openapi-oauth-key-id` | `OPENAPI_OAUTH_KEY_ID` | —              | `kid` header on the client assertion, when the provider has several keys registered for the client. Needs `--openapi-oauth-private-key`. |
| `--openapi-oauth-signing-alg` | `OPENAPI_OAUTH_SIGNING_ALG` | `rs256` | Assertion signature algorithm: `rs256`/`rs384`/`rs512`, `ps256`/`ps384`/`ps512`, `es256`/`es384`, `eddsa`. Must match the key type. |
| `--openapi-oauth-assertion-audience` | `OPENAPI_OAUTH_ASSERTION_AUDIENCE` | token endpoint | `aud` claim of the client assertion. Override when the provider expects its issuer identifier rather than the token endpoint URL. |
| `--openapi-oauth-assertion-lifetime` | `OPENAPI_OAUTH_ASSERTION_LIFETIME` | `60s` | How long a client assertion stays valid (e.g. `30s`, `2m`). |
| `--openapi-oauth-scope` | `OPENAPI_OAUTH_SCOPES` | —          | OAuth2 scope requested (sent space-joined). Repeatable; newline-separated via the env var. |
| `--openapi-oauth-audience` | `OPENAPI_OAUTH_AUDIENCE` | —    | OAuth2 `audience` parameter, when the provider requires it (e.g. Auth0). |
| `--base-url`      | `BASE_URL`       | spec `servers`   | Upstream API base URL that tool calls are proxied to.              |
| `--ca-cert`       | `CA_CERT_FILE`   | —                | Path to a PEM file with extra CA certificate(s) to trust for every outbound TLS connection (upstream, document fetch, OAuth, JWKS). Added on top of the built-in roots, so only your private/corporate CA is needed. Repeatable; newline-separated via the env var. |
| `--header`        | `UPSTREAM_HEADERS` | —              | Extra `Name: Value` header on every upstream request. Repeatable.  |
| `--forward-header`| `FORWARD_HEADERS`  | —              | Name of an incoming request header to forward upstream (e.g. `Authorization`). Repeatable. `streamable-http` only. |
| `--upstream-oauth-token-url` | `UPSTREAM_OAUTH_TOKEN_URL` | — | OAuth2 `client_credentials` token endpoint for **upstream API calls**. Set → every proxied call carries an auto-refreshed bearer. Requires `--upstream-oauth-client-id` plus one credential below. |
| `--upstream-oauth-client-id` | `UPSTREAM_OAUTH_CLIENT_ID` | —  | OAuth2 client ID for the upstream token.                           |
| `--upstream-oauth-client-secret` | `UPSTREAM_OAUTH_CLIENT_SECRET` | — | OAuth2 client secret, sent over HTTP Basic. Mutually exclusive with `--upstream-oauth-private-key`. |
| `--upstream-oauth-private-key` | `UPSTREAM_OAUTH_PRIVATE_KEY_FILE` | — | PKCS#8 PEM key: authenticate with a signed JWT assertion (RFC 7523 §2.2) instead of a secret. |
| `--upstream-oauth-key-id` | `UPSTREAM_OAUTH_KEY_ID` | —          | `kid` header on the upstream client assertion. Needs the private key. |
| `--upstream-oauth-signing-alg` | `UPSTREAM_OAUTH_SIGNING_ALG` | `rs256` | Assertion signature algorithm. Must match the key type. |
| `--upstream-oauth-assertion-audience` | `UPSTREAM_OAUTH_ASSERTION_AUDIENCE` | token endpoint | `aud` claim of the upstream client assertion. |
| `--upstream-oauth-assertion-lifetime` | `UPSTREAM_OAUTH_ASSERTION_LIFETIME` | `60s` | Upstream client assertion validity window. |
| `--upstream-oauth-scope` | `UPSTREAM_OAUTH_SCOPES` | —          | OAuth2 scope requested for the upstream token. Repeatable; newline-separated via the env var. |
| `--upstream-oauth-audience` | `UPSTREAM_OAUTH_AUDIENCE` | —      | OAuth2 `audience` parameter for the upstream token (e.g. Auth0). |
| `--upstream-oauth-grant` | `UPSTREAM_OAUTH_GRANT` | `client-credentials` | `client-credentials`, or `jwt-bearer` (RFC 7523 §2.1) to obtain the token on behalf of a subject. |
| `--upstream-oauth-assertion` | `UPSTREAM_OAUTH_ASSERTION` | `self-signed` | Who signs the `jwt-bearer` assertion: `self-signed` (by oas2mcp) or `caller` (relay the caller's own JWT). |
| `--upstream-oauth-issuer` | `UPSTREAM_OAUTH_ISSUER` | client id | `iss` of the `jwt-bearer` assertion, identifying oas2mcp to the provider. |
| `--upstream-oauth-subject` | `UPSTREAM_OAUTH_SUBJECT` | —      | Fixed `sub` for the assertion — a service account. Every caller shares one token. Mutually exclusive with the claim below. |
| `--upstream-oauth-subject-claim` | `UPSTREAM_OAUTH_SUBJECT_CLAIM` | `sub` | Claim of the **caller's** verified JWT whose value becomes the assertion's `sub`. Needs `--oauth-role-mapper` and `streamable-http`. |
| `--oauth-role-mapper` | `OAUTH_ROLE_MAPPER` | —          | `role:tool_name_regex` mapping that gates tool visibility/invocation on the caller's JWT roles. Repeatable. Requires a JWKS source below. `streamable-http` only. |
| `--oauth-jwks-url` | `OAUTH_JWKS_URL` | —              | URL of a JWKS document (fetched at startup) used to verify incoming JWTs. Required with `--oauth-role-mapper` (or use `--oauth-jwks-file`). |
| `--oauth-jwks-file` | `OAUTH_JWKS_FILE` | —            | Path to a JWKS document on disk. Mutually exclusive with `--oauth-jwks-url`. |
| `--oauth-role-claim` | `OAUTH_ROLE_CLAIM` | `roles`    | JWT claim listing the caller's roles (array of strings, or a whitespace-separated string). |
| `--trace-claim`   | `TRACE_CLAIMS`   | —                | JWT claim name to log on each tool call as a `jwt.claims` field (e.g. `sub`, `email`, `tenant_id`). Repeatable; newline-separated via the env var. Logged only, never a metric label. Needs `--oauth-role-mapper`. |
| `--include`       | `INCLUDE_OPERATIONS` | —            | Only expose operations whose name matches this glob (`*`/`?`). Repeatable. |
| `--exclude`       | `EXCLUDE_OPERATIONS` | —            | Drop operations whose name matches this glob. Repeatable. Wins over `--include`/`--tag`. |
| `--include-regex` | `INCLUDE_OPERATIONS_REGEX` | —      | Only expose operations whose name matches this regex. Repeatable. |
| `--exclude-regex` | `EXCLUDE_OPERATIONS_REGEX` | —      | Drop operations whose name matches this regex. Repeatable. Wins over the allowlist. |
| `--tag`           | `INCLUDE_TAGS`   | —                | Only expose operations carrying this OpenAPI tag (case-insensitive). Repeatable. |
| `--exclude-tag`   | `EXCLUDE_TAGS`   | —                | Drop operations carrying this OpenAPI tag (case-insensitive). Repeatable. Wins over the allowlist. |
| `--otlp-endpoint` | `OTEL_EXPORTER_OTLP_ENDPOINT` | — | Base OTLP endpoint to push tool-call metrics to over HTTP (e.g. `http://localhost:4318`); `/v1/metrics` is appended. Set → OTLP export on. |
| `--metrics-addr`  | `METRICS_ADDR`   | —                | Address to serve a Prometheus `/metrics` endpoint on (e.g. `0.0.0.0:9090`). Set → scrape endpoint on. Independent of `--otlp-endpoint`. |
| `--otel-service-name` | `OTEL_SERVICE_NAME` | `oas2mcp`   | `service.name` reported on exported metrics.                       |
| `--transport`     | `TRANSPORT`      | `stdio`          | One of `stdio`, `sse`, `streamable-http`.                          |
| `--bind-addr`     | `BIND_ADDR`      | `127.0.0.1:8000` | Bind address for the `sse` and `streamable-http` transports.       |
| `--stream-responses` | `STREAM_RESPONSES` | `false`      | Reply on `streamable-http` with an SSE flow and stateful sessions instead of the default single `application/json` body. `streamable-http` only. |
| `--log-filter`    | `RUST_LOG`       | `info`           | `tracing` filter directive (e.g. `oas2mcp=debug,rmcp=warn`).       |

Configuration resolves CLI flags → environment variables → defaults, and every
option is settable through its environment variable. When the base URL is not
passed explicitly, the first absolute entry of the document's `servers` list is
used.

### Examples

Expose the bundled Petstore example over stdio:

```bash
oas2mcp --openapi-file examples/petstore.yaml
```

The same API restated in OpenAPI 3.1 — union types, `const`, `prefixItems`,
`components.pathItems`, a `webhooks` section — is in
`examples/petstore-3.1.yaml`, and needs no different invocation:

```bash
oas2mcp --openapi-file examples/petstore-3.1.yaml
```

Serve a remote API over Streamable HTTP, forwarding a bearer token upstream:

```bash
oas2mcp \
  --openapi-url https://api.example.com/openapi.json \
  --transport streamable-http \
  --bind-addr 0.0.0.0:8000 \
  --header 'Authorization: Bearer <token>'
# MCP endpoint: POST http://0.0.0.0:8000/mcp
```

Forward each MCP client's own `Authorization` (and a tenant header) to the
upstream API instead of a single shared token (`streamable-http` only):

```bash
oas2mcp \
  --openapi-url https://api.example.com/openapi.json \
  --transport streamable-http \
  --bind-addr 0.0.0.0:8000 \
  --forward-header Authorization \
  --forward-header X-Tenant-Id
```

A static `--header` of the same name takes precedence over a forwarded one.
Header names are matched case-insensitively. With multiple values set through
the environment variable, separate them with newlines (e.g.
`FORWARD_HEADERS=$'Authorization\nX-Tenant-Id'`).

Serve over the legacy SSE transport:

```bash
oas2mcp --openapi-file examples/petstore.yaml --transport sse
# SSE stream:   GET  http://127.0.0.1:8000/sse
# Client posts: POST http://127.0.0.1:8000/messages?sessionId=<id>
```

### Reloading the document from a URL

When the document lives behind a URL — and especially when that API still
evolves — pass `--reload-every` to re-fetch it on an interval and rebuild the
tool set in place. If the URL is private, authenticate the fetch with
`--openapi-header` (this is the document URL's own auth, separate from the
upstream `--header`):

```bash
oas2mcp \
  --openapi-url https://api.example.com/openapi.json \
  --openapi-header 'Authorization: Bearer <docs-token>' \
  --reload-every 5m \
  --transport streamable-http \
  --bind-addr 0.0.0.0:8000
```

The interval accepts any `humantime` duration (`30s`, `5m`, `1h`, `90m`, …).
If a reload fails to fetch or parse, the error is logged and the previously
loaded tool set is kept, so a transient upstream blip never empties the server.
`--reload-every` is ignored when the document is loaded from a file. Note that
the server does not yet emit an MCP `tools/list_changed` notification, so a
connected client picks up the new tools on its next `tools/list` call.

#### OAuth for the document fetch

A static `--openapi-header` bearer token works for a one-shot fetch, but on a
long-running server it eventually expires and the reloads start failing. For
that case, authenticate the document fetch with an OAuth2 `client_credentials`
grant: the server obtains a token from the provider, caches it, and refreshes
it automatically shortly before expiry — so the periodic reload keeps working
indefinitely.

```bash
oas2mcp \
  --openapi-url https://api.example.com/openapi.json \
  --reload-every 1h \
  --openapi-oauth-token-url https://idp.example.com/oauth/token \
  --openapi-oauth-client-id "$CLIENT_ID" \
  --openapi-oauth-client-secret "$CLIENT_SECRET" \
  --openapi-oauth-scope read:openapi \
  --transport streamable-http \
  --bind-addr 0.0.0.0:8000
```

Client authentication uses HTTP Basic against the token endpoint (RFC 6749).
The OAuth bearer takes precedence over any static `Authorization` set via
`--openapi-header`. This auth covers the **document fetch only**; upstream API
calls still use `--header` / `--forward-header`.

##### Authenticating with a signed assertion instead of a secret

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

### OAuth for the upstream API

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
  --upstream-oauth-audience 'https://api.example.com'
```

This is configured independently of `--openapi-oauth-*`: the document and the
API may live behind different providers, with different credentials. Both
support the same two client-authentication modes, so
`--upstream-oauth-private-key` gives you `private_key_jwt` here too.

#### Which `Authorization` wins

Three things can set the upstream `Authorization`, so exactly one is picked —
the upstream never receives two:

| Priority | Source | Why it ranks there |
| --- | --- | --- |
| 1 | `--header 'Authorization: …'` | An explicit static override by the operator. |
| 2 | `--upstream-oauth-*` token | The managed credential. |
| 3 | `--forward-header Authorization` | The caller's own token, passed through. |

Every other forwarded header is unaffected — only `Authorization` is contested.

If the token cannot be obtained, the tool call **fails** and no request reaches
the API: proxying it unauthenticated would surface as a puzzling `401` from the
upstream rather than the real cause. The failure is logged with the provider's
own diagnosis and counted as `outcome="auth_error"` in the metrics, kept
distinct from an upstream error so a broken credential is not mistaken for a
broken API.

#### Acting on behalf of the caller

`client_credentials` gets one token for the server itself, so every tool call
reaches the API as the same principal. The upstream audit log shows one identity,
and the API can no longer apply per-user authorization — the only gate left is
`--oauth-role-mapper`, which filters *tool names*, not data. A `reader:^get`
rule lets `getAllCustomers` through for the intern as readily as for the CFO.

The `jwt-bearer` grant (RFC 7523 §2.1) fixes that: oas2mcp presents a signed
assertion naming the caller, and the provider issues a token *for that user*.

```bash
oas2mcp \
  --openapi-url https://api.example.com/openapi.json \
  --transport streamable-http --bind-addr 0.0.0.0:8000 \
  --oauth-jwks-url https://idp.example.com/.well-known/jwks.json \
  --oauth-role-mapper 'user:.*' \
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

##### Choosing the mode

| You want | Flags |
| --- | --- |
| One shared service identity | `--upstream-oauth-grant client-credentials` (the default) |
| A named service account | `--upstream-oauth-grant jwt-bearer --upstream-oauth-subject svc@example.com` |
| Per-caller delegation | `--upstream-oauth-grant jwt-bearer` (subject from the caller's claim) |
| Relay the caller's own token | `--upstream-oauth-grant jwt-bearer --upstream-oauth-assertion caller` |

A `self-signed` assertion needs `--upstream-oauth-private-key`: a shared secret
cannot sign one, and oas2mcp says so at startup rather than failing every call.
The same key signs both the client assertion (§2.2) and the grant assertion
(§2.1) — it is loaded once.

**The `self-signed` mode is a powerful credential.** The provider must be
configured to trust oas2mcp to assert those subjects, which makes that key, in
effect, "speak as anyone". Keep the provider's trust configuration as narrow as
it goes, scope the upstream token to the minimum, and treat the key accordingly.

The `caller` mode signs nothing and needs no key: the caller's verified JWT is
relayed as the assertion, so the provider trusts *their* issuer rather than us.
It is the cleanest option when it works, but it requires the caller's token to be
addressed (`aud`) to the authorization server, which most identity providers do
not do by default. When that is not the case, the mechanism you actually want is
RFC 8693 token exchange, which oas2mcp does not implement.

> Client authentication is still required alongside the `jwt-bearer` grant
> (`--upstream-oauth-client-secret` or `--upstream-oauth-private-key`).
> Providers that accept an assertion-only grant with no client authentication are
> not supported.

### Role-based tool access from the caller's JWT

The filters above are global: every MCP client sees the same tools. When the
server is shared by callers with different privileges, gate the tools on the
**caller's own JWT** instead. Set one or more `--oauth-role-mapper` entries of
the form `role:tool_name_regex`: a tool is visible (in `tools/list`) and
callable (in `tools/call`) only when one of the caller's roles maps to a regex
matching the tool name.

When a mapper is set, the incoming request's `Authorization: Bearer` JWT is
verified against a JWKS (`--oauth-jwks-url`, fetched once at startup, or
`--oauth-jwks-file`) and the roles are read from the `--oauth-role-claim` claim
(default `roles`; an array of strings or a whitespace-separated string). A
caller with no token, an invalid/expired token, or roles that match no mapping
sees and can call **no** tools.

```bash
oas2mcp \
  --openapi-url https://api.example.com/openapi.json \
  --transport streamable-http \
  --bind-addr 0.0.0.0:8000 \
  --oauth-jwks-url https://idp.example.com/.well-known/jwks.json \
  --oauth-role-claim roles \
  --oauth-role-mapper 'admin:.*' \
  --oauth-role-mapper 'reader:^get'
# admins get every tool; readers only the ones whose name starts with "get".
```

This needs the caller's JWT, which only the `streamable-http` transport
exposes — under `stdio`/`sse` no token is available, so every tool stays
hidden. The signature is verified with the key family advertised by the JWK
(an algorithm-substitution downgrade such as `HS256` against a public key is
rejected), and the token's `exp` is enforced. Invalid regexes are rejected at
startup. With multiple entries set through the environment variable, separate
them with newlines (e.g. `OAUTH_ROLE_MAPPER=$'admin:.*\nreader:^get'`).

#### Tracing the caller's JWT claims

Once JWTs are verified for role-based access, you can echo selected claims into
the logs to see *who* made each call. Pass one or more `--trace-claim` with the
claim names you care about; each one that the token actually carried is emitted
on the tool-call log line as a single `jwt.claims` field (a JSON object that
keeps every value's original shape — strings, numbers, arrays):

```bash
oas2mcp \
  --openapi-url https://api.example.com/openapi.json \
  --transport streamable-http \
  --bind-addr 0.0.0.0:8000 \
  --oauth-jwks-url https://idp.example.com/.well-known/jwks.json \
  --oauth-role-mapper 'admin:.*' \
  --trace-claim sub \
  --trace-claim email \
  --trace-claim tenant_id
# logs, per call: jwt.claims={"sub":"u-123","email":"a@b.com","tenant_id":42}
```

The claims come from the same verified JWT used for role mapping, so
`--trace-claim` only takes effect when `--oauth-role-mapper` (and a JWKS) is
configured. Claims go to the logs only — never to metric labels — so a
high-cardinality claim such as `sub` can't blow up your metrics backend.
With multiple names set through the environment variable, separate them with
newlines (e.g. `TRACE_CLAIMS=$'sub\nemail'`).

### Metrics

Every tool call is counted and timed and exposed as OpenTelemetry metrics:

| Instrument | Type | Description |
|------------|------|-------------|
| `mcp.tool.calls` | counter | Number of tool calls. |
| `mcp.tool.call.duration` | histogram (seconds) | Duration of the proxied upstream request. |

Both carry the attributes `tool` (the tool/operation name) and `outcome` — and
nothing else, so metric cardinality stays bounded. `outcome` is one of:

| Value | Meaning |
| --- | --- |
| `success` | The upstream answered with a non-error status. |
| `error` | The upstream answered with a 4xx/5xx, or the request could not be built or sent. |
| `auth_error` | The upstream OAuth token could not be obtained, so **no** request was made. Points at the provider or the credential, not at the API. |

To break activity down by caller, log the relevant JWT claims with
`--trace-claim` (see above) and aggregate them in your logging backend, rather
than turning a per-user identifier into a metric label.

Enable either exporter, both, or neither — they are independent:

```bash
# OTLP push to a collector + a Prometheus scrape endpoint, at once.
oas2mcp \
  --openapi-file ./examples/petstore.yaml \
  --transport streamable-http --bind-addr 0.0.0.0:8000 \
  --otlp-endpoint http://otel-collector:4318 \
  --metrics-addr 0.0.0.0:9090
# Push: POST http://otel-collector:4318/v1/metrics  (HTTP/protobuf, every 30s)
# Pull: GET  http://0.0.0.0:9090/metrics            (Prometheus text format)
```

The Prometheus endpoint runs on its own HTTP server (the `--metrics-addr`
address), separate from the MCP transport, so it works under `stdio` too. OTLP
honours the standard `OTEL_EXPORTER_OTLP_*` environment variables.

### Restricting the exposed operations

A large API turns into a huge tool set: GitLab's OpenAPI document defines ~1700
operations, whose `tools/list` payload is on the order of **half a million
tokens** — it does not fit a model's context, and most MCP clients choke well
before that. Use `--include`/`--exclude` (name globs),
`--include-regex`/`--exclude-regex` (name regexes) and `--tag`/`--exclude-tag`
(OpenAPI tags) to advertise only the operations you actually need.

An operation is kept when it passes **both** tests: it matches the allowlist
(any `--include` glob, any `--include-regex`, **or** any `--tag`; an empty
allowlist means "everything") and it does not match the denylist
(`--exclude` / `--exclude-regex` / `--exclude-tag`, which always win). Name
patterns match the tool name — the `operationId`, or the `<method>_<path>`
fallback. Globs support `*` (any run) and `?` (one character); regexes use the
[`regex`](https://docs.rs/regex) crate syntax (case-insensitive via a leading
`(?i)`) and are unanchored unless you anchor them with `^`/`$`.

```bash
# Expose only the Projects and Merge requests endpoints of GitLab:
oas2mcp \
  --openapi-url https://gitlab.com/gitlab-org/gitlab/-/raw/master/doc/api/openapi/openapi_v3.yaml \
  --tag Projects --tag 'Merge requests'
# ~1700 operations → 114 tools (a ~9× smaller tools/list)

# Or select by name and drop the deprecated ones:
oas2mcp --openapi-file api.yaml --include 'getApiV4Projects*' --exclude '*Deprecated'

# Read-only Projects/Groups endpoints, via a regex:
oas2mcp --openapi-file api.yaml --include-regex '^getApiV4(Projects|Groups)'
```

The startup log reports how many operations were kept versus filtered.

### Using it from an MCP client

For a stdio client (e.g. Claude Desktop / Claude Code), point it at the binary:

```json
{
  "mcpServers": {
    "petstore": {
      "command": "oas2mcp",
      "args": ["--openapi-file", "/abs/path/to/examples/petstore.yaml"]
    }
  }
}
```

For a remote client, start the `streamable-http` transport and connect it to
`http://<host>:<port>/mcp`.

## Deploy on Kubernetes (Helm)

A Helm chart is provided under [charts/oas2mcp](charts/oas2mcp). It deploys the
server with the `streamable-http` transport, a restricted security context, and
resource requests/limits. The upstream auth headers are stored in a `Secret`.

```bash
helm install petstore charts/oas2mcp \
  --set oas2mcp.openapi.url=https://petstore3.swagger.io/api/v3/openapi.json \
  --set-string 'oas2mcp.upstream.headers[0]=Authorization: Bearer <token>'
```

The OpenAPI document can come from a URL (`oas2mcp.openapi.url`) or be supplied
inline (`oas2mcp.openapi.inline`), in which case it is mounted from a
`ConfigMap`. To reuse an existing `Secret` for the upstream headers, set
`oas2mcp.upstream.existingSecret` (key `UPSTREAM_HEADERS`). See the chart's
[README](charts/oas2mcp/README.md) for every value.

To trust a private/corporate CA for outbound TLS, either drop the PEM bundle
into `oas2mcp.caCerts.inline` (stored in a `Secret`, mounted, and wired to
`CA_CERT_FILE` automatically), or mount it from a resource you already manage
via `oas2mcp.caCerts.existing` (`kind: ConfigMap` or `Secret` — a `ConfigMap`
is the natural home for public CA certs):

```bash
# inline PEM → generated Secret
helm install petstore charts/oas2mcp \
  --set oas2mcp.openapi.url=https://internal.example.com/openapi.json \
  --set-file oas2mcp.caCerts.inline=./corp-ca.pem

# or reference an existing ConfigMap
helm install petstore charts/oas2mcp \
  --set oas2mcp.openapi.url=https://internal.example.com/openapi.json \
  --set oas2mcp.caCerts.existing.kind=ConfigMap \
  --set oas2mcp.caCerts.existing.name=corp-ca
```

## How operations map to tools

Given this operation:

```yaml
paths:
  /pet/{petId}:
    get:
      operationId: getPetById
      parameters:
        - { name: petId, in: path, required: true, schema: { type: integer } }
```

`oas2mcp` advertises a `getPetById` tool whose input schema requires a `petId`
property. Calling it with `{ "petId": 1 }` issues `GET <base-url>/pet/1` and
returns the upstream response (status line followed by the body). A non-2xx
upstream status is surfaced as an MCP tool error.

## Run the tests

```bash
cargo test
```

## Development

Install the git hooks and run all checks:

```bash
pre-commit install
pre-commit run --all-files
```

This runs `cargo fmt --check`, `cargo clippy -D warnings`, `hadolint`,
`actionlint`, `shellcheck`, `helm lint`, `helm-docs`, and the standard
whitespace/merge hooks. The hooks call the real binaries, so they need to be on
`PATH`: `hadolint`, `actionlint`, `shellcheck`, `helm` and `helm-docs`
(`brew install hadolint actionlint shellcheck helm norwoodj/tap/helm-docs`).

### CI / Release

GitHub Actions workflows:

- **quality** — runs `pre-commit`, the test suite, and a Trivy scan of the
  repository on every push to `main` and every pull request.
- **build** — builds the container image (multi-arch on native runners) and
  scans it with Trivy; pushes to `ghcr.io/therealm-tech/oas2mcp` only on manual
  dispatch or from a release.
- **chart** — publishes the Helm chart as an OCI artifact to
  `ghcr.io/therealm-tech/charts`, triggered by a `chart-X.Y.Z` tag (or manual
  dispatch). The chart is versioned and released independently of the app.
- **release** — triggered by pushing a `vX.Y.Z` tag: checks that the
  `Cargo.toml` version matches the tag, builds and pushes the image (versioned
  from the tag) and creates a GitHub Release with auto-generated notes. A
  mismatch fails the job before anything is published — the tag names the image
  but `Cargo.toml` is what `oas2mcp --version` reports, so the two must agree.

### Security scanning

[Trivy](https://trivy.dev) runs in two places, and both fail the build on a
**HIGH** or **CRITICAL** finding that has a fix available:

- **quality / trivy** — a filesystem scan of the repository: crate advisories
  from `Cargo.lock`, leaked secrets, and `Dockerfile` and Helm chart
  misconfiguration.
- **build / scan the image** — scans the container image the commit actually
  produces, which is what catches CVEs in the base layer. This runs on releases
  too: a HIGH/CRITICAL finding fails the build, which blocks the `manifest`
  job, so no usable tag is ever published. Note it covers the base layer only —
  the runtime image holds a compiled binary, so Trivy sees no Rust dependencies
  there; those are covered by the `Cargo.lock` scan above.

The runtime image is `gcr.io/distroless/cc-debian12:nonroot`: no shell, no
package manager, no OS package layer to speak of, so a scanner finds next to
nothing to flag. That is a deliberate move away from `debian:bookworm-slim`,
which carried around twenty unfixed HIGH/CRITICAL advisories at any time.

Each runs twice, deliberately: once reporting **every** severity to the
repository's **Security** tab, then once more gating the build on HIGH and
CRITICAL. Advisories with no released fix are excluded from both.

Trivy renders the chart itself, but only when handed the values its templates
require (`TRIVY_HELM_VALUES`). Without them it logs a render error, scans no
chart at all, and still reports success — so keep that variable set.

Reproduce either scan locally:

```bash
# What the quality workflow gates on:
TRIVY_HELM_VALUES=charts/oas2mcp/values-lint.yaml \
  trivy fs . --scanners vuln,secret,misconfig \
    --severity HIGH,CRITICAL --ignore-unfixed \
    --skip-files tests/fixtures/test_rsa_key.pem

# What the build workflow gates on, against a locally built image:
docker build -t oas2mcp:dev .
trivy image oas2mcp:dev --severity HIGH,CRITICAL --ignore-unfixed
```

### Cutting a release

The app and the chart have separate release lifecycles.

The helper script bumps the version files, runs the checks, commits, tags and
pushes — which is what triggers the workflows. Release either side, or both at
once:

```bash
# The application: bumps Cargo.toml + Cargo.lock, tags v0.4.0.
scripts/release.sh 0.4.0

# The chart: bumps Chart.yaml + the generated chart README, tags chart-0.5.0.
scripts/release.sh --chart 0.5.0

# Both: as above, and `appVersion` is pointed at the app version being
# released, since the chart now targets that image.
scripts/release.sh 0.4.0 --chart 0.5.0
```

A chart-only release points `appVersion` at the latest `vX.Y.Z` tag, so a chart
published on its own still ships against the newest app image instead of
quietly lagging behind it. The script refuses to run on a dirty tree, off
`main`, out of sync with `origin/main`, or when a tag already exists. Useful
flags: `--skip-tests`, `--no-push` (commit and tag locally only), `-y` (no
confirmation prompt). Bumping the chart needs `helm` and `helm-docs` on `PATH`.

Doing it by hand works too, as long as `Cargo.toml` already carries the same
version — otherwise the `release` workflow fails the version check:

```bash
# Release the application (image + GitHub Release):
git tag v0.1.0 && git push origin v0.1.0

# Release the Helm chart (OCI push), independently:
git tag chart-0.1.0 && git push origin chart-0.1.0
```

## Limitations

- OpenAPI **3.0.x** and **3.1.x** are supported. A newer 3.x revision is read
  on a best-effort basis (as 3.1) with a warning; Swagger 2.0 is rejected —
  convert it first, e.g. with `swagger2openapi`.
- Only **local** `$ref`s are resolved. A reference into another file or a URL
  is not fetched; it degrades to a bare `object` in the tool's input schema.
- Request bodies are always sent as JSON. An operation whose `content` offers
  no `application/json` (or `…+json`) media type still gets a `body` argument,
  built from the first media type it does declare.
- OpenAPI 3.1 `webhooks` are not exposed as tools: a webhook is a callback the
  upstream API sends *to* the server, not an operation the server can call.
- Cookie parameters are ignored.
- Templated `servers` URLs (`https://{region}.example.com`) are not expanded;
  pass `--base-url` for those.
- The legacy `sse` transport is kept for compatibility but is deprecated by the
  MCP specification; prefer `streamable-http` for new remote deployments.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
