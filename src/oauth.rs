//! OAuth 2.0 `client_credentials` token provider used to authenticate the
//! OpenAPI document fetch. A long-running server that reloads the document
//! periodically cannot rely on a static bearer token — it expires. This
//! provider obtains a token from the configured endpoint, caches it, and
//! refreshes it automatically shortly before it expires.
//!
//! The provider is configured through a [`TokenConfig`] rather than reading the
//! [`Cli`] directly: the grant is a self-contained piece of configuration, and
//! keeping it that way is what makes the token request testable against a local
//! endpoint without fabricating a whole command line.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use serde::Deserialize;
use url::Url;

use crate::cli::Cli;

/// Refresh a token this long before its advertised expiry, to avoid racing a
/// fetch against the exact expiry instant (clock skew, request latency).
const REFRESH_MARGIN: Duration = Duration::from_secs(60);

/// Default token lifetime assumed when the token endpoint omits `expires_in`.
const DEFAULT_TTL: Duration = Duration::from_secs(3600);

/// Everything needed to run one `client_credentials` grant against one token
/// endpoint. Self-contained on purpose — see the module docs.
pub struct TokenConfig {
    pub token_url: Url,
    pub client_id: String,
    pub client_secret: String,
    pub scopes: Vec<String>,
    pub audience: Option<String>,
}

impl TokenConfig {
    /// Read the document-fetch grant off the CLI, or `None` when no OAuth token
    /// URL is configured.
    fn from_cli(cli: &Cli) -> anyhow::Result<Option<Self>> {
        let Some(token_url) = cli.openapi_oauth_token_url.clone() else {
            return Ok(None);
        };
        // clap enforces these via `requires`, but fail loudly rather than panic
        // if that ever changes.
        let client_id = cli
            .openapi_oauth_client_id
            .clone()
            .context("--openapi-oauth-client-id is required with --openapi-oauth-token-url")?;
        let client_secret = cli
            .openapi_oauth_client_secret
            .clone()
            .context("--openapi-oauth-client-secret is required with --openapi-oauth-token-url")?;

        Ok(Some(Self {
            token_url,
            client_id,
            client_secret,
            scopes: cli.openapi_oauth_scopes.clone(),
            audience: cli.openapi_oauth_audience.clone(),
        }))
    }
}

/// A cached access token and the instant past which it should be re-fetched.
struct CachedToken {
    access_token: String,
    refresh_at: Instant,
}

/// Behind an `Arc` so the provider is cheap to clone and the token cache is
/// shared across clones (the startup load and the reload loop reuse the same
/// token).
struct Inner {
    client: reqwest::Client,
    config: TokenConfig,
    cache: Mutex<Option<CachedToken>>,
}

/// Issues and caches bearer tokens for the document fetch.
#[derive(Clone)]
pub struct TokenProvider {
    inner: Arc<Inner>,
}

/// The subset of an RFC 6749 token response we care about.
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    /// Lifetime in seconds. Optional per the spec; some providers omit it.
    expires_in: Option<u64>,
}

impl TokenProvider {
    /// Build a provider for `config`. `client` is shared with the document
    /// fetch so connection pooling and TLS config are reused.
    pub fn new(config: TokenConfig, client: reqwest::Client) -> Self {
        Self {
            inner: Arc::new(Inner {
                client,
                config,
                cache: Mutex::new(None),
            }),
        }
    }

    /// Build the provider from the CLI, or `None` when no OAuth token URL is
    /// configured.
    pub fn from_cli(cli: &Cli, client: reqwest::Client) -> anyhow::Result<Option<Self>> {
        Ok(TokenConfig::from_cli(cli)?.map(|config| Self::new(config, client)))
    }

    /// Return a valid access token, fetching a fresh one when the cache is
    /// empty or the current token is within the refresh margin of expiry.
    pub async fn access_token(&self) -> anyhow::Result<String> {
        // Fast path: a still-valid cached token. The lock is never held across
        // the network request below.
        if let Some(token) = self.cached() {
            return Ok(token);
        }

        let fresh = self.request_token().await?;
        let token = fresh.access_token.clone();
        self.inner
            .cache
            .lock()
            .expect("token cache mutex poisoned")
            .replace(fresh);
        Ok(token)
    }

    /// The cached token if it is still comfortably valid, else `None`.
    fn cached(&self) -> Option<String> {
        let cache = self.inner.cache.lock().expect("token cache mutex poisoned");
        let token = cache.as_ref()?;
        (Instant::now() < token.refresh_at).then(|| token.access_token.clone())
    }

    /// POST the `client_credentials` grant to the token endpoint and parse the
    /// response into a [`CachedToken`].
    async fn request_token(&self) -> anyhow::Result<CachedToken> {
        let config = &self.inner.config;
        tracing::debug!(token_url = %config.token_url, "requesting OAuth client-credentials token");

        let mut form: Vec<(&str, String)> = vec![("grant_type", "client_credentials".to_string())];
        if !config.scopes.is_empty() {
            form.push(("scope", config.scopes.join(" ")));
        }
        if let Some(audience) = &config.audience {
            form.push(("audience", audience.clone()));
        }

        let response = self
            .inner
            .client
            .post(config.token_url.clone())
            // Client authentication via HTTP Basic, as recommended by RFC 6749.
            .basic_auth(&config.client_id, Some(&config.client_secret))
            .form(&form)
            .send()
            .await
            .with_context(|| format!("requesting token from {}", config.token_url))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!(
                "token endpoint {} returned {status}: {body}",
                config.token_url
            );
        }

        let token: TokenResponse = response
            .json()
            .await
            .context("parsing the OAuth token response")?;

        let ttl = token.expires_in.map(Duration::from_secs);
        Ok(CachedToken {
            access_token: token.access_token,
            refresh_at: Instant::now() + trust_window(ttl),
        })
    }
}

/// How long a freshly issued token should be trusted: its lifetime minus the
/// refresh margin, floored at one second so a very short-lived token still
/// makes progress instead of being considered immediately stale.
fn trust_window(ttl: Option<Duration>) -> Duration {
    let ttl = ttl.unwrap_or(DEFAULT_TTL);
    ttl.checked_sub(REFRESH_MARGIN)
        .unwrap_or(Duration::ZERO)
        .max(Duration::from_secs(1))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::Router;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::post;

    use super::*;

    #[test]
    fn trust_window_subtracts_the_refresh_margin() {
        assert_eq!(
            trust_window(Some(Duration::from_secs(3600))),
            Duration::from_secs(3540)
        );
    }

    #[test]
    fn trust_window_defaults_when_expiry_absent() {
        assert_eq!(trust_window(None), DEFAULT_TTL - REFRESH_MARGIN);
    }

    #[test]
    fn trust_window_floors_short_lived_tokens_at_one_second() {
        // A TTL at or below the margin would underflow; it floors to 1s.
        assert_eq!(
            trust_window(Some(Duration::from_secs(30))),
            Duration::from_secs(1)
        );
        assert_eq!(
            trust_window(Some(Duration::from_secs(60))),
            Duration::from_secs(1)
        );
    }

    /// What the fake authorization server should answer.
    #[derive(Clone)]
    enum Reply {
        /// A well-formed token response, with or without `expires_in`.
        Token {
            access_token: &'static str,
            expires_in: Option<u64>,
        },
        /// An OAuth error response.
        Status(StatusCode, &'static str),
        /// A 200 whose body is not a token response.
        Undecodable,
    }

    /// One request as the fake authorization server saw it.
    #[derive(Clone)]
    struct Seen {
        /// The scheme only — never the credential itself. Asserting on
        /// `Basic <base64>` in full would put a credential in the test output.
        auth_scheme: Option<String>,
        body: String,
    }

    #[derive(Clone)]
    struct AsState {
        reply: Reply,
        calls: Arc<AtomicUsize>,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    /// A fake authorization server: its URL plus what it observed.
    struct FakeAs {
        token_url: Url,
        calls: Arc<AtomicUsize>,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl FakeAs {
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn last_seen(&self) -> Seen {
            self.seen
                .lock()
                .expect("seen mutex poisoned")
                .last()
                .expect("the endpoint was called at least once")
                .clone()
        }
    }

    /// `body: String` must stay last — it consumes the request body.
    async fn token_endpoint(
        State(state): State<AsState>,
        headers: HeaderMap,
        body: String,
    ) -> Response {
        state.calls.fetch_add(1, Ordering::SeqCst);
        let auth_scheme = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split_whitespace().next())
            .map(str::to_string);
        state
            .seen
            .lock()
            .expect("seen mutex poisoned")
            .push(Seen { auth_scheme, body });

        match state.reply {
            Reply::Token {
                access_token,
                expires_in,
            } => {
                let mut body = serde_json::json!({
                    "access_token": access_token,
                    "token_type": "Bearer",
                });
                if let Some(ttl) = expires_in {
                    body["expires_in"] = ttl.into();
                }
                axum::Json(body).into_response()
            }
            Reply::Status(status, text) => (status, text).into_response(),
            Reply::Undecodable => "not a token response".into_response(),
        }
    }

    /// Start a fake authorization server on an ephemeral port. The task is
    /// dropped with the test's runtime, so there is nothing to tear down.
    async fn spawn_as(reply: Reply) -> FakeAs {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/token", post(token_endpoint))
            .with_state(AsState {
                reply,
                calls: calls.clone(),
                seen: seen.clone(),
            });

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("binding the fake authorization server");
        let addr = listener
            .local_addr()
            .expect("local address of the listener");
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serving the fake authorization server");
        });

        FakeAs {
            token_url: Url::parse(&format!("http://{addr}/token"))
                .expect("the fake endpoint URL parses"),
            calls,
            seen,
        }
    }

    fn provider_for(fake: &FakeAs, scopes: &[&str], audience: Option<&str>) -> TokenProvider {
        TokenProvider::new(
            TokenConfig {
                token_url: fake.token_url.clone(),
                client_id: "test-client".into(),
                client_secret: "test-secret".into(),
                scopes: scopes.iter().map(|s| (*s).to_string()).collect(),
                audience: audience.map(str::to_string),
            },
            reqwest::Client::new(),
        )
    }

    #[tokio::test]
    async fn requests_a_token_and_sends_the_client_credentials_grant() {
        let fake = spawn_as(Reply::Token {
            access_token: "token-1",
            expires_in: Some(3600),
        })
        .await;
        let provider = provider_for(&fake, &["read:openapi", "read:more"], Some("api://target"));

        let token = provider.access_token().await.expect("the token is issued");
        assert_eq!(token, "token-1");
        assert_eq!(fake.calls(), 1);

        let seen = fake.last_seen();
        // Client authentication is HTTP Basic today; pinning it here means the
        // RFC 7523 `private_key_jwt` work has to state its intent explicitly.
        assert_eq!(seen.auth_scheme.as_deref(), Some("Basic"));
        assert!(
            seen.body.contains("grant_type=client_credentials"),
            "body: {}",
            seen.body
        );
        // Scopes go out space-joined (form encoding turns the space into `+`).
        assert!(
            seen.body.contains("scope=read%3Aopenapi+read%3Amore"),
            "body: {}",
            seen.body
        );
        assert!(
            seen.body.contains("audience=api%3A%2F%2Ftarget"),
            "body: {}",
            seen.body
        );
    }

    #[tokio::test]
    async fn omits_scope_and_audience_when_unset() {
        let fake = spawn_as(Reply::Token {
            access_token: "token-1",
            expires_in: None,
        })
        .await;
        let provider = provider_for(&fake, &[], None);

        provider.access_token().await.expect("the token is issued");
        let body = fake.last_seen().body;
        assert_eq!(body, "grant_type=client_credentials");
    }

    #[tokio::test]
    async fn reuses_the_cached_token_across_clones() {
        let fake = spawn_as(Reply::Token {
            access_token: "token-1",
            expires_in: Some(3600),
        })
        .await;
        let provider = provider_for(&fake, &[], None);

        assert_eq!(
            provider.access_token().await.expect("first token"),
            "token-1"
        );
        // A clone shares the cache — this is what keeps the reload loop from
        // hammering the token endpoint on every tick.
        let clone = provider.clone();
        assert_eq!(clone.access_token().await.expect("cached token"), "token-1");
        assert_eq!(fake.calls(), 1, "the second call must be served from cache");
    }

    #[tokio::test]
    async fn refetches_once_the_cached_token_is_stale() {
        let fake = spawn_as(Reply::Token {
            access_token: "token-1",
            expires_in: Some(3600),
        })
        .await;
        let provider = provider_for(&fake, &[], None);
        provider.access_token().await.expect("first token");
        assert_eq!(fake.calls(), 1);

        // Age the cached token instead of sleeping through a real TTL: the
        // refresh decision is `Instant::now() < refresh_at`, so backdating
        // `refresh_at` is exactly the state the provider would reach on its own.
        provider
            .inner
            .cache
            .lock()
            .expect("token cache mutex poisoned")
            .as_mut()
            .expect("a token was cached")
            .refresh_at = Instant::now() - Duration::from_secs(1);

        provider.access_token().await.expect("refreshed token");
        assert_eq!(fake.calls(), 2, "a stale token must trigger a re-fetch");
    }

    #[tokio::test]
    async fn surfaces_the_status_and_body_of_a_rejected_grant() {
        let fake = spawn_as(Reply::Status(
            StatusCode::UNAUTHORIZED,
            r#"{"error":"invalid_client"}"#,
        ))
        .await;
        let provider = provider_for(&fake, &[], None);

        let err = provider
            .access_token()
            .await
            .expect_err("a 401 must not yield a token");
        let message = format!("{err:#}");
        assert!(message.contains("401"), "{message}");
        // The provider's error carries the AS's own diagnosis, which is the
        // only way to tell a bad secret from a bad audience from the logs.
        assert!(message.contains("invalid_client"), "{message}");
    }

    #[tokio::test]
    async fn rejects_a_response_that_is_not_a_token() {
        let fake = spawn_as(Reply::Undecodable).await;
        let provider = provider_for(&fake, &[], None);

        let err = provider
            .access_token()
            .await
            .expect_err("an undecodable body must not yield a token");
        assert!(
            format!("{err:#}").contains("parsing the OAuth token response"),
            "{err:#}"
        );
    }

    #[tokio::test]
    async fn a_failed_request_leaves_the_cache_empty() {
        let fake = spawn_as(Reply::Status(StatusCode::BAD_REQUEST, "nope")).await;
        let provider = provider_for(&fake, &[], None);

        provider
            .access_token()
            .await
            .expect_err("first attempt fails");
        provider
            .access_token()
            .await
            .expect_err("second attempt fails");
        // Nothing was cached, so the second call really did retry rather than
        // replaying a half-built entry.
        assert_eq!(fake.calls(), 2);
        assert!(provider.cached().is_none());
    }

    #[test]
    fn from_cli_is_none_without_a_token_url() {
        use clap::Parser as _;

        let cli = Cli::try_parse_from(["oas2mcp"]).expect("bare invocation parses");
        let provider = TokenProvider::from_cli(&cli, reqwest::Client::new())
            .expect("no OAuth config is not an error");
        assert!(provider.is_none());
    }

    #[test]
    fn from_cli_reads_the_whole_grant() {
        use clap::Parser as _;

        let cli = Cli::try_parse_from([
            "oas2mcp",
            "--openapi-oauth-token-url",
            "https://idp.example.com/token",
            "--openapi-oauth-client-id",
            "id",
            "--openapi-oauth-client-secret",
            "secret",
            "--openapi-oauth-scope",
            "read:openapi",
            "--openapi-oauth-audience",
            "api://target",
        ])
        .expect("complete OAuth config parses");

        let config = TokenConfig::from_cli(&cli)
            .expect("the grant is well-formed")
            .expect("a token URL was given");
        assert_eq!(config.token_url.as_str(), "https://idp.example.com/token");
        assert_eq!(config.client_id, "id");
        assert_eq!(config.scopes, vec!["read:openapi".to_string()]);
        assert_eq!(config.audience.as_deref(), Some("api://target"));
    }
}
