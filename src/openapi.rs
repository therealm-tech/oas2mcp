//! Loading and parsing of the OpenAPI document.

pub mod spec;

use anyhow::{Context as _, bail};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde_json::Value;
use url::Url;

use crate::cli::Cli;
use crate::oauth::TokenProvider;
use crate::server::parse_headers;

pub use spec::Spec;

/// Authentication for the OpenAPI document fetch: optional static headers and
/// an optional OAuth `client_credentials` token provider. Cheap to clone — the
/// HTTP client is reference-counted and the provider shares its token cache —
/// so the same instance backs the startup load and the reload loop.
#[derive(Clone)]
pub struct DocAuth {
    client: reqwest::Client,
    static_headers: HeaderMap,
    oauth: Option<TokenProvider>,
}

impl DocAuth {
    /// Build the document-fetch auth from the CLI: parse the static
    /// `--openapi-header` values and, if configured, the OAuth provider. The
    /// HTTP client is shared with the OAuth token requests.
    pub fn from_cli(cli: &Cli) -> anyhow::Result<Self> {
        let client = crate::http::client(cli).context("building the document-fetch HTTP client")?;
        let static_headers =
            parse_headers(&cli.openapi_headers).context("parsing --openapi-header values")?;
        let oauth = TokenProvider::from_cli(cli, client.clone())?;
        Ok(Self {
            client,
            static_headers,
            oauth,
        })
    }

    /// Resolve the headers to send with the document request: the static
    /// headers plus, when OAuth is configured, a fresh `Authorization: Bearer`
    /// (which wins over any static `Authorization`).
    async fn headers(&self) -> anyhow::Result<HeaderMap> {
        let mut headers = self.static_headers.clone();
        if let Some(provider) = &self.oauth {
            let token = provider
                .access_token()
                .await
                .context("obtaining the document-fetch OAuth token")?;
            let value = HeaderValue::from_str(&format!("Bearer {token}"))
                .context("building the Authorization header from the OAuth token")?;
            headers.insert(AUTHORIZATION, value);
        }
        Ok(headers)
    }

    /// Fetch and parse the OpenAPI document from `url`, applying the configured
    /// auth. Used both for the initial load and for periodic reloads.
    pub async fn fetch(&self, url: &Url) -> anyhow::Result<Spec> {
        let headers = self.headers().await?;

        tracing::debug!(%url, "fetching OpenAPI document over HTTP");
        let bytes = self
            .client
            .get(url.clone())
            .headers(headers)
            .send()
            .await
            .with_context(|| format!("fetching OpenAPI document from {url}"))?
            .error_for_status()
            .with_context(|| format!("OpenAPI document request to {url} failed"))?
            .bytes()
            .await
            .with_context(|| format!("reading OpenAPI response body from {url}"))?;

        parse(&bytes).with_context(|| format!("parsing OpenAPI document from {url}"))
    }
}

/// Load the OpenAPI document from the source configured on the CLI (a local
/// file or a URL), accepting either JSON or YAML. `auth` applies to the URL
/// source only.
pub async fn load(cli: &Cli, auth: &DocAuth) -> anyhow::Result<Spec> {
    match (&cli.openapi_file, &cli.openapi_url) {
        (Some(path), _) => {
            tracing::debug!(path = %path.display(), "reading OpenAPI document from file");
            let bytes = tokio::fs::read(path)
                .await
                .with_context(|| format!("reading OpenAPI file {}", path.display()))?;
            parse(&bytes)
                .with_context(|| format!("parsing OpenAPI document from {}", path.display()))
        }
        (None, Some(url)) => auth.fetch(url).await,
        (None, None) => bail!("no OpenAPI source: pass --openapi-file or --openapi-url"),
    }
}

/// Parse raw bytes as an OpenAPI document, trying JSON first and falling back
/// to YAML (a superset, so YAML covers `.json` too, but JSON is the common and
/// faster case). The document is decoded to plain JSON before being read as a
/// spec, so the OpenAPI version only has to be interpreted in one place.
fn parse(bytes: &[u8]) -> anyhow::Result<Spec> {
    let raw = match serde_json::from_slice::<Value>(bytes) {
        Ok(raw) => raw,
        Err(json_err) => serde_yaml_ng::from_slice::<Value>(bytes).map_err(|yaml_err| {
            anyhow::anyhow!("document is neither valid JSON ({json_err}) nor YAML ({yaml_err})")
        })?,
    };
    Spec::from_value(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_and_yaml_alike() {
        const JSON: &[u8] = br#"{"openapi":"3.1.0","info":{"title":"T","version":"1"},"paths":{}}"#;
        const YAML: &[u8] = b"openapi: 3.1.0\ninfo:\n  title: T\n  version: '1'\npaths: {}\n";

        assert_eq!(parse(JSON).expect("JSON parses").info().title, "T");
        assert_eq!(parse(YAML).expect("YAML parses").info().title, "T");
    }

    #[test]
    fn reports_both_decoders_when_neither_reads_the_bytes() {
        let err = parse(b"\x00not a document").expect_err("undecodable bytes");
        let message = format!("{err}");
        assert!(message.contains("neither valid JSON"), "{message}");
    }
}
