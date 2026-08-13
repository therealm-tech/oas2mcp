//! `oas2mcp` — load an OpenAPI document at startup and expose every operation
//! as a tool of a Model Context Protocol (MCP) server.
//!
//! The server can be exposed over three transports: `stdio`, the legacy
//! HTTP+SSE transport, and Streamable HTTP. A tool call is proxied as a real
//! HTTP request to the upstream API described by the document.

mod auth;
mod cli;
mod filter;
mod http;
mod oauth;
mod openapi;
mod rename;
mod server;
mod telemetry;
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

    let authorizer = auth::Authorizer::from_cli(&cli)
        .await
        .context("configuring JWT role-based authorization")?;
    if authorizer.is_some() && cli.transport != cli::Transport::StreamableHttp {
        tracing::warn!(
            transport = %cli.transport,
            "--oauth-role-mapper only takes effect on the streamable-http transport; \
             on this transport no client JWT is available, so every tool stays hidden"
        );
    }

    check_delegation_is_possible(&cli, authorizer.is_some())?;

    let telemetry =
        telemetry::Telemetry::from_cli(&cli).context("configuring metrics telemetry")?;

    let server =
        server::OpenApiServer::from_spec(&spec, &cli, authorizer, telemetry.metrics.clone())
            .context("failed to build the MCP server from the OpenAPI document")?;

    tracing::info!(
        transport = %cli.transport,
        openapi = spec.version(),
        tools = server.tool_count(),
        "starting MCP server"
    );

    telemetry
        .serve_metrics()
        .await
        .context("starting the Prometheus metrics endpoint")?;

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

    if cli.stream_responses && cli.transport != cli::Transport::StreamableHttp {
        tracing::warn!(
            transport = %cli.transport,
            "--stream-responses only takes effect on the streamable-http transport; ignored here"
        );
    }

    if !cli.allowed_hosts.is_empty() && cli.transport != cli::Transport::StreamableHttp {
        tracing::warn!(
            transport = %cli.transport,
            "--allowed-host only takes effect on the streamable-http transport; ignored here"
        );
    }

    transport::serve(
        cli.transport,
        cli.bind_addr,
        !cli.stream_responses,
        &cli.allowed_hosts,
        server,
    )
    .await
    .context("MCP transport terminated with an error")?;

    // Flush any metrics buffered by the OTLP exporter before exiting.
    telemetry.shutdown();

    Ok(())
}

/// Refuse to start when the upstream grant delegates but no call could ever
/// supply a caller identity.
///
/// This is an error, not a warning. The other "ineffective flag" cases degrade
/// into doing less; this one would leave a server that accepts calls and fails
/// every single one of them, which is worse discovered at startup than at the
/// first tool call. And the alternative — falling back to the client's own
/// identity — is exactly the privilege escalation the delegation exists to
/// avoid.
fn check_delegation_is_possible(cli: &Cli, has_authorizer: bool) -> anyhow::Result<()> {
    let delegates = cli.upstream_oauth_token_url.is_some()
        && cli.upstream_oauth_grant == cli::UpstreamGrant::JwtBearer
        && cli.upstream_oauth_subject.is_none();
    if !delegates {
        return Ok(());
    }

    if !has_authorizer {
        anyhow::bail!(
            "--upstream-oauth-grant jwt-bearer acts on behalf of the caller, which needs a \
             verified caller identity: configure --oauth-role-mapper with a JWKS, or pin a \
             fixed identity with --upstream-oauth-subject"
        );
    }
    if cli.transport != cli::Transport::StreamableHttp {
        anyhow::bail!(
            "--upstream-oauth-grant jwt-bearer acts on behalf of the caller, but the {} \
             transport exposes no client JWT: use --transport streamable-http, or pin a fixed \
             identity with --upstream-oauth-subject",
            cli.transport
        );
    }
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

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;

    fn cli_from(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("oas2mcp").chain(args.iter().copied()))
            .expect("CLI parses")
    }

    /// The flags that turn on a delegating upstream grant.
    fn delegating(extra: &[&str]) -> Vec<String> {
        let mut args: Vec<String> = [
            "--upstream-oauth-token-url",
            "https://idp.example.com/token",
            "--upstream-oauth-client-id",
            "id",
            "--upstream-oauth-private-key",
            "tests/fixtures/test_rsa_key.pem",
            "--upstream-oauth-grant",
            "jwt-bearer",
        ]
        .iter()
        .map(|arg| (*arg).to_string())
        .collect();
        args.extend(extra.iter().map(|arg| (*arg).to_string()));
        args
    }

    #[test]
    fn delegation_needs_a_verified_caller() {
        let args = delegating(&["--transport", "streamable-http"]);
        let cli = cli_from(&args.iter().map(String::as_str).collect::<Vec<_>>());

        // Without an authorizer there is no verified identity, so every call
        // would fail. Better to say so now than at the first tool call.
        let err = check_delegation_is_possible(&cli, false)
            .expect_err("delegation without an authorizer must be refused");
        assert!(
            format!("{err:#}").contains("verified caller identity"),
            "{err:#}"
        );

        check_delegation_is_possible(&cli, true).expect("with an authorizer it is fine");
    }

    #[test]
    fn delegation_needs_a_transport_that_carries_a_jwt() {
        let args = delegating(&[]);
        let cli = cli_from(&args.iter().map(String::as_str).collect::<Vec<_>>());
        // Default transport is stdio, which exposes no client headers.
        let err =
            check_delegation_is_possible(&cli, true).expect_err("stdio cannot carry a caller JWT");
        assert!(format!("{err:#}").contains("streamable-http"), "{err:#}");
    }

    #[test]
    fn a_fixed_subject_needs_neither() {
        // A service account acts as itself, so it works on any transport with no
        // authorizer at all.
        let args = delegating(&["--upstream-oauth-subject", "service-acct"]);
        let cli = cli_from(&args.iter().map(String::as_str).collect::<Vec<_>>());
        check_delegation_is_possible(&cli, false).expect("a fixed subject delegates to nobody");
    }

    #[test]
    fn client_credentials_is_never_gated() {
        let cli = cli_from(&[
            "--upstream-oauth-token-url",
            "https://idp.example.com/token",
            "--upstream-oauth-client-id",
            "id",
            "--upstream-oauth-client-secret",
            "secret",
        ]);
        check_delegation_is_possible(&cli, false).expect("client_credentials delegates to nobody");
    }
}
