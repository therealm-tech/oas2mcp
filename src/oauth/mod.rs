//! OAuth 2.0 `client_credentials` token provider used to authenticate the
//! OpenAPI document fetch. A long-running server that reloads the document
//! periodically cannot rely on a static bearer token — it expires. This
//! provider obtains a token from the configured endpoint, caches it, and
//! refreshes it automatically shortly before it expires.
//!
//! The client authenticates to the token endpoint in one of two ways:
//!
//! - a shared `client_secret` over HTTP Basic (RFC 6749 §2.3.1);
//! - a JWT assertion signed with the client's private key
//!   (`private_key_jwt`, RFC 7523 §2.2), for providers that will not issue a
//!   shared secret, or to keep the credential out of the environment entirely.
//!
//! The provider is configured through a [`TokenConfig`] rather than reading the
//! [`Cli`] directly: the grant is a self-contained piece of configuration, and
//! keeping it that way is what makes the token request testable against a local
//! endpoint without fabricating a whole command line.

mod assertion;
mod key;
mod provider;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, bail};
use url::Url;

use crate::cli::{Cli, SigningAlg};

pub use assertion::AssertionConfig;
pub use key::SigningKey;
pub use provider::TokenProvider;

/// Lifetime of a client assertion when none is configured. Short on purpose: a
/// fresh assertion is minted per token request, so a longer window buys nothing
/// and only widens the replay opportunity if one leaks.
const DEFAULT_ASSERTION_LIFETIME: Duration = Duration::from_secs(60);

/// How the client authenticates itself to the token endpoint.
pub enum ClientAuth {
    /// RFC 6749 §2.3.1 — a shared secret, sent over HTTP Basic.
    Secret(String),
    /// RFC 7523 §2.2 — a JWT assertion signed with the client's private key.
    PrivateKeyJwt {
        assertion: AssertionConfig,
        key: SigningKey,
    },
}

/// Everything needed to run one `client_credentials` grant against one token
/// endpoint. Self-contained on purpose — see the module docs.
pub struct TokenConfig {
    pub token_url: Url,
    pub client_id: String,
    pub client_auth: ClientAuth,
    pub scopes: Vec<String>,
    pub audience: Option<String>,
}

/// The flags describing one grant, gathered so the shared construction below is
/// written once and fed twice: for the document fetch and for the upstream API.
struct GrantFlags<'a> {
    /// Flag prefix (e.g. `--openapi-oauth`), so an error names the flag the
    /// operator actually typed rather than a generic one.
    prefix: &'static str,
    token_url: Url,
    client_id: Option<&'a String>,
    client_secret: Option<&'a String>,
    private_key: Option<&'a PathBuf>,
    key_id: Option<&'a String>,
    signing_alg: SigningAlg,
    assertion_audience: Option<&'a String>,
    assertion_lifetime: Option<Duration>,
    scopes: &'a [String],
    audience: Option<&'a String>,
}

/// The document-fetch grant's flags, or `None` when no token URL is configured.
fn document_flags(cli: &Cli) -> Option<GrantFlags<'_>> {
    Some(GrantFlags {
        prefix: "--openapi-oauth",
        token_url: cli.openapi_oauth_token_url.clone()?,
        client_id: cli.openapi_oauth_client_id.as_ref(),
        client_secret: cli.openapi_oauth_client_secret.as_ref(),
        private_key: cli.openapi_oauth_private_key.as_ref(),
        key_id: cli.openapi_oauth_key_id.as_ref(),
        signing_alg: cli.openapi_oauth_signing_alg,
        assertion_audience: cli.openapi_oauth_assertion_audience.as_ref(),
        assertion_lifetime: cli.openapi_oauth_assertion_lifetime,
        scopes: &cli.openapi_oauth_scopes,
        audience: cli.openapi_oauth_audience.as_ref(),
    })
}

/// The upstream-API grant's flags, or `None` when no token URL is configured.
fn upstream_flags(cli: &Cli) -> Option<GrantFlags<'_>> {
    Some(GrantFlags {
        prefix: "--upstream-oauth",
        token_url: cli.upstream_oauth_token_url.clone()?,
        client_id: cli.upstream_oauth_client_id.as_ref(),
        client_secret: cli.upstream_oauth_client_secret.as_ref(),
        private_key: cli.upstream_oauth_private_key.as_ref(),
        key_id: cli.upstream_oauth_key_id.as_ref(),
        signing_alg: cli.upstream_oauth_signing_alg,
        assertion_audience: cli.upstream_oauth_assertion_audience.as_ref(),
        assertion_lifetime: cli.upstream_oauth_assertion_lifetime,
        scopes: &cli.upstream_oauth_scopes,
        audience: cli.upstream_oauth_audience.as_ref(),
    })
}

impl TokenConfig {
    /// The document-fetch grant, or `None` when no token URL is configured.
    fn for_document(cli: &Cli) -> anyhow::Result<Option<Self>> {
        document_flags(cli).map(Self::from_flags).transpose()
    }

    /// The upstream-API grant, or `None` when no token URL is configured.
    fn for_upstream(cli: &Cli) -> anyhow::Result<Option<Self>> {
        upstream_flags(cli).map(Self::from_flags).transpose()
    }

    /// Resolve one grant's flags into a usable configuration, loading the signing
    /// key when the client authenticates with an assertion.
    fn from_flags(flags: GrantFlags<'_>) -> anyhow::Result<Self> {
        let prefix = flags.prefix;
        // clap enforces this via `requires_all`, but fail loudly rather than
        // panic if that ever changes.
        let client_id = flags
            .client_id
            .cloned()
            .with_context(|| format!("{prefix}-client-id is required with {prefix}-token-url"))?;

        let client_auth = match (flags.private_key, flags.client_secret) {
            // clap's arg group rejects this pair; refuse it here too rather than
            // silently picking one credential over the other.
            (Some(_), Some(_)) => bail!(
                "{prefix}-private-key and {prefix}-client-secret are mutually exclusive; \
                 pick one way to authenticate the client"
            ),
            (Some(path), None) => {
                let key = key::load(path, flags.signing_alg, flags.key_id.cloned())
                    .context("loading the OAuth client signing key")?;
                ClientAuth::PrivateKeyJwt {
                    assertion: AssertionConfig {
                        // RFC 7523 §3: for client authentication the assertion
                        // is issued by, and speaks for, the client itself.
                        issuer: client_id.clone(),
                        subject: client_id.clone(),
                        audience: flags
                            .assertion_audience
                            .cloned()
                            .unwrap_or_else(|| flags.token_url.to_string()),
                        lifetime: flags
                            .assertion_lifetime
                            .unwrap_or(DEFAULT_ASSERTION_LIFETIME),
                    },
                    key,
                }
            }
            (None, Some(secret)) => {
                // clap excuses `requires = <prefix>-private-key` on these flags
                // when a conflicting argument (the secret) is present, so they
                // reach us silently ineffective. Say so rather than let the
                // operator believe a `kid` is going out on the wire.
                let ignored = ineffective_assertion_flags(&flags);
                if !ignored.is_empty() {
                    tracing::warn!(
                        flags = ignored.join(", "),
                        "these flags only apply to {prefix}-private-key and are ignored with \
                         {prefix}-client-secret"
                    );
                }
                ClientAuth::Secret(secret.clone())
            }
            (None, None) => bail!(
                "{prefix}-token-url needs a client credential; pass \
                 {prefix}-client-secret or {prefix}-private-key"
            ),
        };

        Ok(Self {
            token_url: flags.token_url,
            client_id,
            client_auth,
            scopes: flags.scopes.to_vec(),
            audience: flags.audience.cloned(),
        })
    }
}

/// The assertion-only flags that are set but cannot apply, because the client
/// authenticates with a shared secret instead of a signed assertion.
///
/// The signing algorithm is left out on purpose: it carries a default, so it is
/// always "set" and reporting it would cry wolf on every run.
fn ineffective_assertion_flags(flags: &GrantFlags<'_>) -> Vec<String> {
    let prefix = flags.prefix;
    let mut ignored = Vec::new();
    if flags.key_id.is_some() {
        ignored.push(format!("{prefix}-key-id"));
    }
    if flags.assertion_audience.is_some() {
        ignored.push(format!("{prefix}-assertion-audience"));
    }
    if flags.assertion_lifetime.is_some() {
        ignored.push(format!("{prefix}-assertion-lifetime"));
    }
    ignored
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;

    fn cli_from(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("oas2mcp").chain(args.iter().copied()))
            .expect("CLI parses")
    }

    #[test]
    fn no_token_url_means_no_grant() {
        let config =
            TokenConfig::for_document(&cli_from(&[])).expect("no OAuth config is not an error");
        assert!(config.is_none());
    }

    #[test]
    fn a_client_secret_authenticates_over_basic() {
        let cli = cli_from(&[
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
        ]);
        let config = TokenConfig::for_document(&cli)
            .expect("the grant is well-formed")
            .expect("a token URL was given");

        assert_eq!(config.token_url.as_str(), "https://idp.example.com/token");
        assert_eq!(config.client_id, "id");
        assert_eq!(config.scopes, vec!["read:openapi".to_string()]);
        assert_eq!(config.audience.as_deref(), Some("api://target"));
        assert!(matches!(config.client_auth, ClientAuth::Secret(_)));
    }

    #[test]
    fn a_private_key_defaults_the_assertion_to_the_token_endpoint() {
        let cli = cli_from(&[
            "--openapi-oauth-token-url",
            "https://idp.example.com/token",
            "--openapi-oauth-client-id",
            "client-abc",
            "--openapi-oauth-private-key",
            "tests/fixtures/test_rsa_key.pem",
        ]);
        let config = TokenConfig::for_document(&cli)
            .expect("the grant is well-formed")
            .expect("a token URL was given");

        let ClientAuth::PrivateKeyJwt { assertion, .. } = &config.client_auth else {
            panic!("a private key must select private_key_jwt");
        };
        assert_eq!(assertion.issuer, "client-abc");
        assert_eq!(assertion.subject, "client-abc");
        // Unset `aud` follows the token endpoint, which is what most providers
        // expect and saves an obligatory flag.
        assert_eq!(assertion.audience, "https://idp.example.com/token");
        assert_eq!(assertion.lifetime, DEFAULT_ASSERTION_LIFETIME);
    }

    #[test]
    fn the_assertion_audience_and_lifetime_can_be_overridden() {
        let cli = cli_from(&[
            "--openapi-oauth-token-url",
            "https://idp.example.com/token",
            "--openapi-oauth-client-id",
            "client-abc",
            "--openapi-oauth-private-key",
            "tests/fixtures/test_rsa_key.pem",
            "--openapi-oauth-assertion-audience",
            "https://idp.example.com/",
            "--openapi-oauth-assertion-lifetime",
            "30s",
        ]);
        let config = TokenConfig::for_document(&cli)
            .expect("the grant is well-formed")
            .expect("a token URL was given");

        let ClientAuth::PrivateKeyJwt { assertion, .. } = &config.client_auth else {
            panic!("a private key must select private_key_jwt");
        };
        assert_eq!(assertion.audience, "https://idp.example.com/");
        assert_eq!(assertion.lifetime, Duration::from_secs(30));
    }

    #[test]
    fn assertion_flags_are_reported_as_ineffective_beside_a_client_secret() {
        let cli = cli_from(&[
            "--openapi-oauth-token-url",
            "https://idp.example.com/token",
            "--openapi-oauth-client-id",
            "id",
            "--openapi-oauth-client-secret",
            "secret",
            "--openapi-oauth-key-id",
            "kid-1",
        ]);
        assert_eq!(
            ineffective_assertion_flags(&document_flags(&cli).expect("a token url is set")),
            vec!["--openapi-oauth-key-id"]
        );

        // Nothing to report when no assertion flag was given. `signing-alg`
        // carries a default and must never show up here.
        let cli = cli_from(&[
            "--openapi-oauth-token-url",
            "https://idp.example.com/token",
            "--openapi-oauth-client-id",
            "id",
            "--openapi-oauth-client-secret",
            "secret",
            "--openapi-oauth-signing-alg",
            "es256",
        ]);
        assert!(
            ineffective_assertion_flags(&document_flags(&cli).expect("a token url is set"))
                .is_empty()
        );
    }

    #[test]
    fn an_unreadable_private_key_fails_the_whole_config() {
        let cli = cli_from(&[
            "--openapi-oauth-token-url",
            "https://idp.example.com/token",
            "--openapi-oauth-client-id",
            "client-abc",
            "--openapi-oauth-private-key",
            "/definitely/not/a/key.pem",
        ]);
        // Better to refuse to start than to discover at the first reload that
        // the document fetch can never authenticate. (`err()` rather than
        // `expect_err`: `TokenConfig` holds a `SigningKey`, which has no `Debug`
        // impl on purpose.)
        let err = TokenConfig::for_document(&cli)
            .err()
            .expect("a missing key must fail");
        assert!(
            format!("{err:#}").contains("loading the OAuth client signing key"),
            "{err:#}"
        );
    }
}
