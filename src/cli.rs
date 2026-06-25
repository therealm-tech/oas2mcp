//! Command-line interface. Configuration resolves CLI flags → environment
//! variables → defaults, and every option carries an `env = "..."`.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use url::Url;

/// MCP transport to expose the server over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum Transport {
    /// Standard input/output — for a local subprocess MCP client.
    Stdio,
    /// Legacy HTTP+SSE transport (deprecated by the MCP spec, kept for
    /// compatibility with older clients).
    Sse,
    /// Streamable HTTP — the current remote transport, single `/mcp` endpoint.
    StreamableHttp,
}

impl std::fmt::Display for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Mirror the `ValueEnum` kebab-case names so `default_value_t` round-trips.
        f.write_str(match self {
            Self::Stdio => "stdio",
            Self::Sse => "sse",
            Self::StreamableHttp => "streamable-http",
        })
    }
}

fn default_bind_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 8000))
}

#[derive(Debug, Clone, Parser)]
#[command(name = "oas2mcp", version, about, long_about = None)]
pub struct Cli {
    /// Path to an OpenAPI document (JSON or YAML) on disk.
    #[arg(long, env = "OPENAPI_FILE", conflicts_with = "openapi_url")]
    pub openapi_file: Option<PathBuf>,

    /// URL of an OpenAPI document (JSON or YAML) fetched once at startup.
    #[arg(long, env = "OPENAPI_URL", conflicts_with = "openapi_file")]
    pub openapi_url: Option<Url>,

    /// Base URL of the upstream API that tool calls are proxied to. Overrides
    /// the `servers` entry of the OpenAPI document.
    #[arg(long, env = "BASE_URL")]
    pub base_url: Option<Url>,

    /// Extra header attached to every upstream request, as `Name: Value`.
    /// Repeatable; use it for auth (e.g. `Authorization: Bearer ...`). When set
    /// via the environment variable, separate headers with newlines.
    #[arg(long = "header", env = "UPSTREAM_HEADERS", value_delimiter = '\n')]
    pub headers: Vec<String>,

    /// Name of an incoming-request header to forward verbatim to the upstream
    /// API (e.g. `Authorization`). Repeatable; use it to pass the MCP client's
    /// own credentials through to the API. Only the `streamable-http` transport
    /// exposes the client's HTTP headers; ignored for `stdio` and `sse`. A
    /// static `--header` of the same name takes precedence. When set via the
    /// environment variable, separate names with newlines.
    #[arg(
        long = "forward-header",
        env = "FORWARD_HEADERS",
        value_delimiter = '\n'
    )]
    pub forward_headers: Vec<String>,

    /// MCP transport to expose.
    #[arg(long, env = "TRANSPORT", default_value_t = Transport::Stdio)]
    pub transport: Transport,

    /// Address to bind for the `sse` and `streamable-http` transports.
    #[arg(long, env = "BIND_ADDR", default_value_t = default_bind_addr())]
    pub bind_addr: SocketAddr,

    /// `tracing` filter directive (e.g. `info`, `oas2mcp=debug,rmcp=warn`).
    #[arg(long = "log-filter", env = "RUST_LOG", default_value = "info")]
    pub log_filter: String,
}
