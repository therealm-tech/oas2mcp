//! `oas2mcp` — load an OpenAPI document at startup and expose every operation
//! as a tool of a Model Context Protocol (MCP) server.
//!
//! The server can be exposed over three transports: `stdio`, the legacy
//! HTTP+SSE transport, and Streamable HTTP. A tool call is proxied as a real
//! HTTP request to the upstream API described by the document.

mod cli;
mod filter;
mod oauth;
mod openapi;
mod server;
mod tools;
mod transport;

use std::time::Duration;

use anyhow::Context as _;
use clap::Parser as _;
use tracing_subscriber::EnvFilter;
use url::Url;

use crate::cli::Cli;
use crate::server::OpenApiServer;

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

    let doc_auth =
        openapi::DocAuth::from_cli(&cli).context("configuring OpenAPI document authentication")?;

    let spec = openapi::load(&cli, &doc_auth)
        .await
        .context("failed to load the OpenAPI document")?;

    let server = server::OpenApiServer::from_spec(&spec, &cli)
        .context("failed to build the MCP server from the OpenAPI document")?;

    tracing::info!(
        transport = %cli.transport,
        tools = server.tool_count(),
        "starting MCP server"
    );

    // When asked to reload, re-fetch the document on an interval in the
    // background. Only a URL source can be reloaded; a file source is loaded
    // once at startup.
    if let Some(interval) = cli.reload_every {
        match &cli.openapi_url {
            Some(url) => {
                tracing::info!(?interval, %url, "reloading the OpenAPI document periodically");
                tokio::spawn(reload_loop(
                    server.clone(),
                    cli.clone(),
                    doc_auth.clone(),
                    url.clone(),
                    interval,
                ));
            }
            None => tracing::warn!(
                "--reload-every is set but the OpenAPI document is not loaded from a URL; \
                 nothing to reload"
            ),
        }
    }

    transport::serve(cli.transport, cli.bind_addr, server)
        .await
        .context("MCP transport terminated with an error")?;

    Ok(())
}

/// Periodically re-fetch the OpenAPI document from `url` and swap the server's
/// tool set. A failed fetch or rebuild is logged and the previous tool set is
/// kept, so a transient upstream blip never empties the server.
async fn reload_loop(
    server: OpenApiServer,
    cli: Cli,
    auth: openapi::DocAuth,
    url: Url,
    interval: Duration,
) {
    let mut ticker = tokio::time::interval(interval);
    // The first tick fires immediately; skip it since we just loaded at startup.
    ticker.tick().await;
    loop {
        ticker.tick().await;
        match auth.fetch(&url).await {
            Ok(spec) => {
                if let Err(err) = server.reload(&spec, &cli) {
                    tracing::error!(
                        error = format!("{err:#}"),
                        "failed to rebuild tools from the reloaded document; keeping the current set"
                    );
                }
            }
            Err(err) => tracing::warn!(
                error = format!("{err:#}"),
                "failed to fetch the OpenAPI document for reload; keeping the current set"
            ),
        }
    }
}
