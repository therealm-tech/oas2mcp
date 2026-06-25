//! Serving the MCP server over the selected transport.

mod sse;

use std::net::SocketAddr;

use anyhow::Context as _;
use rmcp::ServiceExt as _;
use rmcp::transport::stdio;
use rmcp::transport::streamable_http_server::StreamableHttpService;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;

use crate::cli::Transport;
use crate::server::OpenApiServer;

/// Serve the MCP server over `transport`, blocking until shutdown.
pub async fn serve(
    transport: Transport,
    bind: SocketAddr,
    server: OpenApiServer,
) -> anyhow::Result<()> {
    match transport {
        Transport::Stdio => serve_stdio(server).await,
        Transport::Sse => sse::serve(bind, server).await,
        Transport::StreamableHttp => serve_streamable_http(bind, server).await,
    }
}

async fn serve_stdio(server: OpenApiServer) -> anyhow::Result<()> {
    let service = server
        .serve(stdio())
        .await
        .context("starting the stdio transport")?;
    service.waiting().await.context("stdio transport failed")?;
    Ok(())
}

async fn serve_streamable_http(bind: SocketAddr, server: OpenApiServer) -> anyhow::Result<()> {
    // One server instance is built per MCP session.
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        LocalSessionManager::default().into(),
        Default::default(),
    );
    let app = axum::Router::new().nest_service("/mcp", service);

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!(%bind, "Streamable HTTP MCP endpoint listening at POST /mcp");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("Streamable HTTP server failed")?;
    Ok(())
}

/// Completes on `SIGTERM` or `SIGINT` so the server can drain gracefully.
pub(crate) async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut interrupt = signal(SignalKind::interrupt()).expect("install SIGINT handler");
        let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    tracing::info!("shutdown signal received, draining");
}
