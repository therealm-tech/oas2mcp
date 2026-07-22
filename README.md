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
- **Periodic reload** — with `--reload-every`, a document loaded from a URL is
  re-fetched on an interval and the exposed tool set is rebuilt in place,
  without restarting the server. The fetch can authenticate via OAuth2
  `client_credentials` (auto-refreshed token), so reloads keep working on a
  long-running server where a static token would expire.
- **One tool per operation** — `operationId` becomes the tool name (falling
  back to `<method>_<path>`); path, query and header parameters become
  top-level tool arguments, and a JSON request body is passed as a `body`
  argument. Local `$ref`s are inlined into each tool's input schema.
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
| `--openapi-oauth-token-url` | `OPENAPI_OAUTH_TOKEN_URL` | — | OAuth2 `client_credentials` token endpoint. Set → the document fetch uses an auto-refreshed bearer token. Requires the client id/secret below. |
| `--openapi-oauth-client-id` | `OPENAPI_OAUTH_CLIENT_ID` | — | OAuth2 client ID for the document-fetch token.                     |
| `--openapi-oauth-client-secret` | `OPENAPI_OAUTH_CLIENT_SECRET` | — | OAuth2 client secret. Prefer the env var so it stays out of the process list. |
| `--openapi-oauth-scope` | `OPENAPI_OAUTH_SCOPES` | —          | OAuth2 scope requested (sent space-joined). Repeatable; newline-separated via the env var. |
| `--openapi-oauth-audience` | `OPENAPI_OAUTH_AUDIENCE` | —    | OAuth2 `audience` parameter, when the provider requires it (e.g. Auth0). |
| `--base-url`      | `BASE_URL`       | spec `servers`   | Upstream API base URL that tool calls are proxied to.              |
| `--ca-cert`       | `CA_CERT_FILE`   | —                | Path to a PEM file with extra CA certificate(s) to trust for every outbound TLS connection (upstream, document fetch, OAuth, JWKS). Added on top of the built-in roots, so only your private/corporate CA is needed. Repeatable; newline-separated via the env var. |
| `--header`        | `UPSTREAM_HEADERS` | —              | Extra `Name: Value` header on every upstream request. Repeatable.  |
| `--forward-header`| `FORWARD_HEADERS`  | —              | Name of an incoming request header to forward upstream (e.g. `Authorization`). Repeatable. `streamable-http` only. |
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

Both carry the attributes `tool` (the tool/operation name) and `outcome`
(`success` or `error`) — and nothing else, so metric cardinality stays bounded.
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
`actionlint`, `helm lint`, `helm-docs`, and the standard whitespace/merge hooks.

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
- **release** — triggered by pushing a `vX.Y.Z` tag: builds and pushes the
  image (versioned from the tag) and creates a GitHub Release with
  auto-generated notes.

### Security scanning

[Trivy](https://trivy.dev) runs in two places, and both fail the build on a
**HIGH** or **CRITICAL** finding that has a fix available:

- **quality / trivy** — a filesystem scan of the repository: crate advisories
  from `Cargo.lock`, leaked secrets, and `Dockerfile` misconfiguration. The
  Helm chart is rendered with `helm template` first, because Trivy cannot
  evaluate Go-templated manifests.
- **build / scan the image** — scans the container image the commit actually
  produces, which is what catches CVEs in the `debian:bookworm-slim` base.

Findings are uploaded to the repository's **Security** tab. Advisories with no
released fix are reported but do not fail the build — the base image carries
around twenty of them at any time and none are actionable here.

Reproduce either scan locally:

```bash
# What the quality workflow runs:
trivy fs . --scanners vuln,secret,misconfig \
  --severity HIGH,CRITICAL --ignore-unfixed \
  --skip-files tests/fixtures/test_rsa_key.pem

# What the build workflow runs, against a locally built image:
docker build -t oas2mcp:dev .
trivy image oas2mcp:dev --severity HIGH,CRITICAL --ignore-unfixed
```

The app and the chart have separate release lifecycles:

```bash
# Release the application (image + GitHub Release):
git tag v0.1.0 && git push origin v0.1.0

# Release the Helm chart (OCI push), independently:
git tag chart-0.1.0 && git push origin chart-0.1.0
```

## Limitations

- OpenAPI **3.0.x** is supported (via [`openapiv3`](https://docs.rs/openapiv3));
  3.1-only documents may not parse.
- Only `application/json` request bodies are proxied.
- Cookie parameters are ignored.
- The legacy `sse` transport is kept for compatibility but is deprecated by the
  MCP specification; prefer `streamable-http` for new remote deployments.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
