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
/// The bundled Mozilla roots stay trusted; the configured certificates are
/// *added* on top, so pointing at a single private/corporate CA bundle is
/// enough to reach an upstream that terminates TLS with it — no need to also
/// re-supply the public roots.
pub fn client(cli: &Cli) -> anyhow::Result<reqwest::Client> {
    // Not reqwest's default platform verifier: see ARCHITECTURE.md, "Outbound
    // TLS trusts the bundled Mozilla roots".
    let mut roots = webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .map(|der| Certificate::from_der(der))
        .collect::<Result<Vec<_>, _>>()
        .context("loading the bundled root CA certificates")?;
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
        roots.extend(certs);
    }
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .tls_certs_only(roots)
        .build()
        .context("building the HTTP client")
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

    /// Serve one HTTPS request on `127.0.0.1` with a `localhost` certificate
    /// signed by a CA generated for this call; returns the CA's PEM and the port.
    async fn serve_tls_signed_by_a_fresh_ca() -> (String, u16) {
        use std::sync::Arc;

        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        use tokio_rustls::rustls;

        let ca_key = rcgen::KeyPair::generate().expect("CA key");
        let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("CA params");
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = rcgen::CertifiedIssuer::self_signed(ca_params, ca_key).expect("CA certificate");
        let leaf_key = rcgen::KeyPair::generate().expect("leaf key");
        let leaf = rcgen::CertificateParams::new(vec!["localhost".to_owned()])
            .expect("leaf params")
            .signed_by(&leaf_key, &ca)
            .expect("leaf certificate");

        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("TLS versions")
            .with_no_client_auth()
            .with_single_cert(
                vec![leaf.der().clone()],
                rustls::pki_types::PrivatePkcs8KeyDer::from(leaf_key.serialize_der()).into(),
            )
            .expect("server certificate");
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("local address").port();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("accept");
            // A client that rejects the certificate aborts the handshake here.
            let Ok(mut tls) = acceptor.accept(tcp).await else {
                return;
            };
            let mut request = [0u8; 1024];
            let _ = tls.read(&mut request).await;
            let _ = tls
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")
                .await;
            let _ = tls.shutdown().await;
        });
        (ca.pem(), port)
    }

    #[tokio::test]
    async fn trusts_a_server_signed_by_an_extra_ca() {
        let (ca_pem, port) = serve_tls_signed_by_a_fresh_ca().await;
        let path = std::env::temp_dir().join(format!("oas2mcp-extra-ca-{port}.pem"));
        std::fs::write(&path, ca_pem).expect("write temp CA");
        let built = client(&cli_from(&["--ca-cert", path.to_str().unwrap()]));
        let _ = std::fs::remove_file(&path);

        let response = built
            .expect("client builds")
            .get(format!("https://localhost:{port}/"))
            .send()
            .await
            .expect("the extra CA is a trust anchor");
        assert_eq!(response.text().await.expect("body"), "ok");
    }

    #[tokio::test]
    async fn rejects_a_server_signed_by_an_unknown_ca() {
        let (_, port) = serve_tls_signed_by_a_fresh_ca().await;
        let err = client(&cli_from(&[]))
            .expect("client builds")
            .get(format!("https://localhost:{port}/"))
            .send()
            .await
            .expect_err("a CA outside the trusted roots must be refused");
        let chain = std::iter::successors(Some(&err as &dyn std::error::Error), |e| e.source())
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(": ");
        assert!(
            chain.contains("UnknownIssuer"),
            "expected a certificate error: {chain}"
        );
    }

    #[test]
    fn ca_cert_env_splits_on_newlines() {
        let cli = cli_from(&[]);
        assert_eq!(cli.ca_certs, Vec::<std::path::PathBuf>::new());

        let cli = cli_from(&["--ca-cert", "/a.pem", "--ca-cert", "/b.pem"]);
        assert_eq!(cli.ca_certs.len(), 2);
    }
}
