<p align="center">
  <img src="https://raw.githubusercontent.com/therealm-tech/oas2mcp/main/logo.svg"
       alt="oas2mcp logo" width="180">
</p>

# oas2mcp

Load an [OpenAPI](https://www.openapis.org/) document at startup and expose
every operation it describes as a tool of a
[Model Context Protocol (MCP)](https://modelcontextprotocol.io/) server.

**Documentation: <https://oas2mcp.therealm.tech/>**

## Description

Each OpenAPI operation becomes one MCP tool. When a client calls the tool,
`oas2mcp` builds and sends the corresponding HTTP request to the upstream API
and returns the response. In other words, it turns any HTTP API that ships an
OpenAPI description into something an MCP-capable agent can drive — without
writing a line of glue code.

It reads OpenAPI 3.0 and 3.1, from a file or a URL it can reload on an
interval; serves MCP over `stdio`, Streamable HTTP or the legacy SSE transport;
authenticates to the upstream API with static, forwarded or OAuth2 credentials,
including per-caller delegation; and verifies callers' JWTs to decide which
tools each one may use. The
[feature list](https://oas2mcp.therealm.tech/#features) has the
details.

See [ARCHITECTURE.md](ARCHITECTURE.md) for how it is put together.

## Getting started

### Prerequisites

[`rustup`](https://rustup.rs): the toolchain version is pinned in
`rust-toolchain.toml` and installed on the first `cargo` invocation. Or Docker,
to build the image instead.

### Installation

```bash
git clone https://github.com/therealm-tech/oas2mcp.git
```

```bash
cd oas2mcp
```

```bash
cargo build --release
```

The binary is `target/release/oas2mcp`. Or with Docker:

```bash
docker build -t oas2mcp .
```

### Usage

Expose the bundled Petstore example over stdio:

```bash
target/release/oas2mcp --openapi-file examples/petstore.yaml
```

Serve a remote API over Streamable HTTP, on `POST http://0.0.0.0:8000/mcp`:

```bash
target/release/oas2mcp http --openapi-url https://api.example.com/openapi.json --bind-addr 0.0.0.0:8000 --header 'Authorization: Bearer <token>'
```

Every option is also an environment variable; the
[configuration reference](https://oas2mcp.therealm.tech/reference/configuration/)
lists them all, and the
[guides](https://oas2mcp.therealm.tech/getting-started/) cover
authentication, filtering, metrics and connecting an MCP client.

### Deployment

A Helm chart lives in [charts/oas2mcp](charts/oas2mcp):

```bash
helm install petstore charts/oas2mcp --set oas2mcp.openapi.url=https://petstore3.swagger.io/api/v3/openapi.json
```

See [Deploying on Kubernetes](https://oas2mcp.therealm.tech/guides/kubernetes/)
and the chart's [README](charts/oas2mcp/README.md) for every value.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development setup, the tests and
the CI.

## License

Apache-2.0 — see [LICENSE](LICENSE).
