---
title: Configuration
description: Every option, its environment variable and its default.
sidebar:
  order: 1
---

Three families of flags configure three directions of authentication:
`--inbound-*` how callers authenticate to oas2mcp, `--upstream-oauth-*` how
oas2mcp authenticates to the API, and `--openapi-oauth-*` how it fetches the
document.

| Option            | Env              | Default          | Description                                                        |
| ----------------- | ---------------- | ---------------- | ------------------------------------------------------------------ |
| `--openapi-file`  | `OPENAPI_FILE`   | —                | Path to an OpenAPI document (JSON or YAML) on disk.                |
| `--openapi-url`   | `OPENAPI_URL`    | —                | URL of an OpenAPI document fetched at startup (and on each reload).|
| `--openapi-header`| `OPENAPI_HEADERS`| —                | `Name: Value` header sent when fetching `--openapi-url` (e.g. for a private document). Repeatable. |
| `--openapi-auth`  | `OPENAPI_AUTH`   | `own`            | Credentials for the document fetch: `own` (`--openapi-header`, `--openapi-oauth-*`) or `upstream` (`--header` and the `--upstream-oauth-*` token). |
| `--reload-every`  | `RELOAD_EVERY`   | —                | Re-fetch `--openapi-url` on this interval and rebuild the tool set (e.g. `30s`, `5m`, `1h`). Off by default; ignored for a file source. |
| `--openapi-resource` | `OPENAPI_RESOURCE` | `false`       | Expose the OpenAPI document as the MCP resource `openapi://document`, cut down per caller to the operations it can list — see [Reading the API contract](/guides/openapi-resource/). |
| `--openapi-oauth-token-url` | `OPENAPI_OAUTH_TOKEN_URL` | — | OAuth2 `client_credentials` token endpoint. Set → the document fetch uses an auto-refreshed bearer token. Requires `--openapi-oauth-client-id` plus one of the two credentials below. |
| `--openapi-oauth-client-id` | `OPENAPI_OAUTH_CLIENT_ID` | — | OAuth2 client ID for the document-fetch token.                     |
| `--openapi-oauth-client-secret` | `OPENAPI_OAUTH_CLIENT_SECRET` | — | OAuth2 client secret, sent over HTTP Basic. Prefer the env var so it stays out of the process list. Mutually exclusive with `--openapi-oauth-private-key`. |
| `--openapi-oauth-private-key` | `OPENAPI_OAUTH_PRIVATE_KEY_FILE` | — | Path to a PKCS#8 PEM private key. Set → the client authenticates with a signed JWT assertion (`private_key_jwt`, RFC 7523 §2.2) instead of a secret. Mutually exclusive with `--openapi-oauth-client-secret`. |
| `--openapi-oauth-key-id` | `OPENAPI_OAUTH_KEY_ID` | —              | `kid` header on the client assertion, when the provider has several keys registered for the client. Needs `--openapi-oauth-private-key`. |
| `--openapi-oauth-signing-alg` | `OPENAPI_OAUTH_SIGNING_ALG` | `rs256` | Assertion signature algorithm: `rs256`/`rs384`/`rs512`, `ps256`/`ps384`/`ps512`, `es256`/`es384`, `eddsa`. Must match the key type. |
| `--openapi-oauth-assertion-audience` | `OPENAPI_OAUTH_ASSERTION_AUDIENCE` | token endpoint | `aud` claim of the client assertion. Override when the provider expects its issuer identifier rather than the token endpoint URL. |
| `--openapi-oauth-assertion-lifetime` | `OPENAPI_OAUTH_ASSERTION_LIFETIME` | `60s` | How long a client assertion stays valid (e.g. `30s`, `2m`). |
| `--openapi-oauth-scope` | `OPENAPI_OAUTH_SCOPES` | —          | OAuth2 scope requested (sent space-joined). Repeatable; newline-separated via the env var. |
| `--openapi-oauth-token-audience` | `OPENAPI_OAUTH_TOKEN_AUDIENCE` | —    | OAuth2 `audience` parameter, when the provider requires it (e.g. Auth0). |
| `--base-url`      | `BASE_URL`       | spec `servers`   | Upstream API base URL that tool calls are proxied to.              |
| `--ca-cert`       | `CA_CERT_FILE`   | —                | Path to a PEM file with extra CA certificate(s) to trust for every outbound TLS connection (upstream, document fetch, OAuth, JWKS). Added on top of the built-in roots, so only your private/corporate CA is needed. Repeatable; newline-separated via the env var. |
| `--header`        | `UPSTREAM_HEADERS` | —              | Extra `Name: Value` header on every upstream request. Repeatable.  |
| `--forward-header`| `FORWARD_HEADERS`  | —              | Name of an incoming request header to forward upstream (e.g. `Authorization`). Repeatable. `http` only. |
| `--upstream-oauth-token-url` | `UPSTREAM_OAUTH_TOKEN_URL` | — | OAuth2 `client_credentials` token endpoint for **upstream API calls**. Set → every proxied call carries an auto-refreshed bearer. Requires `--upstream-oauth-client-id` plus one credential below. |
| `--upstream-oauth-client-id` | `UPSTREAM_OAUTH_CLIENT_ID` | —  | OAuth2 client ID for the upstream token.                           |
| `--upstream-oauth-client-secret` | `UPSTREAM_OAUTH_CLIENT_SECRET` | — | OAuth2 client secret, sent over HTTP Basic. Mutually exclusive with `--upstream-oauth-private-key`. |
| `--upstream-oauth-private-key` | `UPSTREAM_OAUTH_PRIVATE_KEY_FILE` | — | PKCS#8 PEM key: authenticate with a signed JWT assertion (RFC 7523 §2.2) instead of a secret. |
| `--upstream-oauth-key-id` | `UPSTREAM_OAUTH_KEY_ID` | —          | `kid` header on the upstream client assertion. Needs the private key. |
| `--upstream-oauth-signing-alg` | `UPSTREAM_OAUTH_SIGNING_ALG` | `rs256` | Assertion signature algorithm. Must match the key type. |
| `--upstream-oauth-assertion-audience` | `UPSTREAM_OAUTH_ASSERTION_AUDIENCE` | token endpoint | `aud` claim of the upstream client assertion. |
| `--upstream-oauth-assertion-lifetime` | `UPSTREAM_OAUTH_ASSERTION_LIFETIME` | `60s` | Upstream client assertion validity window. |
| `--upstream-oauth-scope` | `UPSTREAM_OAUTH_SCOPES` | —          | OAuth2 scope requested for the upstream token. Repeatable; newline-separated via the env var. |
| `--upstream-oauth-token-audience` | `UPSTREAM_OAUTH_TOKEN_AUDIENCE` | —      | OAuth2 `audience` parameter for the upstream token (e.g. Auth0). |
| `--upstream-oauth-grant` | `UPSTREAM_OAUTH_GRANT` | `client-credentials` | `client-credentials`; `jwt-bearer` (RFC 7523 §2.1) to obtain the token on behalf of a subject with an assertion oas2mcp signs; or `jwt-bearer-relay` to relay the caller's own JWT as that assertion. |
| `--upstream-oauth-assertion-issuer` | `UPSTREAM_OAUTH_ASSERTION_ISSUER` | client id | `iss` of the `jwt-bearer` assertion, identifying oas2mcp to the provider. |
| `--upstream-oauth-subject` | `UPSTREAM_OAUTH_SUBJECT` | —      | Fixed `sub` for the assertion — a service account. Every caller shares one token. Mutually exclusive with the claim below. |
| `--upstream-oauth-subject-claim` | `UPSTREAM_OAUTH_SUBJECT_CLAIM` | `sub` | Claim of the **caller's** verified JWT whose value becomes the assertion's `sub`. Needs a JWKS (`--inbound-jwks-url`/`--inbound-jwks-file`) and `http`. |
| `--inbound-role-mapper` | `INBOUND_ROLE_MAPPER` | —          | `role:operation_regex` mapping that gates tool visibility/invocation on the caller's JWT roles. Repeatable. Unset → any authenticated caller may use every tool. Requires a JWKS source below. |
| `--inbound-anonymous-discovery` | `INBOUND_ANONYMOUS_DISCOVERY` | `false` | A caller without a valid token lists **every** tool (to let a service discover the catalogue) but still calls the public ones only. Requires a JWKS source below. |
| `--inbound-jwks-url` | `INBOUND_JWKS_URL` | —              | URL of a JWKS document (fetched at startup) used to verify incoming JWTs. Set (or `--inbound-jwks-file`) → callers are authenticated from their JWT; without a valid one they get the public tools only. `http` only. |
| `--inbound-jwks-file` | `INBOUND_JWKS_FILE` | —            | Path to a JWKS document on disk. Mutually exclusive with `--inbound-jwks-url`. |
| `--inbound-expected-audience` | `INBOUND_EXPECTED_AUDIENCES` | — | Audience the incoming JWT's `aud` must match. Repeatable. **Set this**: unset, a token your provider minted for another service is accepted here. |
| `--inbound-expected-issuer` | `INBOUND_EXPECTED_ISSUERS` | —      | Issuer the incoming JWT's `iss` must match. Repeatable. Defence in depth next to the JWKS. |
| `--inbound-clock-skew` | `INBOUND_CLOCK_SKEW` | `60s`            | Skew tolerated on the incoming JWT's `exp`/`nbf` (e.g. `30s`, `2m`). |
| `--inbound-resource` | `INBOUND_RESOURCE` | —              | Canonical URL clients reach `/mcp` under. Set → unauthenticated requests (beyond the public tools) get a `401` challenge pointing at the Protected Resource Metadata (RFC 9728), so MCP clients discover the authorization server themselves. Needs `--inbound-role-mapper` and `--inbound-expected-issuer`. `http` only. |
| `--inbound-role-claim` | `INBOUND_ROLE_CLAIM` | `roles`    | JWT claim listing the caller's roles (array of strings, or a whitespace-separated string). |
| `--trace-claim`   | `TRACE_CLAIMS`   | —                | JWT claim name to log on each tool call as a `jwt.claims` field (e.g. `sub`, `email`, `tenant_id`). Repeatable; newline-separated via the env var. Logged only, never a metric label. Needs a JWKS. |
| `--include-regex` | `INCLUDE_OPERATIONS_REGEX` | —      | Only expose operations whose name matches this regex. Repeatable. |
| `--exclude-regex` | `EXCLUDE_OPERATIONS_REGEX` | —      | Drop operations whose name matches this regex. Repeatable. Wins over the allowlist. |
| `--tag`           | `INCLUDE_TAGS`   | —                | Only expose operations carrying this OpenAPI tag (case-insensitive). Repeatable. |
| `--exclude-tag`   | `EXCLUDE_TAGS`   | —                | Drop operations carrying this OpenAPI tag (case-insensitive). Repeatable. Wins over the allowlist. |
| `--rename`        | `RENAME_OPERATIONS` | —             | Rewrite tool names, as `<regex>=<replacement>` (split on the first `=`). Repeatable; rules chain in order. Applied **after** filtering. |
| `--max-name-len`  | `MAX_NAME_LEN`   | `64`             | Maximum tool name length. A longer name is truncated and given a short hash of the full name, and the rewrite is logged. |
| `--auto-tool-annotations` | `AUTO_TOOL_ANNOTATIONS` | `true` | Advertise each tool with the MCP behaviour hints its HTTP method implies — see [Tool annotations](/reference/tools/#tool-annotations). Turn off with `--auto-tool-annotations=false`. |
| `--tools-page-size` | `TOOLS_PAGE_SIZE` | —            | Maximum number of tools per `tools/list` reply, walked with the MCP cursor. Unset → every tool in one reply, since many clients read only the first page. A cursor issued before a reload that changed the tool set is refused (`-32602`); the client lists again from the start. |
| `--tool-output-schema` | `TOOL_OUTPUT_SCHEMA` | `false`   | Declare an MCP `outputSchema` on each tool whose success response has a JSON object body. See [Output schemas](/reference/tools/#output-schemas) for the trade-off. |
| `--otlp-endpoint` | `OTEL_EXPORTER_OTLP_ENDPOINT` | — | Base OTLP endpoint to push tool-call metrics to over HTTP (e.g. `http://localhost:4318`); `/v1/metrics` is appended. Set → OTLP export on. |
| `--metrics-addr`  | `METRICS_ADDR`   | —                | Address to serve a Prometheus `/metrics` endpoint on (e.g. `0.0.0.0:9090`). Set → scrape endpoint on. Independent of `--otlp-endpoint`. |
| `--otel-service-name` | `OTEL_SERVICE_NAME` | `oas2mcp`   | `service.name` reported on exported metrics.                       |
| `--bind-addr`     | `BIND_ADDR`      | `127.0.0.1:8000` | Bind address of the `sse` and `http` subcommands, and the one `healthcheck` probes. |
| `--allowed-host`  | `ALLOWED_HOSTS`  | follows `--bind-addr` | Hostname, or `host:port`, accepted in the inbound `Host` header; `*` accepts any. Repeatable; newline-separated via the env var. `http` only — see [Host header validation](/guides/http-transport/#host-header-validation). |
| `--stream-responses` | `STREAM_RESPONSES` | `false`      | Reply on `http` with an SSE flow and stateful sessions instead of the default single `application/json` body. `http` only. |
| `--log-filter`    | `LOG_FILTER`     | `info`           | `tracing` filter directive (e.g. `oas2mcp=debug,rmcp=warn`).       |

Configuration resolves CLI flags → environment variables → defaults, and every
option is settable through its environment variable. When the base URL is not
passed explicitly, the first absolute entry of the document's `servers` list is
used.
