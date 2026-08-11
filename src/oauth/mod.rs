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

use std::time::Duration;

use anyhow::{Context as _, bail};
use url::Url;

use crate::cli::Cli;

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

impl TokenConfig {
    /// Read the document-fetch grant off the CLI, or `None` when no OAuth token
    /// URL is configured.
    fn from_cli(cli: &Cli) -> anyhow::Result<Option<Self>> {
        let Some(token_url) = cli.openapi_oauth_token_url.clone() else {
            return Ok(None);
        };
        // clap enforces this via `requires_all`, but fail loudly rather than
        // panic if that ever changes.
        let client_id = cli
            .openapi_oauth_client_id
            .clone()
            .context("--openapi-oauth-client-id is required with --openapi-oauth-token-url")?;

        let client_auth = match (
            &cli.openapi_oauth_private_key,
            &cli.openapi_oauth_client_secret,
        ) {
            // clap's arg group rejects this pair; refuse it here too rather than
            // silently picking one credential over the other.
            (Some(_), Some(_)) => bail!(
                "--openapi-oauth-private-key and --openapi-oauth-client-secret are mutually \
                 exclusive; pick one way to authenticate the client"
            ),
            (Some(path), None) => {
                let key = key::load(
                    path,
                    cli.openapi_oauth_signing_alg,
                    cli.openapi_oauth_key_id.clone(),
                )
                .context("loading the OAuth client signing key")?;
                ClientAuth::PrivateKeyJwt {
                    assertion: AssertionConfig {
                        // RFC 7523 §3: for client authentication the assertion
                        // is issued by, and speaks for, the client itself.
                        issuer: client_id.clone(),
                        subject: client_id.clone(),
                        audience: cli
                            .openapi_oauth_assertion_audience
                            .clone()
                            .unwrap_or_else(|| token_url.to_string()),
                        lifetime: cli
                            .openapi_oauth_assertion_lifetime
                            .unwrap_or(DEFAULT_ASSERTION_LIFETIME),
                    },
                    key,
                }
            }
            (None, Some(secret)) => {
                // clap excuses `requires = openapi_oauth_private_key` on these
                // flags when a conflicting argument (the secret) is present, so
                // they reach us silently ineffective. Say so rather than let the
                // operator believe a `kid` is going out on the wire.
                let ignored = ineffective_assertion_flags(cli);
                if !ignored.is_empty() {
                    tracing::warn!(
                        flags = ignored.join(", "),
                        "these flags only apply to --openapi-oauth-private-key and are ignored \
                         with --openapi-oauth-client-secret"
                    );
                }
                ClientAuth::Secret(secret.clone())
            }
            (None, None) => bail!(
                "--openapi-oauth-token-url needs a client credential; pass \
                 --openapi-oauth-client-secret or --openapi-oauth-private-key"
            ),
        };

        Ok(Some(Self {
            token_url,
            client_id,
            client_auth,
            scopes: cli.openapi_oauth_scopes.clone(),
            audience: cli.openapi_oauth_audience.clone(),
        }))
    }
}

/// The assertion-only flags that are set but cannot apply, because the client
/// authenticates with a shared secret instead of a signed assertion.
///
/// `--openapi-oauth-signing-alg` is left out on purpose: it carries a default,
/// so it is always "set" and reporting it would cry wolf on every run.
fn ineffective_assertion_flags(cli: &Cli) -> Vec<&'static str> {
    let mut flags = Vec::new();
    if cli.openapi_oauth_key_id.is_some() {
        flags.push("--openapi-oauth-key-id");
    }
    if cli.openapi_oauth_assertion_audience.is_some() {
        flags.push("--openapi-oauth-assertion-audience");
    }
    if cli.openapi_oauth_assertion_lifetime.is_some() {
        flags.push("--openapi-oauth-assertion-lifetime");
    }
    flags
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
            TokenConfig::from_cli(&cli_from(&[])).expect("no OAuth config is not an error");
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
        let config = TokenConfig::from_cli(&cli)
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
        let config = TokenConfig::from_cli(&cli)
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
        let config = TokenConfig::from_cli(&cli)
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
            ineffective_assertion_flags(&cli),
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
        assert!(ineffective_assertion_flags(&cli).is_empty());
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
        let err = TokenConfig::from_cli(&cli)
            .err()
            .expect("a missing key must fail");
        assert!(
            format!("{err:#}").contains("loading the OAuth client signing key"),
            "{err:#}"
        );
    }
}
