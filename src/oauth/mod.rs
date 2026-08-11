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
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use url::Url;

use crate::cli::{AssertionSource, Cli, SigningAlg, UpstreamGrant};

pub use assertion::AssertionConfig;
pub use key::SigningKey;
pub use provider::{Delegation, TokenProvider};

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
        /// Shared with a self-signed §2.1 assertion when both are in play: the
        /// same key signs "I am the client" and "I speak for this subject", so it
        /// is loaded once and the warnings are emitted once.
        key: Arc<SigningKey>,
    },
}

/// Which grant is presented to the token endpoint.
pub enum Grant {
    /// RFC 6749 — the client acts as itself. One token for every caller.
    ClientCredentials,
    /// RFC 7523 §2.1 — a JWT assertion *is* the grant, which is what allows a
    /// token to be obtained on behalf of a subject.
    JwtBearer(GrantAssertion),
}

/// Where the §2.1 assertion comes from.
pub enum GrantAssertion {
    /// oas2mcp signs it, naming the subject it speaks for. The authorization
    /// server must be configured to trust oas2mcp to assert that subject — which
    /// makes this key, in effect, a "speak as anyone" credential. Keep the
    /// provider's trust configuration as narrow as it will go.
    SelfSigned {
        issuer: String,
        audience: String,
        lifetime: Duration,
        key: Arc<SigningKey>,
        subject: SubjectSource,
    },
    /// The caller's own verified JWT is relayed verbatim. Nothing is signed here;
    /// the authorization server is trusting the caller's issuer, not us.
    Caller,
}

/// Whose identity the assertion speaks for.
pub enum SubjectSource {
    /// A fixed `sub` — a service account acting as itself, the same for every
    /// caller.
    Fixed(String),
    /// The caller's verified identity, resolved from
    /// `--upstream-oauth-subject-claim`. A call with no verified subject is
    /// refused rather than silently downgraded to a broader identity.
    Caller,
}

/// Everything needed to run one grant against one token endpoint.
/// Self-contained on purpose — see the module docs.
pub struct TokenConfig {
    pub token_url: Url,
    pub client_id: String,
    pub client_auth: ClientAuth,
    pub grant: Grant,
    pub scopes: Vec<String>,
    pub audience: Option<String>,
}

impl TokenConfig {
    /// Whether this grant needs a verified caller identity for every call.
    pub fn needs_caller_identity(&self) -> bool {
        matches!(
            &self.grant,
            Grant::JwtBearer(GrantAssertion::Caller)
                | Grant::JwtBearer(GrantAssertion::SelfSigned {
                    subject: SubjectSource::Caller,
                    ..
                })
        )
    }
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
    /// The §2.1 grant, when this endpoint supports one. The document fetch never
    /// does — there is no caller to act on behalf of when loading a document.
    jwt_bearer: Option<JwtBearerFlags<'a>>,
}

/// The flags selecting the RFC 7523 §2.1 grant's shape.
struct JwtBearerFlags<'a> {
    assertion: AssertionSource,
    issuer: Option<&'a String>,
    subject: Option<&'a String>,
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
        jwt_bearer: None,
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
        jwt_bearer: match cli.upstream_oauth_grant {
            UpstreamGrant::ClientCredentials => None,
            UpstreamGrant::JwtBearer => Some(JwtBearerFlags {
                assertion: cli.upstream_oauth_assertion,
                issuer: cli.upstream_oauth_issuer.as_ref(),
                subject: cli.upstream_oauth_subject.as_ref(),
            }),
        },
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
    /// key when the client authenticates with an assertion, presents one as the
    /// grant, or both.
    fn from_flags(flags: GrantFlags<'_>) -> anyhow::Result<Self> {
        let prefix = flags.prefix;
        // clap enforces this via `requires_all`, but fail loudly rather than
        // panic if that ever changes.
        let client_id = flags
            .client_id
            .cloned()
            .with_context(|| format!("{prefix}-client-id is required with {prefix}-token-url"))?;

        // Both assertions — client authentication and the §2.1 grant — address
        // the same authorization server for the same short window.
        let assertion_audience = flags
            .assertion_audience
            .cloned()
            .unwrap_or_else(|| flags.token_url.to_string());
        let assertion_lifetime = flags
            .assertion_lifetime
            .unwrap_or(DEFAULT_ASSERTION_LIFETIME);

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
                        audience: assertion_audience.clone(),
                        lifetime: assertion_lifetime,
                    },
                    key: Arc::new(key),
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

        let grant = match flags.jwt_bearer {
            None => Grant::ClientCredentials,
            Some(jwt_bearer) => Grant::JwtBearer(match jwt_bearer.assertion {
                AssertionSource::Caller => {
                    if jwt_bearer.subject.is_some() {
                        bail!(
                            "{prefix}-subject has no meaning with {prefix}-assertion caller: \
                             the relayed token already names its own subject"
                        );
                    }
                    GrantAssertion::Caller
                }
                AssertionSource::SelfSigned => {
                    // A shared secret cannot sign anything. Rather than accept a
                    // configuration that could never mint an assertion, say so at
                    // startup.
                    let ClientAuth::PrivateKeyJwt { key, .. } = &client_auth else {
                        bail!(
                            "{prefix}-grant jwt-bearer needs {prefix}-private-key to sign the \
                             assertion; a client secret cannot sign one. Use \
                             {prefix}-assertion caller to relay the caller's token instead."
                        )
                    };
                    GrantAssertion::SelfSigned {
                        issuer: jwt_bearer
                            .issuer
                            .cloned()
                            .unwrap_or_else(|| client_id.clone()),
                        audience: assertion_audience,
                        lifetime: assertion_lifetime,
                        key: Arc::clone(key),
                        subject: match jwt_bearer.subject {
                            Some(fixed) => SubjectSource::Fixed(fixed.clone()),
                            None => SubjectSource::Caller,
                        },
                    }
                }
            }),
        };

        Ok(Self {
            token_url: flags.token_url,
            client_id,
            client_auth,
            grant,
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
    fn a_self_signed_jwt_bearer_grant_needs_a_signing_key() {
        // A shared secret cannot sign an assertion, so this configuration could
        // never mint one. Refusing at startup beats failing every call.
        let cli = cli_from(&[
            "--upstream-oauth-token-url",
            "https://idp.example.com/token",
            "--upstream-oauth-client-id",
            "id",
            "--upstream-oauth-client-secret",
            "secret",
            "--upstream-oauth-grant",
            "jwt-bearer",
        ]);
        let err = TokenConfig::for_upstream(&cli)
            .err()
            .expect("a secret cannot sign the grant assertion");
        let message = format!("{err:#}");
        assert!(message.contains("cannot sign one"), "{message}");
        // And it points at the way out.
        assert!(
            message.contains("--upstream-oauth-assertion caller"),
            "{message}"
        );
    }

    #[test]
    fn the_caller_assertion_mode_needs_no_key() {
        let cli = cli_from(&[
            "--upstream-oauth-token-url",
            "https://idp.example.com/token",
            "--upstream-oauth-client-id",
            "id",
            "--upstream-oauth-client-secret",
            "secret",
            "--upstream-oauth-grant",
            "jwt-bearer",
            "--upstream-oauth-assertion",
            "caller",
        ]);
        let config = TokenConfig::for_upstream(&cli)
            .expect("relaying the caller's token needs nothing signed")
            .expect("a token URL was given");
        assert!(matches!(
            config.grant,
            Grant::JwtBearer(GrantAssertion::Caller)
        ));
        assert!(config.needs_caller_identity());
    }

    #[test]
    fn a_fixed_subject_does_not_need_a_caller() {
        let cli = cli_from(&[
            "--upstream-oauth-token-url",
            "https://idp.example.com/token",
            "--upstream-oauth-client-id",
            "id",
            "--upstream-oauth-private-key",
            "tests/fixtures/test_rsa_key.pem",
            "--upstream-oauth-grant",
            "jwt-bearer",
            "--upstream-oauth-subject",
            "service-acct",
            "--upstream-oauth-issuer",
            "https://oas2mcp.example.com",
        ]);
        let config = TokenConfig::for_upstream(&cli)
            .expect("a fixed subject is well-formed")
            .expect("a token URL was given");
        let Grant::JwtBearer(GrantAssertion::SelfSigned {
            issuer, subject, ..
        }) = &config.grant
        else {
            panic!("expected a self-signed jwt-bearer grant");
        };
        assert_eq!(issuer, "https://oas2mcp.example.com");
        assert!(matches!(subject, SubjectSource::Fixed(s) if s == "service-acct"));
        assert!(!config.needs_caller_identity());
    }

    #[test]
    fn the_issuer_defaults_to_the_client_id() {
        let cli = cli_from(&[
            "--upstream-oauth-token-url",
            "https://idp.example.com/token",
            "--upstream-oauth-client-id",
            "client-abc",
            "--upstream-oauth-private-key",
            "tests/fixtures/test_rsa_key.pem",
            "--upstream-oauth-grant",
            "jwt-bearer",
        ]);
        let config = TokenConfig::for_upstream(&cli)
            .expect("well-formed")
            .expect("a token URL was given");
        let Grant::JwtBearer(GrantAssertion::SelfSigned {
            issuer, subject, ..
        }) = &config.grant
        else {
            panic!("expected a self-signed jwt-bearer grant");
        };
        assert_eq!(issuer, "client-abc");
        // Unset `--upstream-oauth-subject` means per-caller delegation.
        assert!(matches!(subject, SubjectSource::Caller));
        assert!(config.needs_caller_identity());
    }

    #[test]
    fn a_fixed_subject_is_rejected_when_relaying_the_callers_token() {
        let cli = cli_from(&[
            "--upstream-oauth-token-url",
            "https://idp.example.com/token",
            "--upstream-oauth-client-id",
            "id",
            "--upstream-oauth-client-secret",
            "secret",
            "--upstream-oauth-grant",
            "jwt-bearer",
            "--upstream-oauth-assertion",
            "caller",
            "--upstream-oauth-subject",
            "service-acct",
        ]);
        // The relayed token names its own subject; a second answer is a mistake
        // worth reporting rather than silently ignoring.
        let err = TokenConfig::for_upstream(&cli)
            .err()
            .expect("a fixed subject makes no sense here");
        assert!(
            format!("{err:#}").contains("has no meaning with"),
            "{err:#}"
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
