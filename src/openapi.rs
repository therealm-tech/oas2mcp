//! Loading and parsing of the OpenAPI document.

use anyhow::{Context as _, bail};
use openapiv3::OpenAPI;

use crate::cli::Cli;

/// Load the OpenAPI document from the source configured on the CLI (a local
/// file or a URL), accepting either JSON or YAML.
pub async fn load(cli: &Cli) -> anyhow::Result<OpenAPI> {
    let (origin, bytes) = match (&cli.openapi_file, &cli.openapi_url) {
        (Some(path), _) => {
            tracing::debug!(path = %path.display(), "reading OpenAPI document from file");
            let bytes = tokio::fs::read(path)
                .await
                .with_context(|| format!("reading OpenAPI file {}", path.display()))?;
            (path.display().to_string(), bytes)
        }
        (None, Some(url)) => {
            tracing::debug!(%url, "fetching OpenAPI document over HTTP");
            let bytes = reqwest::get(url.clone())
                .await
                .with_context(|| format!("fetching OpenAPI document from {url}"))?
                .error_for_status()
                .with_context(|| format!("OpenAPI document request to {url} failed"))?
                .bytes()
                .await
                .with_context(|| format!("reading OpenAPI response body from {url}"))?
                .to_vec();
            (url.to_string(), bytes)
        }
        (None, None) => bail!("no OpenAPI source: pass --openapi-file or --openapi-url"),
    };

    parse(&bytes).with_context(|| format!("parsing OpenAPI document from {origin}"))
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
