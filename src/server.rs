//! The MCP server: advertises one tool per OpenAPI operation and executes a
//! tool call by proxying it as an HTTP request to the upstream API.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context as _, bail};
use openapiv3::OpenAPI;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData, ServerHandler};
use serde_json::{Map, Value};
use url::Url;

use crate::cli::Cli;
use crate::tools::{Param, ParamLocation, ToolSpec, build_tools};

/// MCP server backed by an OpenAPI document. Cheap to clone (everything shared
/// is behind an `Arc`, and `reqwest::Client` is itself reference-counted), as
/// the Streamable HTTP transport builds one instance per session.
#[derive(Clone)]
pub struct OpenApiServer {
    tools: Arc<Vec<ToolSpec>>,
    index: Arc<HashMap<String, usize>>,
    client: reqwest::Client,
    base_url: Url,
    extra_headers: Arc<HeaderMap>,
    /// Names of incoming-request headers to forward verbatim to the upstream API.
    forward_headers: Arc<Vec<HeaderName>>,
    instructions: Arc<str>,
}

impl OpenApiServer {
    /// Build the server from a parsed OpenAPI document and the CLI config.
    pub fn from_spec(spec: &OpenAPI, cli: &Cli) -> anyhow::Result<Self> {
        let base_url = resolve_base_url(spec, cli)?;
        let extra_headers = parse_headers(&cli.headers)?;
        let forward_headers = parse_header_names(&cli.forward_headers)?;

        let tools = build_tools(spec);
        if tools.is_empty() {
            tracing::warn!("the OpenAPI document defines no usable operations");
        }
        let index = tools
            .iter()
            .enumerate()
            .map(|(i, t)| (t.name.clone(), i))
            .collect();

        let instructions = format!(
            "MCP server proxying the \"{}\" API (version {}). \
             Each tool maps to one OpenAPI operation and is executed as an HTTP \
             request against {}. Path/query/header parameters are top-level tool \
             arguments; a JSON request body is passed as the `body` argument.",
            spec.info.title, spec.info.version, base_url,
        );

        Ok(Self {
            tools: Arc::new(tools),
            index: Arc::new(index),
            client: reqwest::Client::builder()
                .user_agent(concat!("oas2mcp/", env!("CARGO_PKG_VERSION")))
                .build()
                .context("building the HTTP client")?,
            base_url,
            extra_headers: Arc::new(extra_headers),
            forward_headers: Arc::new(forward_headers),
            instructions: Arc::from(instructions),
        })
    }

    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }

    /// Execute a tool call as a proxied HTTP request and shape the response as
    /// an MCP tool result.
    async fn execute(
        &self,
        spec: &ToolSpec,
        args: &Map<String, Value>,
        forwarded: &HeaderMap,
    ) -> CallToolResult {
        let request = match self.build_request(spec, args, forwarded) {
            Ok(request) => request,
            Err(err) => return CallToolResult::error(vec![Content::text(err.to_string())]),
        };

        tracing::debug!(tool = %spec.name, method = %spec.method, "proxying upstream request");
        match request.send().await {
            Ok(response) => {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                let text = format!("HTTP {status}\n\n{body}");
                let content = vec![Content::text(text)];
                if status.is_client_error() || status.is_server_error() {
                    CallToolResult::error(content)
                } else {
                    CallToolResult::success(content)
                }
            }
            Err(err) => CallToolResult::error(vec![Content::text(format!(
                "upstream request failed: {err}"
            ))]),
        }
    }

    /// Assemble the `reqwest` request: resolve the path template, collect query
    /// and header parameters, and attach the JSON body.
    fn build_request(
        &self,
        spec: &ToolSpec,
        args: &Map<String, Value>,
        forwarded: &HeaderMap,
    ) -> anyhow::Result<reqwest::RequestBuilder> {
        // Resolve path parameters into the template.
        let mut path = spec.path_template.clone();
        for param in spec
            .params
            .iter()
            .filter(|p| p.location == ParamLocation::Path)
        {
            let value = args.get(&param.name).ok_or_else(|| {
                anyhow::anyhow!("missing required path parameter `{}`", param.name)
            })?;
            let encoded =
                utf8_percent_encode(&value_to_string(value), NON_ALPHANUMERIC).to_string();
            path = path.replace(&format!("{{{}}}", param.name), &encoded);
        }

        let full = format!(
            "{}/{}",
            self.base_url.as_str().trim_end_matches('/'),
            path.trim_start_matches('/')
        );
        let url = Url::parse(&full).with_context(|| format!("building upstream URL `{full}`"))?;

        let mut request = self.client.request(spec.method.clone(), url);

        // Query parameters (scalars and arrays).
        let mut query: Vec<(String, String)> = Vec::new();
        for param in spec
            .params
            .iter()
            .filter(|p| p.location == ParamLocation::Query)
        {
            collect_query(param, args.get(&param.name), &mut query);
        }
        if !query.is_empty() {
            request = request.query(&query);
        }

        // Header parameters.
        for param in spec
            .params
            .iter()
            .filter(|p| p.location == ParamLocation::Header)
        {
            if let Some(value) = args.get(&param.name) {
                let name = HeaderName::from_bytes(param.name.as_bytes())
                    .with_context(|| format!("invalid header name `{}`", param.name))?;
                let value = HeaderValue::from_str(&value_to_string(value))
                    .with_context(|| format!("invalid value for header `{}`", param.name))?;
                request = request.header(name, value);
            }
        }

        // Forwarded incoming-request headers, unless a static header of the
        // same name is configured (static headers win).
        for name in forwarded.keys() {
            if self.extra_headers.contains_key(name) {
                continue;
            }
            for value in forwarded.get_all(name) {
                request = request.header(name.clone(), value.clone());
            }
        }

        // Static headers (auth, etc.) apply to every request.
        request = request.headers((*self.extra_headers).clone());

        // JSON body.
        if spec.has_body
            && let Some(body) = args.get("body")
        {
            request = request.json(body);
        }

        Ok(request)
    }
}

impl ServerHandler for OpenApiServer {
    fn get_info(&self) -> ServerInfo {
        // `ServerInfo` is `#[non_exhaustive]`, so build from default and set fields.
        // Identify as this crate (not rmcp, which `from_build_env` would report).
        let mut server_info = Implementation::default();
        server_info.name = env!("CARGO_PKG_NAME").to_string();
        server_info.version = env!("CARGO_PKG_VERSION").to_string();

        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = server_info;
        info.instructions = Some(self.instructions.to_string());
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools = self
            .tools
            .iter()
            .map(|spec| {
                Tool::new(
                    spec.name.clone(),
                    spec.description.clone().unwrap_or_default(),
                    spec.input_schema.clone(),
                )
            })
            .collect();
        Ok(ListToolsResult {
            tools,
            next_cursor: None,
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let Some(&idx) = self.index.get(request.name.as_ref()) else {
            return Err(ErrorData::invalid_params(
                Cow::from(format!("unknown tool `{}`", request.name)),
                None,
            ));
        };
        let spec = &self.tools[idx];
        let args = request.arguments.unwrap_or_default();
        let forwarded = self.forwarded_headers(&context);
        Ok(self.execute(spec, &args, &forwarded).await)
    }
}

impl OpenApiServer {
    /// Collect the allow-listed incoming-request headers to forward upstream.
    /// Only the Streamable HTTP transport injects the request [`Parts`]; for
    /// `stdio` and `sse` this yields an empty map.
    ///
    /// [`Parts`]: http::request::Parts
    fn forwarded_headers(&self, context: &RequestContext<RoleServer>) -> HeaderMap {
        if self.forward_headers.is_empty() {
            return HeaderMap::new();
        }
        match context.extensions.get::<http::request::Parts>() {
            Some(parts) => filter_forwarded(&self.forward_headers, &parts.headers),
            None => HeaderMap::new(),
        }
    }
}

/// Pick the headers named in `allow` out of `src`, preserving multiple values
/// for the same name.
fn filter_forwarded(allow: &[HeaderName], src: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for name in allow {
        for value in src.get_all(name) {
            out.append(name.clone(), value.clone());
        }
    }
    out
}

/// Determine the upstream base URL: the CLI override wins, otherwise the first
/// absolute `servers` entry of the document.
fn resolve_base_url(spec: &OpenAPI, cli: &Cli) -> anyhow::Result<Url> {
    if let Some(url) = &cli.base_url {
        return Ok(url.clone());
    }
    for server in &spec.servers {
        if let Ok(url) = Url::parse(&server.url) {
            return Ok(url);
        }
    }
    bail!("no usable base URL: the OpenAPI `servers` list is empty or relative; pass --base-url")
}

/// Parse `Name: Value` header strings from the CLI into a [`HeaderMap`].
fn parse_headers(raw: &[String]) -> anyhow::Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    for entry in raw {
        let (name, value) = entry
            .split_once(':')
            .with_context(|| format!("header `{entry}` is not in `Name: Value` form"))?;
        let name = HeaderName::from_bytes(name.trim().as_bytes())
            .with_context(|| format!("invalid header name in `{entry}`"))?;
        let value = HeaderValue::from_str(value.trim())
            .with_context(|| format!("invalid header value in `{entry}`"))?;
        headers.insert(name, value);
    }
    Ok(headers)
}

/// Parse bare header names (e.g. `Authorization`) from the CLI into a list of
/// [`HeaderName`]s, validating each.
fn parse_header_names(raw: &[String]) -> anyhow::Result<Vec<HeaderName>> {
    raw.iter()
        .map(|name| {
            HeaderName::from_bytes(name.trim().as_bytes())
                .with_context(|| format!("invalid header name `{name}`"))
        })
        .collect()
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Append a query parameter, expanding arrays into repeated entries.
fn collect_query(param: &Param, value: Option<&Value>, out: &mut Vec<(String, String)>) {
    match value {
        Some(Value::Array(items)) => {
            for item in items {
                out.push((param.name.clone(), value_to_string(item)));
            }
        }
        Some(value) => out.push((param.name.clone(), value_to_string(value))),
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_validates_header_names() {
        let names =
            parse_header_names(&["Authorization".into(), " X-Tenant ".into()]).expect("valid");
        assert_eq!(
            names,
            vec![
                HeaderName::from_static("authorization"),
                HeaderName::from_static("x-tenant"),
            ]
        );
        assert!(parse_header_names(&["not a header".into()]).is_err());
    }

    #[test]
    fn filter_forwarded_keeps_only_allow_listed_headers() {
        let mut src = HeaderMap::new();
        src.insert("authorization", HeaderValue::from_static("Bearer secret"));
        src.insert("cookie", HeaderValue::from_static("session=nope"));
        src.append("x-tenant", HeaderValue::from_static("a"));
        src.append("x-tenant", HeaderValue::from_static("b"));

        let allow = vec![
            HeaderName::from_static("authorization"),
            HeaderName::from_static("x-tenant"),
            HeaderName::from_static("x-absent"),
        ];
        let out = filter_forwarded(&allow, &src);

        assert_eq!(out.get("authorization").unwrap(), "Bearer secret");
        assert!(out.get("cookie").is_none());
        let tenant: Vec<_> = out.get_all("x-tenant").iter().collect();
        assert_eq!(tenant, vec!["a", "b"]);
    }
}
