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

- **Input from a file or a URL** — the document is fetched/read once at
  startup, in JSON or YAML.
- **One tool per operation** — `operationId` becomes the tool name (falling
  back to `<method>_<path>`); path, query and header parameters become
  top-level tool arguments, and a JSON request body is passed as a `body`
  argument. Local `$ref`s are inlined into each tool's input schema.
- **Three transports** — the MCP server can be exposed over:
  - `stdio` — for a local subprocess MCP client.
  - `streamable-http` — the current remote transport, single `POST /mcp`
    endpoint.
  - `sse` — the legacy HTTP+SSE transport (deprecated by the MCP spec, kept
    for compatibility with older clients).
- **Auth passthrough** — attach arbitrary static headers (e.g. a bearer token)
  to every upstream request, or forward the MCP client's own request headers
  (e.g. `Authorization`) upstream per call (`streamable-http` only).
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
| `--openapi-url`   | `OPENAPI_URL`    | —                | URL of an OpenAPI document fetched once at startup.                |
| `--base-url`      | `BASE_URL`       | spec `servers`   | Upstream API base URL that tool calls are proxied to.              |
| `--header`        | `UPSTREAM_HEADERS` | —              | Extra `Name: Value` header on every upstream request. Repeatable.  |
| `--forward-header`| `FORWARD_HEADERS`  | —              | Name of an incoming request header to forward upstream (e.g. `Authorization`). Repeatable. `streamable-http` only. |
| `--include`       | `INCLUDE_OPERATIONS` | —            | Only expose operations whose name matches this glob (`*`/`?`). Repeatable. |
| `--exclude`       | `EXCLUDE_OPERATIONS` | —            | Drop operations whose name matches this glob. Repeatable. Wins over `--include`/`--tag`. |
| `--include-regex` | `INCLUDE_OPERATIONS_REGEX` | —      | Only expose operations whose name matches this regex. Repeatable. |
| `--exclude-regex` | `EXCLUDE_OPERATIONS_REGEX` | —      | Drop operations whose name matches this regex. Repeatable. Wins over the allowlist. |
| `--tag`           | `INCLUDE_TAGS`   | —                | Only expose operations carrying this OpenAPI tag (case-insensitive). Repeatable. |
| `--exclude-tag`   | `EXCLUDE_TAGS`   | —                | Drop operations carrying this OpenAPI tag (case-insensitive). Repeatable. Wins over the allowlist. |
| `--transport`     | `TRANSPORT`      | `stdio`          | One of `stdio`, `sse`, `streamable-http`.                          |
| `--bind-addr`     | `BIND_ADDR`      | `127.0.0.1:8000` | Bind address for the `sse` and `streamable-http` transports.       |
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

- **quality** — runs `pre-commit` and the test suite on every push to `main` and
  every pull request.
- **build** — builds the container image (multi-arch on native runners); pushes
  to `ghcr.io/therealm-tech/oas2mcp` only on manual dispatch or from a release.
- **chart** — publishes the Helm chart as an OCI artifact to
  `ghcr.io/therealm-tech/charts`, triggered by a `chart-X.Y.Z` tag (or manual
  dispatch). The chart is versioned and released independently of the app.
- **release** — triggered by pushing a `vX.Y.Z` tag: builds and pushes the
  image (versioned from the tag) and creates a GitHub Release with
  auto-generated notes.

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
