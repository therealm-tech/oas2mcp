//! The MCP server: advertises one tool per OpenAPI operation and executes a
//! tool call by proxying it as an HTTP request to the upstream API.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, bail};
use arc_swap::ArcSwap;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData, ServerHandler};
use serde_json::{Map, Value};
use url::Url;

use crate::auth::{Authorizer, bearer_token};
use crate::cli::Cli;
use crate::filter::{FilterConfig, OperationFilter};
use crate::oauth::{Delegation, TokenProvider};
use crate::openapi::Spec;
use crate::rename::{RenameConfig, ToolRenamer};
use crate::telemetry::{Metrics, Outcome};
use crate::tools::{Param, ParamLocation, ToolSpec, build_tools};

/// The part of the server that an OpenAPI reload replaces: the resolved tools,
/// their name index, the upstream base URL (which may be derived from the
/// document's `servers`), and the instructions string. Swapped atomically as a
/// whole so a reload never exposes a half-updated state.
struct Snapshot {
    tools: Vec<ToolSpec>,
    index: HashMap<String, usize>,
    base_url: Url,
    instructions: String,
}

/// MCP server backed by an OpenAPI document. Cheap to clone (everything shared
/// is behind an `Arc`, and `reqwest::Client` is itself reference-counted), as
/// the Streamable HTTP transport builds one instance per session. The
/// document-derived state lives behind an [`ArcSwap`] so a periodic reload
/// updates every clone at once.
#[derive(Clone)]
pub struct OpenApiServer {
    state: Arc<ArcSwap<Snapshot>>,
    client: reqwest::Client,
    extra_headers: Arc<HeaderMap>,
    /// Names of incoming-request headers to forward verbatim to the upstream API.
    forward_headers: Arc<Vec<HeaderName>>,
    /// Optional JWT role-based tool authorization. `None` exposes every tool.
    authorizer: Option<Arc<Authorizer>>,
    /// Optional OAuth token provider for upstream API calls. `None` leaves the
    /// upstream `Authorization` to `--header` / `--forward-header`.
    upstream_token: Option<TokenProvider>,
    /// Tool-call metrics. No-op when telemetry is disabled.
    metrics: Metrics,
}

/// The authenticated caller of a request: their JWT roles (when authorization
/// is enabled), the claims selected for tracing, and the identity a delegated
/// upstream token is obtained for.
struct Caller {
    access: Access,
    /// The `--trace-claim` claims present in the verified token, logged with the
    /// tool call. Empty unless claim tracing is configured and the token carried
    /// them.
    traced_claims: Map<String, Value>,
    /// The verified identity to delegate as, from a **successfully verified**
    /// token only. `None` denies delegation.
    identity: Option<Identity>,
}

/// Which tools a request may see and call.
enum Access {
    /// No authorizer is configured: every tool.
    Unrestricted,
    /// No valid token: the public tools only.
    Anonymous,
    /// A verified token carrying these roles.
    Authenticated(HashSet<String>),
}

/// A verified caller identity, everything a delegated token request needs.
struct Identity {
    /// Value of the delegation subject claim.
    subject: String,
    /// The token's `iss`, which scopes the subject: part of the cache key.
    issuer: Option<String>,
    /// The token's `exp` as an instant, so a delegated token is not cached past
    /// the caller session that justified it.
    expiry: Option<Instant>,
    /// The caller's raw JWT, relayed by the `caller` assertion mode.
    token: String,
}

impl OpenApiServer {
    /// Build the server from a parsed OpenAPI document and the CLI config.
    /// `authorizer`, when set, gates tool visibility and invocation on the
    /// caller's JWT roles.
    pub fn from_spec(
        spec: &Spec,
        cli: &Cli,
        authorizer: Option<Arc<Authorizer>>,
        metrics: Metrics,
    ) -> anyhow::Result<Self> {
        let extra_headers = parse_headers(&cli.headers)?;
        let forward_headers = parse_header_names(
            cli.http()
                .map_or(&[], |http| http.forward_headers.as_slice()),
        )?;
        let snapshot = build_snapshot(spec, cli)?;
        let client = crate::http::client(cli).context("building the upstream HTTP client")?;
        // Shares the upstream client, so token requests reuse its connection
        // pool and TLS trust (including `--ca-cert`).
        let upstream_token = TokenProvider::for_upstream(cli, client.clone())
            .context("configuring the upstream OAuth token provider")?;
        check_header_sources(&extra_headers, &forward_headers, upstream_token.is_some())?;

        Ok(Self {
            state: Arc::new(ArcSwap::from_pointee(snapshot)),
            client,
            extra_headers: Arc::new(extra_headers),
            forward_headers: Arc::new(forward_headers),
            authorizer,
            upstream_token,
            metrics,
        })
    }

    /// Rebuild the tool set from a freshly fetched document and swap it in
    /// atomically. The static config (auth headers, forwarded header names, the
    /// HTTP client) is untouched. If the new document yields no usable tools,
    /// the swap still happens — that is what the document now says.
    pub fn reload(&self, spec: &Spec, cli: &Cli) -> anyhow::Result<()> {
        let snapshot = build_snapshot(spec, cli)?;
        let tools = snapshot.tools.len();
        self.state.store(Arc::new(snapshot));
        tracing::info!(tools, "reloaded the OpenAPI document");
        Ok(())
    }

    pub fn tool_count(&self) -> usize {
        self.state.load().tools.len()
    }

    /// Execute a tool call as a proxied HTTP request and shape the response as
    /// an MCP tool result.
    async fn execute(
        &self,
        spec: &ToolSpec,
        base_url: &Url,
        args: &Map<String, Value>,
        forwarded: &HeaderMap,
        bearer: Option<&str>,
    ) -> CallToolResult {
        let request = match self.build_request(spec, base_url, args, forwarded, bearer) {
            Ok(request) => request,
            Err(err) => return CallToolResult::error(vec![ContentBlock::text(err.to_string())]),
        };

        tracing::debug!(tool = %spec.name, method = %spec.method, "proxying upstream request");
        match request.send().await {
            Ok(response) => {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                shape_response(status, &body)
            }
            Err(err) => CallToolResult::error(vec![ContentBlock::text(format!(
                "upstream request failed: {err}"
            ))]),
        }
    }

    /// Assemble the `reqwest` request: resolve the path template, collect query
    /// and header parameters, and attach the JSON body.
    fn build_request(
        &self,
        spec: &ToolSpec,
        base_url: &Url,
        args: &Map<String, Value>,
        forwarded: &HeaderMap,
        bearer: Option<&str>,
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
            base_url.as_str().trim_end_matches('/'),
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

        // Each header has a single source: `check_header_sources` refuses to
        // start otherwise, so none of these can overwrite another.
        for (name, value) in forwarded {
            request = request.header(name.clone(), value.clone());
        }

        if let Some(token) = bearer {
            let value = HeaderValue::from_str(&format!("Bearer {token}"))
                .context("building the Authorization header from the upstream OAuth token")?;
            request = request.header(AUTHORIZATION, value);
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
    fn get_info(&self) -> ServerConfig {
        // `ServerConfig` is `#[non_exhaustive]`, so build from default and set fields.
        // Identify as this crate (not rmcp, which `from_build_env` would report).
        let mut server_info = Implementation::default();
        server_info.name = env!("CARGO_PKG_NAME").to_string();
        server_info.version = env!("CARGO_PKG_VERSION").to_string();

        let mut info = ServerConfig::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = server_info;
        info.instructions = Some(self.state.load().instructions.clone());
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let access = self.caller(&context).access;
        let tools = self
            .state
            .load()
            .tools
            .iter()
            .filter(|spec| self.is_listed(&access, &spec.operation))
            .map(advertised)
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
    ) -> Result<CallToolResponse, ErrorData> {
        // Pin the current snapshot for the whole call so a concurrent reload
        // cannot swap the tool out from under us mid-request.
        let state = self.state.load_full();
        let Some(&idx) = state.index.get(request.name.as_ref()) else {
            return Err(ErrorData::invalid_params(
                Cow::from(format!("unknown tool `{}`", request.name)),
                None,
            ));
        };
        let spec = &state.tools[idx];

        // Enforce JWT role authorization. A tool the caller cannot list is
        // reported as unknown, so the gate does not leak which tools exist; one
        // it listed through anonymous discovery is plainly refused.
        let caller = self.caller(&context);
        if !self.is_allowed(&caller.access, &spec.operation) {
            tracing::warn!(tool = %spec.name, "denying tool call: caller is not authorized");
            let message = if self.is_listed(&caller.access, &spec.operation) {
                format!("tool `{}` needs a bearer token", request.name)
            } else {
                format!("unknown tool `{}`", request.name)
            };
            return Err(ErrorData::invalid_params(Cow::from(message), None));
        }

        // Surface the configured JWT claims on the call for observability. Logged
        // only (never a metric label), and only when `--trace-claim` selected
        // claims that the token actually carried.
        if !caller.traced_claims.is_empty() {
            let claims = Value::Object(caller.traced_claims.clone());
            tracing::info!(
                tool = %spec.name,
                jwt.claims = %claims,
                "tool call carrying traced JWT claims",
            );
        }

        let args = request.arguments.unwrap_or_default();
        let forwarded = self.forwarded_headers(&context);

        let started = std::time::Instant::now();

        // Obtain the upstream OAuth bearer before building the request. A failure
        // here fails the call rather than proxying it unauthenticated, which
        // would surface as a puzzling 401 from the upstream instead of the real
        // cause. The detail stays in the log: the MCP client has no business
        // knowing our provider's internals.
        let bearer = match &self.upstream_token {
            Some(provider) => {
                // A delegating grant needs a verified caller. No identity means
                // no token — never a quiet fall back to the client's own, which
                // would hand this caller the broadest identity the server has.
                let delegation = if provider.needs_caller_identity() {
                    match &caller.identity {
                        Some(identity) => Some(Delegation {
                            issuer: identity.issuer.as_deref(),
                            subject: &identity.subject,
                            expiry: identity.expiry,
                            token: &identity.token,
                        }),
                        None => {
                            tracing::warn!(
                                tool = %spec.name,
                                "denying tool call: the upstream grant delegates, but this call \
                                 carries no verified caller identity",
                            );
                            self.metrics.record_call(
                                &spec.name,
                                Outcome::AuthError,
                                started.elapsed(),
                            );
                            return Ok(CallToolResult::error(vec![ContentBlock::text(
                                "no verified caller identity to obtain an upstream token for",
                            )])
                            .into());
                        }
                    }
                } else {
                    None
                };

                let issued = match &delegation {
                    Some(delegation) => provider.delegated_token(delegation).await,
                    None => provider.access_token().await,
                };
                match issued {
                    Ok(token) => Some(token),
                    Err(err) => {
                        tracing::error!(
                            tool = %spec.name,
                            error = %format!("{err:#}"),
                            "failed to obtain the upstream OAuth token; not calling the API",
                        );
                        self.metrics
                            .record_call(&spec.name, Outcome::AuthError, started.elapsed());
                        return Ok(CallToolResult::error(vec![ContentBlock::text(
                            "could not obtain an upstream OAuth token; see the server logs",
                        )])
                        .into());
                    }
                }
            }
            None => None,
        };

        let result = self
            .execute(spec, &state.base_url, &args, &forwarded, bearer.as_deref())
            .await;
        let outcome = if result.is_error.unwrap_or(false) {
            Outcome::Error
        } else {
            Outcome::Success
        };
        self.metrics
            .record_call(&spec.name, outcome, started.elapsed());

        Ok(result.into())
    }
}

impl OpenApiServer {
    /// Resolve the caller's identity for this request: their access and `sub`.
    ///
    /// A request without a bearer token (always the case on `stdio`/`sse`, which
    /// expose no client headers) or whose token fails verification is anonymous.
    /// `sub` is set only from a successfully verified token.
    fn caller(&self, context: &RequestContext<RoleServer>) -> Caller {
        let Some(authorizer) = self.authorizer.as_ref() else {
            return Caller {
                access: Access::Unrestricted,
                traced_claims: Map::new(),
                identity: None,
            };
        };
        let token = context
            .extensions
            .get::<http::request::Parts>()
            .and_then(|parts| bearer_token(&parts.headers));
        match token {
            Some(token) => match authorizer.verify(token) {
                Ok(claims) => Caller {
                    access: Access::Authenticated(claims.roles),
                    traced_claims: claims.traced,
                    identity: claims.subject.map(|subject| Identity {
                        subject,
                        issuer: claims.issuer,
                        expiry: claims.expiry.and_then(unix_to_instant),
                        token: token.to_string(),
                    }),
                },
                Err(err) => {
                    tracing::warn!(error = %format!("{err:#}"), "rejecting request: JWT verification failed");
                    Caller {
                        access: Access::Anonymous,
                        traced_claims: Map::new(),
                        identity: None,
                    }
                }
            },
            None => {
                if authorizer.anonymous_discovery() {
                    tracing::debug!(
                        "no bearer token: listing every tool, calling the public ones only"
                    );
                } else if authorizer.has_public_tools() {
                    tracing::debug!("no bearer token: serving the public tools only");
                } else {
                    tracing::warn!(
                        "no bearer token on a JWT-protected server: no tool is available"
                    );
                }
                Caller {
                    access: Access::Anonymous,
                    traced_claims: Map::new(),
                    identity: None,
                }
            }
        }
    }

    /// Whether the tool for `operation` is visible/callable with `access`.
    fn is_allowed(&self, access: &Access, operation: &str) -> bool {
        match (access, &self.authorizer) {
            (Access::Unrestricted, _) | (_, None) => true,
            (Access::Anonymous, Some(authorizer)) => authorizer.is_public(operation),
            (Access::Authenticated(roles), Some(authorizer)) => authorizer.allows(roles, operation),
        }
    }

    /// Whether the tool for `operation` appears in `tools/list` for `access`:
    /// every tool an anonymous caller discovers, otherwise the callable ones.
    fn is_listed(&self, access: &Access, operation: &str) -> bool {
        let discovers = matches!(
            (access, &self.authorizer),
            (Access::Anonymous, Some(authorizer)) if authorizer.anonymous_discovery()
        );
        discovers || self.is_allowed(access, operation)
    }

    /// Whether the tool advertised as `name` may be used without a token.
    pub fn is_public_tool(&self, name: &str) -> bool {
        let state = self.state.load();
        let Some(&idx) = state.index.get(name) else {
            return false;
        };
        self.is_allowed(&Access::Anonymous, &state.tools[idx].operation)
    }

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

/// Convert a JWT `exp` (seconds since the Unix epoch) into an [`Instant`].
///
/// `Instant` has no epoch, so the conversion goes through the wall clock: how far
/// away `exp` is from now, added to now. Returns `None` for an expiry already in
/// the past — the verifier rejects those, so it means the clocks disagree, and a
/// zero-length trust window is the safe reading.
/// The MCP tool a [`ToolSpec`] is listed as.
fn advertised(spec: &ToolSpec) -> Tool {
    let tool = Tool::new(
        spec.name.clone(),
        spec.description.clone().unwrap_or_default(),
        spec.input_schema.clone(),
    );
    match &spec.title {
        Some(title) => tool.with_title(title.clone()),
        None => tool,
    }
}

fn unix_to_instant(exp: u64) -> Option<Instant> {
    let now_unix = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    exp.checked_sub(now_unix)
        .map(|remaining| Instant::now() + std::time::Duration::from_secs(remaining))
}

/// Refuse a configuration where a header has two sources, since one of them
/// could never take effect: a static `--header` would always mask the same
/// header forwarded from the caller, and the upstream OAuth token owns
/// `Authorization` outright.
fn check_header_sources(
    extra: &HeaderMap,
    forwarded: &[HeaderName],
    upstream_oauth: bool,
) -> anyhow::Result<()> {
    if let Some(name) = forwarded.iter().find(|name| extra.contains_key(*name)) {
        bail!(
            "`{name}` is both set with --header and forwarded with --forward-header; \
             the static value would always win, so keep only one"
        );
    }
    if upstream_oauth {
        if extra.contains_key(AUTHORIZATION) {
            bail!(
                "--header sets `Authorization`, which the --upstream-oauth-* token also sets; \
                 keep only one"
            );
        }
        if forwarded.contains(&AUTHORIZATION) {
            bail!(
                "--forward-header Authorization would always be replaced by the \
                 --upstream-oauth-* token; keep only one"
            );
        }
    }
    Ok(())
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

/// Build the document-derived [`Snapshot`]: resolve the base URL, apply the
/// operation filter, build the tools and their (renamed) name index, and render
/// the instructions. Shared by the initial build and every reload.
fn build_snapshot(spec: &Spec, cli: &Cli) -> anyhow::Result<Snapshot> {
    let base_url = resolve_base_url(spec, cli)?;

    let filter = OperationFilter::new(FilterConfig {
        include_regexes: cli.include_operations_regex.clone(),
        exclude_regexes: cli.exclude_operations_regex.clone(),
        include_tags: cli.include_tags.clone(),
        exclude_tags: cli.exclude_tags.clone(),
    });
    // The filter matches the raw operation name; the renamer only shapes the
    // name that is finally advertised. See `build_tools`.
    let renamer = ToolRenamer::new(RenameConfig {
        rules: cli.rename_operations.clone(),
        max_len: cli.max_name_len,
    });
    let tools = build_tools(spec, &filter, &renamer);
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
        spec.info().title,
        spec.info().version,
        base_url,
    );

    Ok(Snapshot {
        tools,
        index,
        base_url,
        instructions,
    })
}

/// Determine the upstream base URL: the CLI override wins, otherwise the first
/// absolute `servers` entry of the document.
fn resolve_base_url(spec: &Spec, cli: &Cli) -> anyhow::Result<Url> {
    if let Some(url) = &cli.base_url {
        return Ok(url.clone());
    }
    for server in spec.servers() {
        if let Ok(url) = Url::parse(&server.url) {
            return Ok(url);
        }
    }
    bail!("no usable base URL: the OpenAPI `servers` list is empty or relative; pass --base-url")
}

/// Parse `Name: Value` header strings from the CLI into a [`HeaderMap`].
pub(crate) fn parse_headers(raw: &[String]) -> anyhow::Result<HeaderMap> {
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

/// Shape an upstream response into an MCP tool result.
///
/// The text block keeps its `HTTP {status}\n\n{body}` form: it is what lets a
/// model tell a 404 from a 200, where `isError` only ever says yes or no. When
/// the body parses as JSON it is *also* attached verbatim as
/// `structuredContent`, so a client reads the data without splitting the
/// string. A body that is not JSON — an empty 204, `text/plain`, a gateway's
/// error page — leaves `structuredContent` unset.
fn shape_response(status: reqwest::StatusCode, body: &str) -> CallToolResult {
    let content = vec![ContentBlock::text(format!("HTTP {status}\n\n{body}"))];
    let mut result = if status.is_client_error() || status.is_server_error() {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    };
    // `CallToolResult` is `#[non_exhaustive]`, hence the build-then-assign.
    result.structured_content = serde_json::from_str::<Value>(body).ok();
    result
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
    use clap::Parser as _;

    fn spec_from(yaml: &str) -> Spec {
        let raw: Value = serde_yaml_ng::from_str(yaml).expect("valid YAML");
        Spec::from_value(raw).expect("supported document")
    }

    #[test]
    fn reload_swaps_the_tool_set() {
        let cli = Cli::try_parse_from(["oas2mcp"]).expect("minimal CLI parses");

        const ONE_OP: &str = r#"
openapi: 3.0.0
info: { title: T, version: "1" }
servers: [{ url: "https://api.example.com" }]
paths:
  /a: { get: { operationId: getA, responses: { "200": { description: ok } } } }
"#;
        const TWO_OPS: &str = r#"
openapi: 3.0.0
info: { title: T, version: "2" }
servers: [{ url: "https://api.example.com" }]
paths:
  /a: { get: { operationId: getA, responses: { "200": { description: ok } } } }
  /b: { get: { operationId: getB, responses: { "200": { description: ok } } } }
"#;
        let spec_one = spec_from(ONE_OP);
        let server = OpenApiServer::from_spec(&spec_one, &cli, None, Metrics::disabled())
            .expect("server builds");
        assert_eq!(server.tool_count(), 1);

        let spec_two = spec_from(TWO_OPS);
        server.reload(&spec_two, &cli).expect("reload succeeds");
        assert_eq!(server.tool_count(), 2);
        assert!(server.state.load().index.contains_key("getB"));
    }

    const ONE_GET: &str = r#"
openapi: 3.0.0
info: { title: T, version: "1" }
servers: [{ url: "https://api.example.com" }]
paths:
  /a: { get: { operationId: getA, responses: { "200": { description: ok } } } }
"#;

    #[test]
    fn a_listed_tool_carries_the_summary_as_its_title() {
        const SPEC: &str = r#"
openapi: 3.0.0
info: { title: T, version: "1" }
servers: [{ url: "https://api.example.com" }]
paths:
  /a:
    get:
      operationId: getA
      summary: Get an A
      description: Returns the A.
      responses: { "200": { description: ok } }
  /b: { get: { operationId: getB, responses: { "200": { description: ok } } } }
"#;
        let cli = Cli::try_parse_from(["oas2mcp"]).expect("minimal CLI parses");
        let server = OpenApiServer::from_spec(&spec_from(SPEC), &cli, None, Metrics::disabled())
            .expect("server builds");
        let state = server.state.load();
        let tool = |name: &str| advertised(&state.tools[state.index[name]]);

        let a = tool("getA");
        assert_eq!(a.title.as_deref(), Some("Get an A"));
        assert_eq!(a.description.as_deref(), Some("Get an A\n\nReturns the A."));
        assert_eq!(tool("getB").title, None);
    }

    #[test]
    fn access_rules_match_the_operation_name_not_the_renamed_one() {
        // A rule written against the document keeps meaning the same operation
        // whatever `--rename` does to the advertised name.
        let cli =
            Cli::try_parse_from(["oas2mcp", "--rename", "^getA$=fetch_a"]).expect("CLI parses");
        let authorizer = crate::auth::tests::test_authorizer_with_public_tools(&["^getA$"]);
        let server = OpenApiServer::from_spec(
            &spec_from(ONE_GET),
            &cli,
            Some(Arc::new(authorizer)),
            Metrics::disabled(),
        )
        .expect("server builds");

        assert!(server.is_public_tool("fetch_a"));
        // The original name is not a tool anyone can call any more.
        assert!(!server.is_public_tool("getA"));
    }

    #[test]
    fn anonymous_discovery_lists_every_tool_but_calls_none_beyond_the_public_ones() {
        let cli = Cli::try_parse_from(["oas2mcp"]).expect("minimal CLI parses");
        let authorizer = crate::auth::tests::test_authorizer_with_anonymous_discovery();
        let server = OpenApiServer::from_spec(
            &spec_from(ONE_GET),
            &cli,
            Some(Arc::new(authorizer)),
            Metrics::disabled(),
        )
        .expect("server builds");

        assert!(server.is_listed(&Access::Anonymous, "getA"));
        assert!(!server.is_allowed(&Access::Anonymous, "getA"));
        // A verified caller still lists only what its roles grant.
        let nobody = Access::Authenticated(HashSet::from(["nobody".to_string()]));
        assert!(!server.is_listed(&nobody, "getA"));
        let admin = Access::Authenticated(HashSet::from(["admin".to_string()]));
        assert!(server.is_listed(&admin, "getA"));
        assert!(server.is_allowed(&admin, "getA"));
    }

    #[test]
    fn without_anonymous_discovery_an_anonymous_caller_lists_the_public_tools_only() {
        let cli = Cli::try_parse_from(["oas2mcp"]).expect("minimal CLI parses");
        let server = OpenApiServer::from_spec(
            &spec_from(ONE_GET),
            &cli,
            Some(Arc::new(crate::auth::tests::test_authorizer())),
            Metrics::disabled(),
        )
        .expect("server builds");

        assert!(!server.is_listed(&Access::Anonymous, "getA"));
    }

    /// Build the request one tool call would send, and hand back its headers.
    ///
    /// Goes through the real `build_request`, so the test sees exactly what
    /// the upstream would receive.
    fn authorization_of(
        args: &[&str],
        forwarded: &[(&str, &str)],
        bearer: Option<&str>,
    ) -> Vec<String> {
        let cli = Cli::try_parse_from(["oas2mcp", "http"].into_iter().chain(args.iter().copied()))
            .expect("CLI parses");
        let spec = spec_from(ONE_GET);
        let server = OpenApiServer::from_spec(&spec, &cli, None, Metrics::disabled())
            .expect("server builds");

        let mut incoming = HeaderMap::new();
        for (name, value) in forwarded {
            incoming.append(
                HeaderName::from_bytes(name.as_bytes()).expect("valid header name"),
                HeaderValue::from_str(value).expect("valid header value"),
            );
        }

        let state = server.state.load();
        let request = server
            .build_request(
                &state.tools[0],
                &state.base_url,
                &Map::new(),
                &incoming,
                bearer,
            )
            .expect("the request builds")
            .build()
            .expect("the request is well-formed");

        request
            .headers()
            .get_all(AUTHORIZATION)
            .iter()
            .map(|value| value.to_str().expect("ASCII header").to_string())
            .collect()
    }

    #[test]
    fn the_oauth_token_becomes_the_upstream_authorization() {
        assert_eq!(
            authorization_of(&[], &[], Some("tok-1")),
            vec!["Bearer tok-1".to_string()]
        );
    }

    #[test]
    fn a_forwarded_authorization_applies_without_a_token() {
        assert_eq!(
            authorization_of(
                &["--forward-header", "Authorization"],
                &[("authorization", "Bearer from-caller")],
                None,
            ),
            vec!["Bearer from-caller".to_string()]
        );
    }

    #[test]
    fn forwarded_headers_travel_beside_the_token() {
        let cli = Cli::try_parse_from(["oas2mcp", "http", "--forward-header", "X-Tenant"])
            .expect("CLI parses");
        let spec = spec_from(ONE_GET);
        let server = OpenApiServer::from_spec(&spec, &cli, None, Metrics::disabled())
            .expect("server builds");

        let mut incoming = HeaderMap::new();
        incoming.insert("x-tenant", HeaderValue::from_static("acme"));

        let state = server.state.load();
        let request = server
            .build_request(
                &state.tools[0],
                &state.base_url,
                &Map::new(),
                &incoming,
                Some("tok-1"),
            )
            .expect("the request builds")
            .build()
            .expect("the request is well-formed");

        assert_eq!(
            request
                .headers()
                .get("x-tenant")
                .map(|v| v.to_str().unwrap()),
            Some("acme")
        );
        assert_eq!(
            request
                .headers()
                .get(AUTHORIZATION)
                .map(|v| v.to_str().unwrap()),
            Some("Bearer tok-1")
        );
    }

    #[test]
    fn a_header_with_two_sources_is_refused() {
        let headers = |raw: &[&str]| {
            parse_headers(&raw.iter().map(|h| h.to_string()).collect::<Vec<_>>())
                .expect("valid headers")
        };
        let names = |raw: &[&str]| {
            parse_header_names(&raw.iter().map(|h| h.to_string()).collect::<Vec<_>>())
                .expect("valid names")
        };

        // Static and forwarded under one name: the forwarded value never lands.
        assert!(
            check_header_sources(&headers(&["X-Tenant: acme"]), &names(&["x-tenant"]), false)
                .is_err()
        );
        // The upstream token owns `Authorization`, from either other source.
        assert!(check_header_sources(&headers(&["Authorization: Basic x"]), &[], true).is_err());
        assert!(check_header_sources(&HeaderMap::new(), &names(&["Authorization"]), true).is_err());

        // One source per header is fine, whichever it is.
        assert!(
            check_header_sources(
                &headers(&["Authorization: Basic x"]),
                &names(&["X-Tenant"]),
                false
            )
            .is_ok()
        );
        assert!(check_header_sources(&HeaderMap::new(), &names(&["Authorization"]), false).is_ok());
        assert!(
            check_header_sources(&headers(&["X-Api-Key: k"]), &names(&["X-Tenant"]), true).is_ok()
        );
    }

    #[test]
    fn no_upstream_oauth_means_no_provider() {
        let cli = Cli::try_parse_from(["oas2mcp"]).expect("minimal CLI parses");
        let spec = spec_from(ONE_GET);
        let server = OpenApiServer::from_spec(&spec, &cli, None, Metrics::disabled())
            .expect("server builds");
        assert!(server.upstream_token.is_none());
    }

    #[test]
    fn upstream_oauth_flags_build_a_provider() {
        let cli = Cli::try_parse_from([
            "oas2mcp",
            "--upstream-oauth-token-url",
            "https://idp.example.com/token",
            "--upstream-oauth-client-id",
            "id",
            "--upstream-oauth-client-secret",
            "secret",
        ])
        .expect("CLI parses");
        let spec = spec_from(ONE_GET);
        let server = OpenApiServer::from_spec(&spec, &cli, None, Metrics::disabled())
            .expect("server builds");
        assert!(server.upstream_token.is_some());
    }

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

    /// The single text block of a shaped result.
    fn text_of(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|block| block.as_text().map(|t| t.text.clone()))
            .collect()
    }

    #[test]
    fn a_json_body_becomes_structured_content_without_losing_the_text_block() {
        let body = r#"{"id":7,"name":"rex","tags":["good","boy"]}"#;
        let result = shape_response(reqwest::StatusCode::OK, body);

        // The machine-readable half: the body verbatim, no re-shaping.
        assert_eq!(
            result.structured_content,
            Some(serde_json::from_str::<Value>(body).expect("the fixture is JSON"))
        );
        // The human-readable half is untouched, status prefix included.
        assert_eq!(text_of(&result), format!("HTTP 200 OK\n\n{body}"));
        assert_eq!(result.is_error, Some(false));
    }

    #[test]
    fn a_non_json_body_carries_no_structured_content() {
        // An empty 204, a `text/plain` payload, a gateway's HTML error page:
        // nothing to attach, and the text block stays exactly as it was.
        for (status, body) in [
            (reqwest::StatusCode::NO_CONTENT, ""),
            (reqwest::StatusCode::OK, "pong"),
            (reqwest::StatusCode::OK, "<html>not json</html>"),
        ] {
            let result = shape_response(status, body);
            assert_eq!(result.structured_content, None, "body: {body:?}");
            assert_eq!(text_of(&result), format!("HTTP {status}\n\n{body}"));
        }
    }

    #[test]
    fn an_upstream_error_keeps_is_error_alongside_its_structured_body() {
        let body = r#"{"error":"not found","code":404}"#;
        let result = shape_response(reqwest::StatusCode::NOT_FOUND, body);

        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content,
            Some(serde_json::from_str::<Value>(body).expect("the fixture is JSON"))
        );
        assert_eq!(text_of(&result), format!("HTTP 404 Not Found\n\n{body}"));
    }

    #[test]
    fn a_json_scalar_body_is_still_structured_content() {
        // `structuredContent` is any JSON value, not only an object — a bare
        // array or number from an upstream is worth attaching too.
        let result = shape_response(reqwest::StatusCode::OK, "[1,2,3]");
        assert_eq!(
            result.structured_content,
            Some(serde_json::json!([1, 2, 3]))
        );
    }
}
