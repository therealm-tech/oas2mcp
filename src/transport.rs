//! Serving the MCP server over the selected transport.

mod access_log;
mod sse;

use std::net::SocketAddr;

use anyhow::Context as _;
use rmcp::ServiceExt as _;
use rmcp::transport::stdio;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};

use crate::cli::Transport;
use crate::server::OpenApiServer;

/// `Host` values `rmcp` accepts by default — the loopback names, which is where
/// a DNS rebinding attack would aim.
const LOOPBACK_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

/// `--allowed-host` value that turns `Host` validation off entirely.
const ANY_HOST: &str = "*";

/// Serve the MCP server over `transport`, blocking until shutdown.
///
/// `json_response` and `allowed_hosts` only affect `streamable-http`: the former
/// makes POST replies a single `application/json` body instead of an SSE stream,
/// the latter gates the inbound `Host` header (see `Cli`).
pub async fn serve(
    transport: Transport,
    bind: SocketAddr,
    json_response: bool,
    allowed_hosts: &[String],
    server: OpenApiServer,
) -> anyhow::Result<()> {
    match transport {
        Transport::Stdio => serve_stdio(server).await,
        Transport::Sse => sse::serve(bind, server).await,
        Transport::StreamableHttp => {
            serve_streamable_http(bind, json_response, allowed_hosts, server).await
        }
    }
}

/// Resolve the `Host` allowlist `rmcp` validates inbound requests against.
///
/// An empty result disables the check — that is `rmcp`'s own reading of an empty
/// list, not a special case invented here.
///
/// `rmcp` defaults to loopback-only, which is right for a server on a developer's
/// machine and wrong for every other deployment: bound to `0.0.0.0` behind a
/// Kubernetes Service or an Ingress, the `Host` is a name this process never
/// learns, so the default turns every real request into a `403`. The bind
/// address is the honest signal of which situation we are in, since DNS
/// rebinding is an attack on locally-bound servers specifically.
fn resolve_allowed_hosts(bind: SocketAddr, configured: &[String]) -> Vec<String> {
    if configured.iter().any(|host| host == ANY_HOST) {
        tracing::warn!(
            "--allowed-host {ANY_HOST} accepts any Host header, disabling DNS rebinding protection"
        );
        return Vec::new();
    }
    if !configured.is_empty() {
        tracing::debug!(hosts = ?configured, "validating the inbound Host header");
        return configured.to_vec();
    }
    if bind.ip().is_loopback() {
        tracing::debug!(
            hosts = ?LOOPBACK_HOSTS,
            "bound to loopback, so only loopback Host headers are accepted; \
             set --allowed-host to name others"
        );
        return LOOPBACK_HOSTS.map(String::from).to_vec();
    }
    tracing::info!(
        "accepting any Host header: --allowed-host is unset and the bind address is not \
         loopback; set it to the hostnames clients reach this server under"
    );
    Vec::new()
}

async fn serve_stdio(server: OpenApiServer) -> anyhow::Result<()> {
    let service = server
        .serve(stdio())
        .await
        .context("starting the stdio transport")?;
    service.waiting().await.context("stdio transport failed")?;
    Ok(())
}

async fn serve_streamable_http(
    bind: SocketAddr,
    json_response: bool,
    allowed_hosts: &[String],
    server: OpenApiServer,
) -> anyhow::Result<()> {
    // One server instance is built per MCP session.
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        LocalSessionManager::default().into(),
        // rmcp only honours `json_response` in stateless mode: the stateful
        // path always replies over SSE (with a priming event) regardless. So
        // enabling JSON replies means turning stateful sessions off too. That
        // is fine behind a proxy — oas2mcp is a stateless request/response
        // proxy, and a gateway like Envoy manages MCP sessions itself.
        StreamableHttpServerConfig::default()
            .with_json_response(json_response)
            .with_stateful_mode(!json_response)
            .with_allowed_hosts(resolve_allowed_hosts(bind, allowed_hosts)),
    );
    // The access log wraps `rmcp`'s service: most of its rejections happen in
    // there, and this is the only place they become visible.
    let app = axum::Router::new()
        .nest_service("/mcp", service)
        .layer(axum::middleware::from_fn(access_log::log_requests));

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

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(literal: &str) -> SocketAddr {
        literal.parse().expect("valid socket address")
    }

    #[test]
    fn an_explicit_list_is_used_as_is() {
        let configured = ["mcp.example.com".to_string(), "10.0.0.7:8000".to_string()];
        assert_eq!(
            resolve_allowed_hosts(addr("0.0.0.0:8000"), &configured),
            configured
        );
        // Even bound to loopback: an explicit list is a deliberate answer, and
        // silently adding the loopback names back would widen it.
        assert_eq!(
            resolve_allowed_hosts(addr("127.0.0.1:8000"), &configured),
            configured
        );
    }

    #[test]
    fn the_wildcard_disables_the_check() {
        // Empty is how rmcp spells "accept anything".
        assert!(resolve_allowed_hosts(addr("127.0.0.1:8000"), &[ANY_HOST.to_string()]).is_empty());
        // It wins over its neighbours rather than being one more allowed name.
        let mixed = [ANY_HOST.to_string(), "mcp.example.com".to_string()];
        assert!(resolve_allowed_hosts(addr("0.0.0.0:8000"), &mixed).is_empty());
    }

    #[test]
    fn a_loopback_bind_keeps_the_rebinding_protection() {
        for bind in ["127.0.0.1:8000", "[::1]:8000"] {
            assert_eq!(
                resolve_allowed_hosts(addr(bind), &[]),
                LOOPBACK_HOSTS,
                "{bind}"
            );
        }
    }

    #[test]
    fn a_routable_bind_accepts_any_host_by_default() {
        // The hostname clients use is a Service or Ingress name this process
        // cannot guess, so a loopback-only default would reject every request.
        for bind in ["0.0.0.0:8000", "[::]:8000", "10.0.0.7:8000"] {
            assert!(resolve_allowed_hosts(addr(bind), &[]).is_empty(), "{bind}");
        }
    }
}
