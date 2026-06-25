//! `oas2mcp` — load an OpenAPI document at startup and expose every operation
//! as a tool of a Model Context Protocol (MCP) server.
//!
//! The server can be exposed over three transports: `stdio`, the legacy
//! HTTP+SSE transport, and Streamable HTTP. A tool call is proxied as a real
//! HTTP request to the upstream API described by the document.

mod cli;
mod openapi;
mod server;
mod tools;
mod transport;

use anyhow::Context as _;
use clap::Parser as _;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();

    // The filter directive flows through clap (RUST_LOG), never read directly.
    // SSE/stdio multiplex protocol traffic, so logs always go to stderr.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(&cli.log_filter))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let spec = openapi::load(&cli)
        .await
        .context("failed to load the OpenAPI document")?;

    let server = server::OpenApiServer::from_spec(&spec, &cli)
        .context("failed to build the MCP server from the OpenAPI document")?;

    tracing::info!(
        transport = %cli.transport,
        tools = server.tool_count(),
        "starting MCP server"
    );

    transport::serve(cli.transport, cli.bind_addr, server)
        .await
        .context("MCP transport terminated with an error")?;

    Ok(())
}
