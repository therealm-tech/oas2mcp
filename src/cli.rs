//! Command-line interface. Configuration resolves CLI flags → environment
//! variables → defaults, and every option carries an `env = "..."`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, ValueEnum};
use regex::Regex;
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

    /// URL of an OpenAPI document (JSON or YAML) fetched at startup (and
    /// periodically when `--reload-every` is set).
    #[arg(long, env = "OPENAPI_URL", conflicts_with = "openapi_file")]
    pub openapi_url: Option<Url>,

    /// Header to send when fetching the OpenAPI document from `--openapi-url`,
    /// as `Name: Value`. Repeatable; use it when the document URL is not public
    /// and needs auth (e.g. `Authorization: Bearer ...`). This is separate from
    /// `--header`, which targets the upstream API, not the document URL. When
    /// set via the environment variable, separate headers with newlines.
    #[arg(
        long = "openapi-header",
        env = "OPENAPI_HEADERS",
        value_delimiter = '\n'
    )]
    pub openapi_headers: Vec<String>,

    /// Re-fetch the OpenAPI document from `--openapi-url` on this interval and
    /// rebuild the exposed tool set (e.g. `30s`, `5m`, `1h`). Omit to load the
    /// document only once at startup. Ignored when the document is loaded from a
    /// file rather than a URL.
    #[arg(long, env = "RELOAD_EVERY", value_parser = humantime::parse_duration)]
    pub reload_every: Option<Duration>,

    /// OAuth 2.0 token endpoint for the `client_credentials` grant. When set,
    /// the OpenAPI document fetch (initial and every reload) authenticates with
    /// a bearer token obtained here, refreshed automatically before it expires —
    /// use it instead of a static `--openapi-header` token that would go stale
    /// on a long-running server. Requires `--openapi-oauth-client-id` and
    /// `--openapi-oauth-client-secret`.
    #[arg(
        long = "openapi-oauth-token-url",
        env = "OPENAPI_OAUTH_TOKEN_URL",
        requires = "openapi_oauth_client_id"
    )]
    pub openapi_oauth_token_url: Option<Url>,

    /// OAuth 2.0 client ID for the document-fetch `client_credentials` grant.
    #[arg(
        long = "openapi-oauth-client-id",
        env = "OPENAPI_OAUTH_CLIENT_ID",
        requires = "openapi_oauth_client_secret"
    )]
    pub openapi_oauth_client_id: Option<String>,

    /// OAuth 2.0 client secret for the document-fetch `client_credentials`
    /// grant. Prefer the environment variable over the command line so the
    /// secret does not leak into the process list.
    #[arg(
        long = "openapi-oauth-client-secret",
        env = "OPENAPI_OAUTH_CLIENT_SECRET"
    )]
    pub openapi_oauth_client_secret: Option<String>,

    /// OAuth 2.0 scope requested for the document-fetch token. Repeatable; sent
    /// space-joined as the `scope` parameter. When set via the environment
    /// variable, separate scopes with newlines.
    #[arg(
        long = "openapi-oauth-scope",
        env = "OPENAPI_OAUTH_SCOPES",
        value_delimiter = '\n'
    )]
    pub openapi_oauth_scopes: Vec<String>,

    /// OAuth 2.0 `audience` parameter for the document-fetch token. Some
    /// providers (e.g. Auth0) require it to issue a token for the target API.
    #[arg(long = "openapi-oauth-audience", env = "OPENAPI_OAUTH_AUDIENCE")]
    pub openapi_oauth_audience: Option<String>,

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

    /// Only expose operations whose name (operationId, or `<method>_<path>`)
    /// matches this glob. Repeatable; an operation is kept if it matches any
    /// `--include` or carries any `--tag`. Globs support `*` and `?`. Use it to
    /// cut a huge API down to a usable tool set. When set via the environment
    /// variable, separate patterns with newlines.
    #[arg(long = "include", env = "INCLUDE_OPERATIONS", value_delimiter = '\n')]
    pub include_operations: Vec<String>,

    /// Drop operations whose name matches this glob. Repeatable; takes
    /// precedence over `--include`/`--tag`. When set via the environment
    /// variable, separate patterns with newlines.
    #[arg(long = "exclude", env = "EXCLUDE_OPERATIONS", value_delimiter = '\n')]
    pub exclude_operations: Vec<String>,

    /// Only expose operations whose name matches this regex (e.g.
    /// `^(get|post)ApiV4Projects`). Repeatable; combines with `--include`/`--tag`
    /// as an allowlist. Invalid patterns are rejected at startup. When set via
    /// the environment variable, separate patterns with newlines.
    #[arg(
        long = "include-regex",
        env = "INCLUDE_OPERATIONS_REGEX",
        value_delimiter = '\n',
        value_parser = Regex::new
    )]
    pub include_operations_regex: Vec<Regex>,

    /// Drop operations whose name matches this regex. Repeatable; takes
    /// precedence over the allowlist. Invalid patterns are rejected at startup.
    /// When set via the environment variable, separate patterns with newlines.
    #[arg(
        long = "exclude-regex",
        env = "EXCLUDE_OPERATIONS_REGEX",
        value_delimiter = '\n',
        value_parser = Regex::new
    )]
    pub exclude_operations_regex: Vec<Regex>,

    /// Only expose operations carrying this OpenAPI tag (case-insensitive).
    /// Repeatable; combines with `--include` as an allowlist. When set via the
    /// environment variable, separate tags with newlines.
    #[arg(long = "tag", env = "INCLUDE_TAGS", value_delimiter = '\n')]
    pub include_tags: Vec<String>,

    /// Drop operations carrying this OpenAPI tag (case-insensitive). Repeatable;
    /// takes precedence over the allowlist. When set via the environment
    /// variable, separate tags with newlines.
    #[arg(long = "exclude-tag", env = "EXCLUDE_TAGS", value_delimiter = '\n')]
    pub exclude_tags: Vec<String>,

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_operation_regex_is_compiled() {
        let cli = Cli::try_parse_from(["oas2mcp", "--include-regex", "^getApiV4Projects"])
            .expect("valid regex parses");
        assert_eq!(
            cli.include_operations_regex[0].as_str(),
            "^getApiV4Projects"
        );
    }

    #[test]
    fn invalid_operation_regex_is_rejected_by_clap() {
        let err = Cli::try_parse_from(["oas2mcp", "--include-regex", "("])
            .expect_err("invalid regex must fail at parse time");
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn oauth_token_url_requires_client_id_and_secret() {
        // token-url alone is incomplete: it requires a client id, which in turn
        // requires a client secret.
        let err = Cli::try_parse_from([
            "oas2mcp",
            "--openapi-oauth-token-url",
            "https://idp.example.com/token",
        ])
        .expect_err("token-url without credentials must fail");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);

        // The full triple parses.
        Cli::try_parse_from([
            "oas2mcp",
            "--openapi-oauth-token-url",
            "https://idp.example.com/token",
            "--openapi-oauth-client-id",
            "id",
            "--openapi-oauth-client-secret",
            "secret",
        ])
        .expect("complete OAuth config parses");
    }
}
