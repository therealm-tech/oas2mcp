<p align="center">
  <img src="logo.svg" alt="oas2mcp logo" width="180">
</p>

# oas2mcp

[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/therealm-tech/oas2mcp/badge)](https://scorecard.dev/viewer/?uri=github.com/therealm-tech/oas2mcp)

Load an [OpenAPI](https://www.openapis.org/) document at startup and expose
every operation it describes as a tool of a
[Model Context Protocol (MCP)](https://modelcontextprotocol.io/) server.

**Documentation: <https://oas2mcp.therealm.tech/>**
— also for LLMs, at <https://oas2mcp.therealm.tech/llms.txt>.

## Description

Each OpenAPI operation becomes one MCP tool: calling it sends the matching HTTP
request to the upstream API, so an agent can drive that API without glue code.

It reads OpenAPI 3.0 and 3.1, serves MCP over `stdio`, Streamable HTTP or SSE,
authenticates upstream with static, forwarded or OAuth2 credentials, and gates
tools by the caller's verified JWT. The
[feature list](https://oas2mcp.therealm.tech/#features) has the details.

See [ARCHITECTURE.md](ARCHITECTURE.md) for how it is put together.

## Getting started

### Prerequisites

Docker, to run the published image; or a Rust toolchain, to build from source —
[mise](https://mise.jdx.dev) installs the version pinned in
[`mise.toml`](mise.toml) with `mise install`.

### Installation

The image is published on GHCR for `amd64` and `arm64`; its entrypoint is the
`oas2mcp` binary:

```bash
docker run --rm -i ghcr.io/therealm-tech/oas2mcp --openapi-url https://petstore3.swagger.io/api/v3/openapi.json
```

Each release image carries a signed build provenance attestation; check that
this repository's CI built it with the [GitHub CLI](https://cli.github.com/):

```bash
gh attestation verify oci://ghcr.io/therealm-tech/oas2mcp:latest --repo therealm-tech/oas2mcp
```

Or build from source:

```bash
git clone https://github.com/therealm-tech/oas2mcp.git
```

```bash
cd oas2mcp
```

```bash
cargo build --release
```

The binary is `target/release/oas2mcp`.

### Configuration

Every option is also an environment variable; the
[configuration reference](https://oas2mcp.therealm.tech/reference/configuration/)
lists them all.

### Usage

Expose the bundled Petstore example over stdio:

```bash
target/release/oas2mcp --openapi-file examples/petstore.yaml
```

Serve a remote API over Streamable HTTP, on `POST http://0.0.0.0:8000/mcp`:

```bash
target/release/oas2mcp http --openapi-url https://api.example.com/openapi.json --bind-addr 0.0.0.0:8000 --header 'Authorization: Bearer <token>'
```

The [documentation](https://oas2mcp.therealm.tech/getting-started/) covers
authentication, filtering, metrics and connecting an MCP client.

### Deployment

```bash
helm install petstore oci://ghcr.io/therealm-tech/charts/oas2mcp --version 1.1.0 --set oas2mcp.openapi.url=https://petstore3.swagger.io/api/v3/openapi.json
```

See [Deploying on Kubernetes](https://oas2mcp.therealm.tech/guides/kubernetes/).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development setup, the tests and
the CI.

## License

Apache-2.0 — see [LICENSE](LICENSE).
