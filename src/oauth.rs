//! OAuth 2.0 `client_credentials` token provider used to authenticate the
//! OpenAPI document fetch. A long-running server that reloads the document
//! periodically cannot rely on a static bearer token — it expires. This
//! provider obtains a token from the configured endpoint, caches it, and
//! refreshes it automatically shortly before it expires.

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

/// A cached access token and the instant past which it should be re-fetched.
struct CachedToken {
    access_token: String,
    refresh_at: Instant,
}

/// Static configuration of the `client_credentials` grant. Behind an `Arc` so
/// the provider is cheap to clone and the token cache is shared across clones
/// (the startup load and the reload loop reuse the same token).
struct Inner {
    client: reqwest::Client,
    token_url: Url,
    client_id: String,
    client_secret: String,
    scopes: Vec<String>,
    audience: Option<String>,
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
    /// Build the provider from the CLI, or `None` when no OAuth token URL is
    /// configured. `client` is shared with the document fetch so connection
    /// pooling and TLS config are reused.
    pub fn from_cli(cli: &Cli, client: reqwest::Client) -> anyhow::Result<Option<Self>> {
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
            inner: Arc::new(Inner {
                client,
                token_url,
                client_id,
                client_secret,
                scopes: cli.openapi_oauth_scopes.clone(),
                audience: cli.openapi_oauth_audience.clone(),
                cache: Mutex::new(None),
            }),
        }))
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
        let inner = &self.inner;
        tracing::debug!(token_url = %inner.token_url, "requesting OAuth client-credentials token");

        let mut form: Vec<(&str, String)> = vec![("grant_type", "client_credentials".to_string())];
        if !inner.scopes.is_empty() {
            form.push(("scope", inner.scopes.join(" ")));
        }
        if let Some(audience) = &inner.audience {
            form.push(("audience", audience.clone()));
        }

        let response = inner
            .client
            .post(inner.token_url.clone())
            // Client authentication via HTTP Basic, as recommended by RFC 6749.
            .basic_auth(&inner.client_id, Some(&inner.client_secret))
            .form(&form)
            .send()
            .await
            .with_context(|| format!("requesting token from {}", inner.token_url))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!(
                "token endpoint {} returned {status}: {body}",
                inner.token_url
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
}
