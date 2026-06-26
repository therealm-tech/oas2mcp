//! Loading and parsing of the OpenAPI document.

use anyhow::{Context as _, bail};
use openapiv3::OpenAPI;
use url::Url;

use crate::cli::Cli;
use crate::server::parse_headers;

/// Load the OpenAPI document from the source configured on the CLI (a local
/// file or a URL), accepting either JSON or YAML.
pub async fn load(cli: &Cli) -> anyhow::Result<OpenAPI> {
    match (&cli.openapi_file, &cli.openapi_url) {
        (Some(path), _) => {
            tracing::debug!(path = %path.display(), "reading OpenAPI document from file");
            let bytes = tokio::fs::read(path)
                .await
                .with_context(|| format!("reading OpenAPI file {}", path.display()))?;
            parse(&bytes)
                .with_context(|| format!("parsing OpenAPI document from {}", path.display()))
        }
        (None, Some(url)) => fetch(url, &cli.openapi_headers).await,
        (None, None) => bail!("no OpenAPI source: pass --openapi-file or --openapi-url"),
    }
}

/// Fetch and parse the OpenAPI document from `url`, attaching `raw_headers`
/// (`Name: Value` strings) to authenticate against a non-public document URL.
/// Used both for the initial load and for periodic reloads.
pub async fn fetch(url: &Url, raw_headers: &[String]) -> anyhow::Result<OpenAPI> {
    let headers = parse_headers(raw_headers).context("parsing --openapi-header values")?;

    tracing::debug!(%url, "fetching OpenAPI document over HTTP");
    let bytes = reqwest::Client::new()
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

/// Parse raw bytes as an OpenAPI document, trying JSON first and falling back
/// to YAML (a superset, so YAML covers `.json` too, but JSON is the common and
/// faster case).
fn parse(bytes: &[u8]) -> anyhow::Result<OpenAPI> {
    match serde_json::from_slice::<OpenAPI>(bytes) {
        Ok(spec) => Ok(spec),
        Err(json_err) => serde_yaml_ng::from_slice::<OpenAPI>(bytes).map_err(|yaml_err| {
            anyhow::anyhow!(
                "document is neither valid OpenAPI JSON ({json_err}) nor YAML ({yaml_err})"
            )
        }),
    }
}
