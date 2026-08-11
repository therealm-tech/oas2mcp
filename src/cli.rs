//! Command-line interface. Configuration resolves CLI flags → environment
//! variables → defaults, and every option carries an `env = "..."`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use clap::{ArgGroup, Parser, ValueEnum};
use regex::Regex;
use url::Url;

/// Signature algorithm for the JWT client assertions of RFC 7523. Only
/// asymmetric algorithms are offered: RFC 7523 §2.2 permits a MAC, but an HMAC
/// keyed on the client secret adds nothing over sending the secret itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum SigningAlg {
    Rs256,
    Rs384,
    Rs512,
    Ps256,
    Ps384,
    Ps512,
    Es256,
    Es384,
    EdDsa,
}

impl std::fmt::Display for SigningAlg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Mirror the `ValueEnum` names so `default_value_t` round-trips.
        f.write_str(match self {
            Self::Rs256 => "rs256",
            Self::Rs384 => "rs384",
            Self::Rs512 => "rs512",
            Self::Ps256 => "ps256",
            Self::Ps384 => "ps384",
            Self::Ps512 => "ps512",
            Self::Es256 => "es256",
            Self::Es384 => "es384",
            Self::EdDsa => "eddsa",
        })
    }
}

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
// The document-fetch grant authenticates with *either* a shared secret or a
// signed assertion, never both. Naming the pair as a group is what lets
// `--openapi-oauth-token-url` require "one of these" rather than a single flag.
#[command(group(
    ArgGroup::new("openapi_oauth_client_auth")
        .args(["openapi_oauth_client_secret", "openapi_oauth_private_key"])
        .multiple(false)
))]
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
    /// on a long-running server. Requires `--openapi-oauth-client-id` plus one
    /// of `--openapi-oauth-client-secret` / `--openapi-oauth-private-key`.
    #[arg(
        long = "openapi-oauth-token-url",
        env = "OPENAPI_OAUTH_TOKEN_URL",
        requires_all = ["openapi_oauth_client_id", "openapi_oauth_client_auth"]
    )]
    pub openapi_oauth_token_url: Option<Url>,

    /// OAuth 2.0 client ID for the document-fetch `client_credentials` grant.
    #[arg(long = "openapi-oauth-client-id", env = "OPENAPI_OAUTH_CLIENT_ID")]
    pub openapi_oauth_client_id: Option<String>,

    /// OAuth 2.0 client secret for the document-fetch `client_credentials`
    /// grant, sent over HTTP Basic. Prefer the environment variable over the
    /// command line so the secret does not leak into the process list. Mutually
    /// exclusive with `--openapi-oauth-private-key`.
    #[arg(
        long = "openapi-oauth-client-secret",
        env = "OPENAPI_OAUTH_CLIENT_SECRET"
    )]
    pub openapi_oauth_client_secret: Option<String>,

    /// Path to a PEM file holding the client's private key, to authenticate the
    /// document-fetch grant with a signed JWT assertion instead of a shared
    /// secret (`private_key_jwt`, RFC 7523 §2.2). Use it with providers that
    /// will not issue a client secret, or to keep the credential
    /// non-exportable. PKCS#8 PEM is expected; the algorithm is
    /// `--openapi-oauth-signing-alg`. Mutually exclusive with
    /// `--openapi-oauth-client-secret`.
    #[arg(
        long = "openapi-oauth-private-key",
        env = "OPENAPI_OAUTH_PRIVATE_KEY_FILE"
    )]
    pub openapi_oauth_private_key: Option<PathBuf>,

    /// `kid` header to put on the client assertion, so the provider can pick the
    /// right key from the ones it has registered for this client. Omit when the
    /// provider holds a single key. Only used with
    /// `--openapi-oauth-private-key`.
    #[arg(
        long = "openapi-oauth-key-id",
        env = "OPENAPI_OAUTH_KEY_ID",
        requires = "openapi_oauth_private_key"
    )]
    pub openapi_oauth_key_id: Option<String>,

    /// Algorithm used to sign the client assertion. Must match the key type of
    /// `--openapi-oauth-private-key` (an RSA key for `rs*`/`ps*`, an EC key for
    /// `es*`, an Ed25519 key for `eddsa`) and be one the provider accepts. Only
    /// used with `--openapi-oauth-private-key`.
    #[arg(
        long = "openapi-oauth-signing-alg",
        env = "OPENAPI_OAUTH_SIGNING_ALG",
        default_value_t = SigningAlg::Rs256
    )]
    pub openapi_oauth_signing_alg: SigningAlg,

    /// `aud` claim of the client assertion, identifying the authorization
    /// server. Defaults to the token endpoint URL, which is what RFC 7523
    /// suggests and what most providers expect; override it when the provider
    /// wants its issuer identifier instead. Only used with
    /// `--openapi-oauth-private-key`.
    #[arg(
        long = "openapi-oauth-assertion-audience",
        env = "OPENAPI_OAUTH_ASSERTION_AUDIENCE",
        requires = "openapi_oauth_private_key"
    )]
    pub openapi_oauth_assertion_audience: Option<String>,

    /// How long a client assertion stays valid (e.g. `30s`, `2m`). Defaults to
    /// `60s`. Keep it short — the assertion is minted per token request, so a
    /// long window only widens the replay opportunity. Only used with
    /// `--openapi-oauth-private-key`.
    #[arg(
        long = "openapi-oauth-assertion-lifetime",
        env = "OPENAPI_OAUTH_ASSERTION_LIFETIME",
        value_parser = humantime::parse_duration,
        requires = "openapi_oauth_private_key"
    )]
    pub openapi_oauth_assertion_lifetime: Option<Duration>,

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

    /// Restrict which tools a caller may see and invoke based on the roles
    /// carried in their JWT, as `role:tool_name_regex` (e.g.
    /// `admin:.*`, `reader:^get`). Repeatable; a tool is allowed if any of the
    /// caller's roles maps to a regex matching the tool name. When set, the
    /// incoming request's `Authorization: Bearer` JWT is verified against a
    /// JWKS (`--oauth-jwks-url` or `--oauth-jwks-file`, one is required) and the
    /// roles are read from the `--oauth-role-claim` claim. A caller with no
    /// valid token, or whose roles match nothing, sees and can call no tools.
    /// Only the `streamable-http` transport exposes the client's JWT; ignored
    /// for `stdio` and `sse`. Invalid regexes are rejected at startup. When set
    /// via the environment variable, separate entries with newlines.
    #[arg(
        long = "oauth-role-mapper",
        env = "OAUTH_ROLE_MAPPER",
        value_delimiter = '\n'
    )]
    pub oauth_role_mapper: Vec<String>,

    /// URL of a JWKS document, fetched at startup, whose keys verify the
    /// incoming JWT signatures. Required (with `--oauth-jwks-file` as the
    /// alternative) when `--oauth-role-mapper` is set.
    #[arg(
        long = "oauth-jwks-url",
        env = "OAUTH_JWKS_URL",
        conflicts_with = "oauth_jwks_file"
    )]
    pub oauth_jwks_url: Option<Url>,

    /// Path to a JWKS document on disk whose keys verify the incoming JWT
    /// signatures. Mutually exclusive with `--oauth-jwks-url`.
    #[arg(long = "oauth-jwks-file", env = "OAUTH_JWKS_FILE")]
    pub oauth_jwks_file: Option<PathBuf>,

    /// Name of the JWT claim listing the caller's roles. The claim value may be
    /// an array of strings or a single whitespace-separated string. Only used
    /// when `--oauth-role-mapper` is set.
    #[arg(
        long = "oauth-role-claim",
        env = "OAUTH_ROLE_CLAIM",
        default_value = "roles"
    )]
    pub oauth_role_claim: String,

    /// Name of a JWT claim to surface in the per-call tracing log as a
    /// `jwt.claims` field (e.g. `sub`, `email`, `tenant_id`), for observability.
    /// Repeatable; each named claim that is present in the verified token is
    /// emitted, keeping its JSON shape. Claims are written to logs only, never
    /// added to metric labels, so this never inflates metric cardinality. Reads
    /// the claims from the JWT verified for role-based access, so it only takes
    /// effect when `--oauth-role-mapper` (and a JWKS) is configured. When set via
    /// the environment variable, separate names with newlines.
    #[arg(long = "trace-claim", env = "TRACE_CLAIMS", value_delimiter = '\n')]
    pub trace_claims: Vec<String>,

    /// Base OTLP endpoint to push tool-call metrics to over HTTP/protobuf (e.g.
    /// `http://localhost:4318`). When set, metrics are exported to this
    /// collector; the `/v1/metrics` signal path is appended automatically.
    /// Honours the standard `OTEL_EXPORTER_OTLP_ENDPOINT` variable. Independent
    /// of `--metrics-addr`; enable either, both, or neither.
    #[arg(long = "otlp-endpoint", env = "OTEL_EXPORTER_OTLP_ENDPOINT")]
    pub otlp_endpoint: Option<Url>,

    /// Address to serve a Prometheus `/metrics` endpoint on (e.g.
    /// `0.0.0.0:9090`). When set, tool-call metrics are exposed for scraping on
    /// a dedicated HTTP server, independent of the MCP transport.
    #[arg(long = "metrics-addr", env = "METRICS_ADDR")]
    pub metrics_addr: Option<SocketAddr>,

    /// `service.name` reported on exported metrics.
    #[arg(
        long = "otel-service-name",
        env = "OTEL_SERVICE_NAME",
        default_value = "oas2mcp"
    )]
    pub otel_service_name: String,

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

    /// Path to a PEM file holding one or more extra CA certificates to trust
    /// when verifying TLS for every outbound connection (upstream API, OpenAPI
    /// document fetch, OAuth token endpoint, JWKS). Repeatable; a single file
    /// may bundle a whole chain. The platform's built-in roots stay trusted —
    /// these are added on top, so you only need to supply your private or
    /// corporate CA. When set via the environment variable, separate paths with
    /// newlines.
    #[arg(long = "ca-cert", env = "CA_CERT_FILE", value_delimiter = '\n')]
    pub ca_certs: Vec<PathBuf>,

    /// MCP transport to expose.
    #[arg(long, env = "TRANSPORT", default_value_t = Transport::Stdio)]
    pub transport: Transport,

    /// Address to bind for the `sse` and `streamable-http` transports.
    #[arg(long, env = "BIND_ADDR", default_value_t = default_bind_addr())]
    pub bind_addr: SocketAddr,

    /// Stream `streamable-http` replies as a `text/event-stream` (SSE) flow with
    /// stateful MCP sessions, instead of the default single `application/json`
    /// reply.
    ///
    /// By default oas2mcp answers each `streamable-http` request with one
    /// `application/json` body (running statelessly). That is the most
    /// compatible mode: rmcp's SSE framing prepends a priming event whose
    /// `data:` line is empty, which some strict proxies (e.g. Envoy AI Gateway)
    /// refuse to parse — they abort on the empty event and report
    /// `MCP message is not a response`. Turn this on only when you specifically
    /// want SSE streaming and stateful sessions, and you are not behind such a
    /// proxy. Only affects `streamable-http`; ignored for `stdio` and `sse`.
    #[arg(long = "stream-responses", env = "STREAM_RESPONSES")]
    pub stream_responses: bool,

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
    fn json_replies_are_the_default_and_streaming_is_opt_in() {
        // JSON replies are the default — streaming stays off unless asked.
        let cli = Cli::try_parse_from(["oas2mcp"]).expect("bare invocation parses");
        assert!(!cli.stream_responses);

        // The flag opts into SSE streaming.
        let cli = Cli::try_parse_from(["oas2mcp", "--stream-responses"]).expect("bare flag parses");
        assert!(cli.stream_responses);
    }

    /// The document-fetch OAuth flags, plus whatever the test adds.
    fn oauth_args(extra: &[&str]) -> Vec<String> {
        let mut args = vec![
            "oas2mcp".to_string(),
            "--openapi-oauth-token-url".to_string(),
            "https://idp.example.com/token".to_string(),
        ];
        args.extend(extra.iter().map(|arg| (*arg).to_string()));
        args
    }

    #[test]
    fn oauth_token_url_requires_a_client_id_and_one_credential() {
        // token-url alone is incomplete: it needs a client id *and* a credential.
        let err = Cli::try_parse_from(oauth_args(&[]))
            .expect_err("token-url without credentials must fail");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);

        // A client id with no credential is still incomplete — this is the case
        // the old `client-id requires client-secret` chain used to catch.
        let err = Cli::try_parse_from(oauth_args(&["--openapi-oauth-client-id", "id"]))
            .expect_err("a client id alone must fail");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn either_client_credential_completes_the_oauth_config() {
        Cli::try_parse_from(oauth_args(&[
            "--openapi-oauth-client-id",
            "id",
            "--openapi-oauth-client-secret",
            "secret",
        ]))
        .expect("a client secret completes the config");

        Cli::try_parse_from(oauth_args(&[
            "--openapi-oauth-client-id",
            "id",
            "--openapi-oauth-private-key",
            "/keys/client.pem",
        ]))
        .expect("a private key completes the config");
    }

    #[test]
    fn the_two_client_credentials_are_mutually_exclusive() {
        let err = Cli::try_parse_from(oauth_args(&[
            "--openapi-oauth-client-id",
            "id",
            "--openapi-oauth-client-secret",
            "secret",
            "--openapi-oauth-private-key",
            "/keys/client.pem",
        ]))
        .expect_err("a secret and a key together must be rejected");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn the_assertion_flags_need_a_private_key() {
        // On their own, clap rejects them: they only mean something for
        // `private_key_jwt`.
        for (flag, value) in [
            ("--openapi-oauth-key-id", "kid-1"),
            ("--openapi-oauth-assertion-audience", "https://idp/"),
            ("--openapi-oauth-assertion-lifetime", "30s"),
        ] {
            let err = Cli::try_parse_from(["oas2mcp", flag, value])
                .expect_err("{flag} without a private key must be rejected");
            assert_eq!(
                err.kind(),
                clap::error::ErrorKind::MissingRequiredArgument,
                "{flag}"
            );
        }
    }

    #[test]
    fn an_assertion_flag_beside_a_client_secret_parses() {
        // clap suppresses a `requires` whose target conflicts with an argument
        // that *is* present: `--openapi-oauth-private-key` is mutually exclusive
        // with the secret, so the requirement is excused rather than reported.
        // The flag would therefore be silently ineffective — `oauth` warns about
        // it instead (see `oauth::ineffective_assertion_flags`).
        Cli::try_parse_from(oauth_args(&[
            "--openapi-oauth-client-id",
            "id",
            "--openapi-oauth-client-secret",
            "secret",
            "--openapi-oauth-key-id",
            "kid-1",
        ]))
        .expect("clap accepts this combination; the config layer warns");
    }

    #[test]
    fn the_signing_algorithm_defaults_to_rs256_and_parses_the_offered_names() {
        let cli = Cli::try_parse_from(["oas2mcp"]).expect("bare invocation parses");
        assert_eq!(cli.openapi_oauth_signing_alg, SigningAlg::Rs256);

        // `default_value_t` renders through `Display`, so the name it prints has
        // to be a name it can also parse back.
        for alg in [
            SigningAlg::Rs256,
            SigningAlg::Rs384,
            SigningAlg::Rs512,
            SigningAlg::Ps256,
            SigningAlg::Ps384,
            SigningAlg::Ps512,
            SigningAlg::Es256,
            SigningAlg::Es384,
            SigningAlg::EdDsa,
        ] {
            let rendered = alg.to_string();
            let cli = Cli::try_parse_from(["oas2mcp", "--openapi-oauth-signing-alg", &rendered])
                .unwrap_or_else(|err| panic!("`{rendered}` must parse back: {err}"));
            assert_eq!(cli.openapi_oauth_signing_alg, alg, "{rendered}");
        }
    }

    #[test]
    fn symmetric_signing_algorithms_are_not_offered() {
        // RFC 7523 §2.2 permits a MAC, but an HMAC keyed on the client secret is
        // no better than sending the secret. Not offering it is deliberate.
        let err = Cli::try_parse_from(["oas2mcp", "--openapi-oauth-signing-alg", "hs256"])
            .expect_err("hs256 must not be accepted");
        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidValue);
    }
}
