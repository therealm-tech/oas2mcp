---
title: Getting started
description: Build oas2mcp, run it against an OpenAPI document and connect an MCP client.
---

## Install

Requires `rustup`: the toolchain version is pinned in
[`rust-toolchain.toml`](https://github.com/therealm-tech/oas2mcp/blob/main/rust-toolchain.toml)
and installed on the first `cargo` invocation.

```bash
git clone https://github.com/therealm-tech/oas2mcp.git
cd oas2mcp
cargo build --release
# binary at target/release/oas2mcp
```

Or with Docker:

```bash
docker build -t oas2mcp .
```

To run it on a cluster, see [Deploying on Kubernetes](/oas2mcp/guides/kubernetes/).

## Usage

```text
oas2mcp [OPTIONS] [stdio | sse | http] [TRANSPORT OPTIONS]
oas2mcp healthcheck [--bind-addr ADDR]
```

The subcommand picks the transport, `stdio` when none is given. Options for
one transport only exist under its subcommand: `--bind-addr` under `sse` and
`http`, and `--allowed-host`, `--stream-responses`, `--forward-header` and every
`--inbound-*` flag under `http` alone. Every other option may come before or
after the subcommand.

`healthcheck` serves nothing: it exits 0 when something accepts TCP connections
on `--bind-addr` (a wildcard address is probed on loopback), 1 otherwise. It is
the container image's `HEALTHCHECK`.

The OpenAPI source is required to serve: pass exactly one of `--openapi-file` or
`--openapi-url`. Every option is listed in the
[configuration reference](/oas2mcp/reference/configuration/).

## Examples

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
oas2mcp http \
  --openapi-url https://api.example.com/openapi.json \
  --bind-addr 0.0.0.0:8000 \
  --header 'Authorization: Bearer <token>'
# MCP endpoint: POST http://0.0.0.0:8000/mcp
```

Serve over the legacy SSE transport:

```bash
oas2mcp sse --openapi-file examples/petstore.yaml
# SSE stream:   GET  http://127.0.0.1:8000/sse
# Client posts: POST http://127.0.0.1:8000/messages?sessionId=<id>
```

## Using it from an MCP client

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

For a remote client, start the `http` transport and connect it to
`http://<host>:<port>/mcp`.
