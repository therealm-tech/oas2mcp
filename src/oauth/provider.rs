//! The token provider itself: issues, caches and refreshes the access token,
//! whichever way the client authenticates. See the module docs of the parent
//! for the shape of the configuration.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use serde::Deserialize;

use super::{
    AssertionConfig, ClientAuth, Grant, GrantAssertion, SubjectSource, TokenConfig, assertion,
};
use crate::cli::Cli;

/// Refresh a token this long before its advertised expiry, to avoid racing a
/// fetch against the exact expiry instant (clock skew, request latency).
const REFRESH_MARGIN: Duration = Duration::from_secs(60);

/// Default token lifetime assumed when the token endpoint omits `expires_in`.
const DEFAULT_TTL: Duration = Duration::from_secs(3600);

/// `client_assertion_type` naming a JWT bearer assertion, per RFC 7523 §2.2.
const CLIENT_ASSERTION_TYPE: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";

/// `grant_type` naming a JWT bearer authorization grant, per RFC 7523 §2.1.
const JWT_BEARER_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";

/// Most delegated tokens the cache will hold at once.
///
/// A per-caller cache grows with the number of distinct active users, so it needs
/// a ceiling or a busy multi-tenant server leaks memory one identity at a time.
/// Past the ceiling, expired entries are swept and, failing that, the entry
/// closest to expiry is evicted: the cache degrades into more token requests, not
/// into unbounded growth.
const MAX_CACHED_TOKENS: usize = 10_000;

/// Who a cached token belongs to.
#[derive(PartialEq, Eq, Hash, Clone, Debug)]
enum CacheKey {
    /// The client's own token: `client_credentials`, or a `jwt-bearer` grant with
    /// a fixed subject. One entry, shared by every caller.
    Client,
    /// A token obtained on behalf of one caller.
    ///
    /// The issuer is part of the key because `sub` is only unique **within** an
    /// issuer (OIDC Core §2). Keyed on the subject alone, two identity providers
    /// that both mint `sub: alice` would share one entry — and one tenant would
    /// be handed another tenant's upstream token.
    Delegated {
        issuer: Option<String>,
        subject: String,
    },
}

/// The verified caller a token is being obtained for.
pub struct Delegation<'a> {
    /// `iss` of the caller's token, part of the cache key.
    pub issuer: Option<&'a str>,
    /// The identity to act on behalf of, resolved from the configured claim.
    pub subject: &'a str,
    /// The caller's own `exp`. A delegated token is not cached past it: the
    /// caller's session ending should not leave a usable token behind.
    pub expiry: Option<Instant>,
    /// The caller's raw JWT, relayed verbatim by the `caller` assertion mode.
    pub token: &'a str,
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
    cache: Mutex<HashMap<CacheKey, CachedToken>>,
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
                cache: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Provider for the OpenAPI document fetch, or `None` when
    /// `--openapi-oauth-token-url` is unset.
    pub fn for_document(cli: &Cli, client: reqwest::Client) -> anyhow::Result<Option<Self>> {
        Ok(TokenConfig::for_document(cli)?.map(|config| Self::new(config, client)))
    }

    /// Provider for upstream API calls, or `None` when
    /// `--upstream-oauth-token-url` is unset.
    pub fn for_upstream(cli: &Cli, client: reqwest::Client) -> anyhow::Result<Option<Self>> {
        Ok(TokenConfig::for_upstream(cli)?.map(|config| Self::new(config, client)))
    }

    /// Whether this provider needs a verified caller identity on every call.
    pub fn needs_caller_identity(&self) -> bool {
        self.inner.config.needs_caller_identity()
    }

    /// A token for the client itself.
    pub async fn access_token(&self) -> anyhow::Result<String> {
        self.token_for(None).await
    }

    /// A token obtained on behalf of a verified caller.
    pub async fn delegated_token(&self, delegation: &Delegation<'_>) -> anyhow::Result<String> {
        self.token_for(Some(delegation)).await
    }

    /// Return a valid access token for `delegation` (or for the client itself
    /// when it is `None`), fetching a fresh one when nothing usable is cached.
    async fn token_for(&self, delegation: Option<&Delegation<'_>>) -> anyhow::Result<String> {
        let key = self.cache_key(delegation);

        // Fast path: a still-valid cached token. The lock is never held across
        // the network request below.
        if let Some(token) = self.cached(&key) {
            return Ok(token);
        }

        let fresh = self.request_token(delegation).await?;
        let token = fresh.access_token.clone();
        self.store(key, fresh);
        Ok(token)
    }

    /// The cache slot a token belongs in.
    ///
    /// A fixed-subject grant shares one slot: every caller gets the same token,
    /// because the assertion names the same subject regardless of who called.
    fn cache_key(&self, delegation: Option<&Delegation<'_>>) -> CacheKey {
        match (&self.inner.config.grant, delegation) {
            (Grant::ClientCredentials, _) => CacheKey::Client,
            (
                Grant::JwtBearer(GrantAssertion::SelfSigned {
                    subject: SubjectSource::Fixed(_),
                    ..
                }),
                _,
            ) => CacheKey::Client,
            (Grant::JwtBearer(_), Some(delegation)) => CacheKey::Delegated {
                issuer: delegation.issuer.map(str::to_string),
                subject: delegation.subject.to_string(),
            },
            // A delegating grant with no caller cannot mint anything; the request
            // below fails. Keying it as `Client` would be worse than useless — it
            // would let a failure share a slot with a real token.
            (Grant::JwtBearer(_), None) => CacheKey::Delegated {
                issuer: None,
                subject: String::new(),
            },
        }
    }

    /// The cached token for `key` if it is still comfortably valid, else `None`.
    fn cached(&self, key: &CacheKey) -> Option<String> {
        let cache = self.inner.cache.lock().expect("token cache mutex poisoned");
        let token = cache.get(key)?;
        (Instant::now() < token.refresh_at).then(|| token.access_token.clone())
    }

    /// Cache a freshly issued token, keeping the cache bounded.
    fn store(&self, key: CacheKey, token: CachedToken) {
        let mut cache = self.inner.cache.lock().expect("token cache mutex poisoned");
        if cache.len() >= MAX_CACHED_TOKENS && !cache.contains_key(&key) {
            let now = Instant::now();
            cache.retain(|_, cached| now < cached.refresh_at);
            // Still full of live tokens: give up the one that would have expired
            // first. Bounded memory beats a perfect hit rate.
            if cache.len() >= MAX_CACHED_TOKENS
                && let Some(soonest) = cache
                    .iter()
                    .min_by_key(|(_, cached)| cached.refresh_at)
                    .map(|(key, _)| key.clone())
            {
                tracing::debug!(
                    cached = cache.len(),
                    "delegated token cache is full; evicting the entry closest to expiry"
                );
                cache.remove(&soonest);
            }
        }
        cache.insert(key, token);
    }

    /// POST the configured grant to the token endpoint and parse the response
    /// into a [`CachedToken`].
    async fn request_token(
        &self,
        delegation: Option<&Delegation<'_>>,
    ) -> anyhow::Result<CachedToken> {
        let config = &self.inner.config;

        let mut form: Vec<(&str, String)> = Vec::new();
        match &config.grant {
            Grant::ClientCredentials => {
                tracing::debug!(token_url = %config.token_url, "requesting OAuth client-credentials token");
                form.push(("grant_type", "client_credentials".to_string()));
            }
            Grant::JwtBearer(source) => {
                let (assertion, subject) = self.grant_assertion(source, delegation)?;
                tracing::debug!(
                    token_url = %config.token_url,
                    subject = %subject,
                    "requesting OAuth jwt-bearer token",
                );
                form.push(("grant_type", JWT_BEARER_GRANT_TYPE.to_string()));
                form.push(("assertion", assertion));
            }
        }
        if !config.scopes.is_empty() {
            form.push(("scope", config.scopes.join(" ")));
        }
        if let Some(audience) = &config.audience {
            form.push(("audience", audience.clone()));
        }

        let mut request = self.inner.client.post(config.token_url.clone());
        match &config.client_auth {
            // HTTP Basic, as recommended by RFC 6749 §2.3.1.
            ClientAuth::Secret(secret) => {
                request = request.basic_auth(&config.client_id, Some(secret));
            }
            // RFC 7523 §2.2. `client_id` duplicates the assertion's `iss` and is
            // not required by the RFC, but several providers reject the request
            // without it — and sending it costs nothing.
            ClientAuth::PrivateKeyJwt { assertion, key } => {
                let signed = assertion::sign(assertion, key)
                    .context("signing the client assertion for the token request")?;
                form.push(("client_id", config.client_id.clone()));
                form.push(("client_assertion_type", CLIENT_ASSERTION_TYPE.to_string()));
                form.push(("client_assertion", signed));
            }
        }

        let response = request
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
        let mut refresh_at = Instant::now() + trust_window(ttl);
        // Never trust a delegated token past the caller session it was minted
        // for. Without this, revoking a user leaves a usable upstream token in
        // the cache until the *upstream* token expires, which can be much later.
        if let Some(caller_expiry) = delegation.and_then(|delegation| delegation.expiry) {
            refresh_at = refresh_at.min(caller_expiry);
        }
        Ok(CachedToken {
            access_token: token.access_token,
            refresh_at,
        })
    }
}

impl TokenProvider {
    /// Build the RFC 7523 §2.1 assertion presented as the grant, and report the
    /// subject it speaks for (for the log line).
    ///
    /// A delegating configuration with no verified caller is an error, never a
    /// fallback: quietly obtaining a token for the client itself would hand the
    /// least-authorized caller the broadest identity the server has.
    fn grant_assertion(
        &self,
        source: &GrantAssertion,
        delegation: Option<&Delegation<'_>>,
    ) -> anyhow::Result<(String, String)> {
        match source {
            GrantAssertion::Caller => {
                let delegation = delegation.context(
                    "the jwt-bearer grant relays the caller's token, but this call carries no \
                     verified caller identity",
                )?;
                Ok((delegation.token.to_string(), delegation.subject.to_string()))
            }
            GrantAssertion::SelfSigned {
                issuer,
                audience,
                lifetime,
                key,
                subject,
            } => {
                let subject = match subject {
                    SubjectSource::Fixed(fixed) => fixed.clone(),
                    SubjectSource::Caller => delegation
                        .context(
                            "the jwt-bearer grant acts on behalf of the caller, but this call \
                             carries no verified caller identity",
                        )?
                        .subject
                        .to_string(),
                };
                let config = AssertionConfig {
                    issuer: issuer.clone(),
                    subject: subject.clone(),
                    audience: audience.clone(),
                    lifetime: *lifetime,
                };
                let signed = assertion::sign(&config, key)
                    .context("signing the jwt-bearer grant assertion")?;
                Ok((signed, subject))
            }
        }
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
    use url::Url;

    use super::*;
    use crate::oauth::AssertionConfig;

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
        provider_with(
            fake,
            ClientAuth::Secret("test-secret".into()),
            scopes,
            audience,
        )
    }

    fn provider_with(
        fake: &FakeAs,
        client_auth: ClientAuth,
        scopes: &[&str],
        audience: Option<&str>,
    ) -> TokenProvider {
        TokenProvider::new(
            TokenConfig {
                token_url: fake.token_url.clone(),
                client_id: "test-client".into(),
                client_auth,
                grant: Grant::ClientCredentials,
                scopes: scopes.iter().map(|s| (*s).to_string()).collect(),
                audience: audience.map(str::to_string),
            },
            reqwest::Client::new(),
        )
    }

    /// Backdate a cached entry so the next call re-fetches. The refresh decision
    /// is `Instant::now() < refresh_at`, so this is exactly the state the
    /// provider would reach on its own — in 0 ms instead of a real TTL.
    fn age_out(provider: &TokenProvider, key: &CacheKey) {
        provider
            .inner
            .cache
            .lock()
            .expect("token cache mutex poisoned")
            .get_mut(key)
            .expect("a token was cached under this key")
            .refresh_at = Instant::now() - Duration::from_secs(1);
    }

    /// Pull one form field out of a captured request body.
    fn form_field(body: &str, name: &str) -> Option<String> {
        url::form_urlencoded::parse(body.as_bytes())
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
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
        // A shared secret authenticates over HTTP Basic, and nothing about the
        // client leaks into the form body.
        assert_eq!(seen.auth_scheme.as_deref(), Some("Basic"));
        assert!(form_field(&seen.body, "client_assertion").is_none());
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
        age_out(&provider, &CacheKey::Client);

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
        assert!(provider.cached(&CacheKey::Client).is_none());
    }

    /// A `private_key_jwt` client auth backed by the RSA fixture.
    fn private_key_jwt(audience: &str) -> ClientAuth {
        ClientAuth::PrivateKeyJwt {
            assertion: AssertionConfig {
                issuer: "test-client".into(),
                subject: "test-client".into(),
                audience: audience.to_string(),
                lifetime: Duration::from_secs(60),
            },
            key: Arc::new(
                super::super::key::load(
                    std::path::Path::new("tests/fixtures/test_rsa_key.pem"),
                    crate::cli::SigningAlg::Rs256,
                    Some("kid-1".to_string()),
                )
                .expect("the RSA fixture loads"),
            ),
        }
    }

    #[tokio::test]
    async fn private_key_jwt_sends_a_signed_assertion_instead_of_basic_auth() {
        let fake = spawn_as(Reply::Token {
            access_token: "token-1",
            expires_in: Some(3600),
        })
        .await;
        let audience = fake.token_url.to_string();
        let provider = provider_with(
            &fake,
            private_key_jwt(&audience),
            &["read:openapi"],
            Some("api://target"),
        );

        let token = provider.access_token().await.expect("the token is issued");
        assert_eq!(token, "token-1");

        let seen = fake.last_seen();
        // The whole point: no shared secret travels, so no Basic credential.
        assert!(
            seen.auth_scheme.is_none(),
            "auth scheme: {:?}",
            seen.auth_scheme
        );
        assert_eq!(
            form_field(&seen.body, "grant_type").as_deref(),
            Some("client_credentials"),
        );
        assert_eq!(
            form_field(&seen.body, "client_assertion_type").as_deref(),
            Some("urn:ietf:params:oauth:client-assertion-type:jwt-bearer"),
        );
        // Redundant with the assertion's `iss`, but several providers insist.
        assert_eq!(
            form_field(&seen.body, "client_id").as_deref(),
            Some("test-client"),
        );
        // Scope and audience still ride along — client authentication and the
        // grant are separate concerns, and swapping one must not disturb the
        // other. `audience` in particular is the provider-specific parameter
        // (Auth0 and friends) that decides *which API* the token is for.
        assert_eq!(
            form_field(&seen.body, "scope").as_deref(),
            Some("read:openapi"),
        );
        assert_eq!(
            form_field(&seen.body, "audience").as_deref(),
            Some("api://target"),
        );

        // The assertion the AS received must verify against the client's public
        // key and be addressed to the AS. Claim-level coverage lives in
        // `assertion`; this checks the wire actually carried a usable one.
        let sent = form_field(&seen.body, "client_assertion").expect("an assertion was sent");
        let key = jsonwebtoken::DecodingKey::from_rsa_components(RSA_N, "AQAB")
            .expect("public key builds");
        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
        validation.set_audience(&[audience.as_str()]);
        let claims = jsonwebtoken::decode::<serde_json::Value>(&sent, &key, &validation)
            .expect("the assertion verifies against the client's public key")
            .claims;
        assert_eq!(claims["iss"], "test-client");
        assert_eq!(
            jsonwebtoken::decode_header(&sent)
                .expect("header decodes")
                .kid
                .as_deref(),
            Some("kid-1")
        );
    }

    #[tokio::test]
    async fn a_fresh_assertion_is_minted_for_every_token_request() {
        let fake = spawn_as(Reply::Token {
            access_token: "token-1",
            expires_in: Some(3600),
        })
        .await;
        let audience = fake.token_url.to_string();
        let provider = provider_with(&fake, private_key_jwt(&audience), &[], None);

        provider.access_token().await.expect("first token");
        // Age the cache so the next call really goes back to the endpoint.
        age_out(&provider, &CacheKey::Client);
        provider.access_token().await.expect("refreshed token");

        let seen = fake.seen.lock().expect("seen mutex poisoned").clone();
        assert_eq!(seen.len(), 2);
        let first = form_field(&seen[0].body, "client_assertion").expect("first assertion");
        let second = form_field(&seen[1].body, "client_assertion").expect("second assertion");
        // Reusing an assertion is what an AS replay cache rejects, so the
        // provider must never cache one alongside the token.
        assert_ne!(first, second);
    }

    /// A `jwt-bearer` grant signed with the RSA fixture, acting for whoever the
    /// caller turns out to be.
    fn delegating_provider(fake: &FakeAs, subject: SubjectSource) -> TokenProvider {
        let key = Arc::new(
            super::super::key::load(
                std::path::Path::new("tests/fixtures/test_rsa_key.pem"),
                crate::cli::SigningAlg::Rs256,
                None,
            )
            .expect("the RSA fixture loads"),
        );
        TokenProvider::new(
            TokenConfig {
                token_url: fake.token_url.clone(),
                client_id: "test-client".into(),
                client_auth: ClientAuth::PrivateKeyJwt {
                    assertion: AssertionConfig {
                        issuer: "test-client".into(),
                        subject: "test-client".into(),
                        audience: fake.token_url.to_string(),
                        lifetime: Duration::from_secs(60),
                    },
                    key: Arc::clone(&key),
                },
                grant: Grant::JwtBearer(GrantAssertion::SelfSigned {
                    issuer: "test-client".into(),
                    audience: fake.token_url.to_string(),
                    lifetime: Duration::from_secs(60),
                    key,
                    subject,
                }),
                scopes: Vec::new(),
                audience: None,
            },
            reqwest::Client::new(),
        )
    }

    fn delegation<'a>(issuer: Option<&'a str>, subject: &'a str, token: &'a str) -> Delegation<'a> {
        Delegation {
            issuer,
            subject,
            expiry: None,
            token,
        }
    }

    /// Decode the assertion the fake AS received, without verifying it (the
    /// signature is covered in `assertion`).
    fn assertion_claims(body: &str) -> serde_json::Value {
        let assertion = form_field(body, "assertion").expect("an assertion was sent");
        let payload = assertion.split('.').nth(1).expect("a JWT has three parts");
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .expect("the payload is base64url");
        serde_json::from_slice(&bytes).expect("the payload is JSON")
    }

    #[tokio::test]
    async fn the_jwt_bearer_grant_carries_the_callers_subject() {
        let fake = spawn_as(Reply::Token {
            access_token: "alice-token",
            expires_in: Some(3600),
        })
        .await;
        let provider = delegating_provider(&fake, SubjectSource::Caller);

        let token = provider
            .delegated_token(&delegation(Some("https://idp/"), "alice", "caller-jwt"))
            .await
            .expect("a delegated token is issued");
        assert_eq!(token, "alice-token");

        let seen = fake.last_seen();
        assert_eq!(
            form_field(&seen.body, "grant_type").as_deref(),
            Some("urn:ietf:params:oauth:grant-type:jwt-bearer"),
        );
        let claims = assertion_claims(&seen.body);
        // `iss` is us, `sub` is the caller: that difference is the delegation.
        assert_eq!(claims["iss"], "test-client");
        assert_eq!(claims["sub"], "alice");
    }

    #[tokio::test]
    async fn two_callers_never_share_a_delegated_token() {
        let fake = spawn_as(Reply::Token {
            access_token: "a-token",
            expires_in: Some(3600),
        })
        .await;
        let provider = delegating_provider(&fake, SubjectSource::Caller);

        provider
            .delegated_token(&delegation(Some("https://idp/"), "alice", "jwt-a"))
            .await
            .expect("alice gets a token");
        provider
            .delegated_token(&delegation(Some("https://idp/"), "bob", "jwt-b"))
            .await
            .expect("bob gets a token");
        // Two distinct subjects must mean two token requests. Sharing one entry
        // here would mean handing Bob Alice's upstream access.
        assert_eq!(fake.calls(), 2);

        // And each is cached under its own key.
        provider
            .delegated_token(&delegation(Some("https://idp/"), "alice", "jwt-a"))
            .await
            .expect("alice is served from cache");
        assert_eq!(fake.calls(), 2);
    }

    #[tokio::test]
    async fn the_same_subject_from_two_issuers_never_shares_a_token() {
        let fake = spawn_as(Reply::Token {
            access_token: "a-token",
            expires_in: Some(3600),
        })
        .await;
        let provider = delegating_provider(&fake, SubjectSource::Caller);

        // `sub` is only unique within an issuer: `alice` at one provider is not
        // `alice` at another. This is the subtle half of the cache key.
        provider
            .delegated_token(&delegation(Some("https://idp-one/"), "alice", "jwt-1"))
            .await
            .expect("first tenant");
        provider
            .delegated_token(&delegation(Some("https://idp-two/"), "alice", "jwt-2"))
            .await
            .expect("second tenant");
        assert_eq!(fake.calls(), 2);
    }

    #[tokio::test]
    async fn a_fixed_subject_shares_one_token_across_callers() {
        let fake = spawn_as(Reply::Token {
            access_token: "service-token",
            expires_in: Some(3600),
        })
        .await;
        let provider = delegating_provider(&fake, SubjectSource::Fixed("service-acct".into()));
        assert!(!provider.needs_caller_identity());

        provider.access_token().await.expect("first call");
        provider.access_token().await.expect("second call");
        // The assertion names the same subject whoever calls, so one entry is
        // correct — and one token request is enough.
        assert_eq!(fake.calls(), 1);
        let claims = assertion_claims(&fake.last_seen().body);
        assert_eq!(claims["sub"], "service-acct");
    }

    #[tokio::test]
    async fn delegation_without_a_caller_is_refused_not_downgraded() {
        let fake = spawn_as(Reply::Token {
            access_token: "should-not-be-issued",
            expires_in: Some(3600),
        })
        .await;
        let provider = delegating_provider(&fake, SubjectSource::Caller);
        assert!(provider.needs_caller_identity());

        // The whole point: no caller means no token. Quietly falling back to the
        // client's own identity would hand this call the broadest access there is.
        let err = provider
            .access_token()
            .await
            .expect_err("a delegating grant cannot mint a token with no caller");
        assert!(
            format!("{err:#}").contains("no verified caller identity"),
            "{err:#}"
        );
        assert_eq!(fake.calls(), 0, "no request may reach the token endpoint");
    }

    #[tokio::test]
    async fn the_caller_assertion_mode_relays_the_callers_token() {
        let fake = spawn_as(Reply::Token {
            access_token: "exchanged",
            expires_in: Some(3600),
        })
        .await;
        let provider = TokenProvider::new(
            TokenConfig {
                token_url: fake.token_url.clone(),
                client_id: "test-client".into(),
                client_auth: ClientAuth::Secret("test-secret".into()),
                grant: Grant::JwtBearer(GrantAssertion::Caller),
                scopes: Vec::new(),
                audience: None,
            },
            reqwest::Client::new(),
        );

        provider
            .delegated_token(&delegation(
                Some("https://idp/"),
                "alice",
                "the.callers.jwt",
            ))
            .await
            .expect("the exchange succeeds");

        let seen = fake.last_seen();
        // Relayed verbatim — nothing is signed in this mode.
        assert_eq!(
            form_field(&seen.body, "assertion").as_deref(),
            Some("the.callers.jwt")
        );
    }

    #[tokio::test]
    async fn a_delegated_token_is_not_cached_past_the_callers_own_expiry() {
        let fake = spawn_as(Reply::Token {
            access_token: "alice-token",
            // An hour of upstream validity...
            expires_in: Some(3600),
        })
        .await;
        let provider = delegating_provider(&fake, SubjectSource::Caller);

        // ...but the caller's session ends in 5 seconds.
        let caller_expiry = Instant::now() + Duration::from_secs(5);
        provider
            .delegated_token(&Delegation {
                issuer: Some("https://idp/"),
                subject: "alice",
                expiry: Some(caller_expiry),
                token: "caller-jwt",
            })
            .await
            .expect("a delegated token is issued");

        let cache = provider
            .inner
            .cache
            .lock()
            .expect("token cache mutex poisoned");
        let cached = cache
            .get(&CacheKey::Delegated {
                issuer: Some("https://idp/".to_string()),
                subject: "alice".to_string(),
            })
            .expect("cached under the caller's key");
        // Revoking the user must not leave an hour of usable upstream access
        // sitting in our cache.
        assert!(
            cached.refresh_at <= caller_expiry,
            "the cached window must not outlive the caller's token"
        );
    }

    #[test]
    fn the_cache_stays_bounded_under_many_distinct_callers() {
        let provider = TokenProvider::new(
            TokenConfig {
                token_url: Url::parse("https://idp.example.com/token").expect("valid URL"),
                client_id: "test-client".into(),
                client_auth: ClientAuth::Secret("secret".into()),
                grant: Grant::ClientCredentials,
                scopes: Vec::new(),
                audience: None,
            },
            reqwest::Client::new(),
        );

        // Half live, half already stale, then push well past the ceiling: a
        // per-caller cache must not grow with the user count forever.
        for i in 0..(MAX_CACHED_TOKENS + 500) {
            let refresh_at = if i % 2 == 0 {
                Instant::now() + Duration::from_secs(3600)
            } else {
                Instant::now() - Duration::from_secs(1)
            };
            provider.store(
                CacheKey::Delegated {
                    issuer: Some("https://idp/".to_string()),
                    subject: format!("user-{i}"),
                },
                CachedToken {
                    access_token: format!("token-{i}"),
                    refresh_at,
                },
            );
        }

        let cached = provider
            .inner
            .cache
            .lock()
            .expect("token cache mutex poisoned")
            .len();
        assert!(
            cached <= MAX_CACHED_TOKENS,
            "cache grew to {cached}, past the {MAX_CACHED_TOKENS} ceiling"
        );
    }

    /// Modulus of the throwaway RSA fixture, to verify what the AS received.
    const RSA_N: &str = "pIrAmCcbgl0Z6Fmomx9TVpVhMiOjJOrtjzKHoKnV5pYyFz86Zpor4tHmK8inQB6ES7j2V-0cgnT-62g_wCCwJHS-jJY0GawNgkxPq_5zFSFBuhJjyGpQzofexEPP7Qof6ZQKRViNw5A64C-dkcgoixhOBS1TWk6mkDOgoYOv9q2IUM5saRYZIwQw7OU4hsKetZcq8gbmVSjbzPylFryaIu5Udlo4JxFt-7t0RG_N858nu6eBYR68KMlOZIqN4YsaaQBm6teCdOUUXxAww8Yuij0gbz_YXMSnu5A5Ooff8w83kQLJqPLJyyEb357CvCqZsDZmlp3LFVRRmNuDPUtTKQ";
}
