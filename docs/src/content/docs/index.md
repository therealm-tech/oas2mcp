---
title: oas2mcp
description: Expose every operation of an OpenAPI document as a tool of an MCP server.
template: splash
hero:
  tagline: Load an OpenAPI document and expose every operation it describes as a tool of a Model Context Protocol server — without writing a line of glue code.
  image:
    file: ../../../../logo.svg
  actions:
    - text: Get started
      link: /getting-started/
      icon: right-arrow
    - text: View on GitHub
      link: https://github.com/therealm-tech/oas2mcp
      icon: external
      variant: minimal
---

Each [OpenAPI](https://www.openapis.org/) operation becomes one
[MCP](https://modelcontextprotocol.io/) tool. When a client calls the tool,
`oas2mcp` builds and sends the corresponding HTTP request to the upstream API
and returns the response. In other words, it turns any HTTP API that ships an
OpenAPI description into something an MCP-capable agent can drive.

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
- **[Periodic reload](/guides/document/)** — with `--reload-every`, a
  document loaded from a URL is re-fetched on an interval and the exposed tool
  set is rebuilt in place, without restarting the server. The fetch can
  authenticate via OAuth2 `client_credentials` (auto-refreshed token), so
  reloads keep working on a long-running server where a static token would
  expire. The client authenticates with either a shared secret or a signed JWT
  assertion (`private_key_jwt`, RFC 7523 §2.2), for providers that will not
  issue a secret.
- **[One tool per operation](/reference/tools/)** — `operationId`
  becomes the tool name (falling back to `<method>_<path>`); path, query and
  header parameters become top-level tool arguments, and a JSON request body is
  passed as a `body` argument. Local `$ref`s are inlined into each tool's input
  schema, whatever they point at (`#/components/schemas/…`, `#/$defs/…`, …); a
  recursive schema collapses to a bare object rather than expanding forever.
- **[Readable tool names](/guides/operations/#renaming-the-exposed-tools)**
  — rewrite the names an OpenAPI document produces with chained `--rename` regex
  rules, and cap their length with `--max-name-len` (64 by default, the limit
  Anthropic and OpenAI enforce). GitLab's
  `postApiV4ProjectsIdMergeRequestsNoteableIdDiscussionsDiscussionIdNotes`
  becomes `post_projmrdiscNotes`, which fits under a gateway prefix and is far
  easier for a model to pick. Filters keep matching the original `operationId`.
- **[Behaviour hints](/reference/tools/#tool-annotations)** — each tool
  carries the MCP annotations its HTTP method implies (`readOnlyHint` on a
  `GET`, `destructiveHint` on a `DELETE`, …), so a client can tell a read from a
  write.
- **Three transports** — the MCP server can be exposed over:
  - `stdio` — for a local subprocess MCP client.
  - `http` — the current remote transport, single `POST /mcp` endpoint. By
    default each request is answered with a single `application/json` body
    (stateless), which is the most interoperable mode — notably with strict
    proxies such as Envoy AI Gateway. Pass `--stream-responses` to reply with a
    `text/event-stream` (SSE) flow and keep stateful sessions instead.
  - `sse` — the legacy HTTP+SSE transport (deprecated by the MCP spec, kept for
    compatibility with older clients).
- **[Auth passthrough](/guides/upstream-auth/#static-and-forwarded-headers)**
  — attach arbitrary static headers (e.g. a bearer token) to every upstream
  request, or forward the MCP client's own request headers (e.g.
  `Authorization`) upstream per call (`http` only).
- **[OAuth for the upstream API](/guides/upstream-auth/)** — obtain the
  upstream `Authorization: Bearer` from an OAuth2 grant, refreshed
  automatically before it expires, instead of a static token that goes stale.
  Authenticates with a client secret or a signed JWT assertion (RFC 7523 §2.2),
  and is configured independently of the document-fetch grant.
- **[Acting on behalf of the caller](/guides/upstream-auth/#acting-on-behalf-of-the-caller)**
  — with the `jwt-bearer` grant (RFC 7523 §2.1), obtain a *per-caller* upstream
  token from the identity in their verified JWT, so the upstream API sees who is
  really acting and applies its own authorization, instead of every call
  arriving as one shared service account.
- **[Caller authentication and role-based tool access](/guides/caller-auth/)**
  — verify the caller's JWT against a JWKS, and optionally gate which tools they
  can see and call by mapping each `role` to a regex over operation names
  (`http` only). Tools mapped to the reserved role `*` stay open to everyone,
  token or not.
- **[MCP authorization discovery](/guides/caller-auth/#letting-mcp-clients-find-the-authorization-server)**
  — with `--inbound-resource`, `/mcp` behaves as the OAuth protected resource
  the MCP authorization spec describes: a request without a valid token is
  answered `401` with a `WWW-Authenticate` challenge, and the Protected Resource
  Metadata (RFC 9728) tells the client which authorization server to log in
  with.
- **[JWT claim tracing](/guides/caller-auth/#tracing-the-callers-jwt-claims)**
  — with `--trace-claim`, echo selected claims from the verified token (e.g.
  `sub`, `email`, `tenant_id`) onto each tool-call log line to see who made each
  call, without inflating metric cardinality.
- **[OpenTelemetry metrics](/guides/metrics/)** — count and time every
  tool call, labelled by tool and outcome (kept low-cardinality), exported over
  OTLP and/or a Prometheus `/metrics` endpoint.
- **Custom CA trust** — point `--ca-cert` at a PEM bundle to trust a private or
  corporate CA for every outbound TLS connection (upstream API, document fetch,
  OAuth, JWKS), on top of the built-in public roots.
- **Graceful shutdown** on `SIGTERM`/`SIGINT`.
