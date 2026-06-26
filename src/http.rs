//! Shared outbound HTTP client construction.
//!
//! Every outbound client — upstream API calls, the OpenAPI document fetch, the
//! OAuth token requests, the JWKS fetch — is built here so they all carry the
//! same identity and, crucially, the same TLS trust configuration. Extra CA
//! certificates injected via `--ca-cert` are loaded once and applied to all of
//! them.

use std::fs;

use anyhow::{Context as _, bail};
use reqwest::Certificate;

use crate::cli::Cli;

/// `User-Agent` sent on every outbound request.
const USER_AGENT: &str = concat!("oas2mcp/", env!("CARGO_PKG_VERSION"));

/// Build a `reqwest` client carrying the shared user agent plus any extra root
/// CA certificates configured via `--ca-cert`.
///
/// The platform's built-in roots stay trusted; the configured certificates are
/// *added* on top, so pointing at a single private/corporate CA bundle is
/// enough to reach an upstream that terminates TLS with it — no need to also
/// re-supply the public roots.
pub fn client(cli: &Cli) -> anyhow::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder().user_agent(USER_AGENT);
    for path in &cli.ca_certs {
        let pem = fs::read(path)
            .with_context(|| format!("reading CA certificate file {}", path.display()))?;
        // A single file may hold a whole chain; trust every certificate in it.
        let certs = Certificate::from_pem_bundle(&pem)
            .with_context(|| format!("parsing CA certificates from {}", path.display()))?;
        // `from_pem_bundle` happily returns zero certificates for a file with no
        // PEM blocks; treat that as a configuration error rather than silently
        // trusting nothing extra.
        if certs.is_empty() {
            bail!(
                "no PEM certificates found in CA certificate file {}",
                path.display()
            );
        }
        tracing::debug!(path = %path.display(), count = certs.len(), "adding extra root CA certificates");
        for cert in certs {
            builder = builder.add_root_certificate(cert);
        }
    }
    builder.build().context("building the HTTP client")
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;

    /// A throwaway self-signed CA, valid PEM, used only to exercise loading.
    const TEST_CA_PEM: &str = "\
-----BEGIN CERTIFICATE-----
MIIDFTCCAf2gAwIBAgIUD8PxV4nLr6/fcCykaGommOzTBZAwDQYJKoZIhvcNAQEL
BQAwGjEYMBYGA1UEAwwPb2FzMm1jcC10ZXN0LWNhMB4XDTI2MDYyNjEwMjk1NloX
DTM2MDYyMzEwMjk1NlowGjEYMBYGA1UEAwwPb2FzMm1jcC10ZXN0LWNhMIIBIjAN
BgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA0Rw9ASDlLQF7QUGG8DEkd2CZ1CEM
prxcUcektPgL3cSFRnDkHqFnf6rce3fCyEhkry+AqgiSFUG9ot41UjdOBiXfBLq7
FtbYmjinf6Zeip6qvcpE9G5U9Ss3npgjTlR6H/MXmj4Q29IHuXqsw1pY/R7/JsJq
LtrSI+uvLxMTvmSW5Q9/V5AZ9k7VqVdbxKH7MgmIwhXn+CtqmOsVi2p+fvHBJo3y
gJxpj0oU3N2do6eB+57xa7Q3TlCVLeJ68ciDH2M1AheRNIfmwdQ+Y5O3lXmopfEh
K2EysrAAKIHrMHoziRRObwyfCDF0PJSRwqC79ybxu2pc+XHIKV1MR5NC+wIDAQAB
o1MwUTAdBgNVHQ4EFgQUccK7pqm80TLZ8aiMGes/Fod9IGYwHwYDVR0jBBgwFoAU
ccK7pqm80TLZ8aiMGes/Fod9IGYwDwYDVR0TAQH/BAUwAwEB/zANBgkqhkiG9w0B
AQsFAAOCAQEAkjriujOiYLf/+dQiFGWevxbuxQ5JyDtEoyBkD9C+lz9zJRFldPFy
X+7sABBSJvA6TgmPLlHas0k+Ur6ytjWmaCBW6h0dz8A6Z7QOabFx1kFlzE/ji8hb
h9c3ZKkrRIM4LH6Z7zmfoPNz4IuFs6WHOaxEyHuxoBeN1YTMzvUHk5FtG1Me5+q/
Fg28Sq+0W005wSD7UJR/uaYt7la198c/65eD/icegJV83Xull8Vo+cJ5YeCe0Ulz
ieKi2h0XXjYwHyhgYBszrqyFLbCufMm0padl4PWB2RRMjRUB3NTTdg83HWnIpoN2
gpg0QXMxyWnjLJlYIpB9z4CDr4TZIpkqgw==
-----END CERTIFICATE-----
";

    fn cli_from(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("oas2mcp").chain(args.iter().copied()))
            .expect("CLI parses")
    }

    #[test]
    fn builds_without_extra_ca_certs() {
        let cli = cli_from(&[]);
        client(&cli).expect("a client with no extra CA certs builds");
    }

    #[test]
    fn missing_ca_cert_file_is_an_error() {
        let cli = cli_from(&["--ca-cert", "/definitely/not/a/real/ca.pem"]);
        let err = client(&cli).expect_err("a non-existent CA file must fail");
        assert!(
            format!("{err:#}").contains("reading CA certificate file"),
            "error should point at the unreadable file: {err:#}"
        );
    }

    #[test]
    fn invalid_pem_is_an_error() {
        let dir = std::env::temp_dir();
        let path = dir.join("oas2mcp-invalid-ca.pem");
        std::fs::write(&path, b"not a certificate").expect("write temp file");
        let cli = cli_from(&["--ca-cert", path.to_str().unwrap()]);
        let err = client(&cli).expect_err("garbage PEM must fail");
        let _ = std::fs::remove_file(&path);
        assert!(
            format!("{err:#}").contains("no PEM certificates found"),
            "a file with no PEM blocks should be rejected: {err:#}"
        );
    }

    #[test]
    fn loads_a_valid_ca_certificate() {
        let path = std::env::temp_dir().join("oas2mcp-valid-ca.pem");
        std::fs::write(&path, TEST_CA_PEM).expect("write temp CA");
        let cli = cli_from(&["--ca-cert", path.to_str().unwrap()]);
        let built = client(&cli);
        let _ = std::fs::remove_file(&path);
        built.expect("a client with a valid extra CA builds");
    }

    #[test]
    fn ca_cert_env_splits_on_newlines() {
        let cli = cli_from(&[]);
        assert!(cli.ca_certs.is_empty());

        let cli = cli_from(&["--ca-cert", "/a.pem", "--ca-cert", "/b.pem"]);
        assert_eq!(cli.ca_certs.len(), 2);
    }
}
