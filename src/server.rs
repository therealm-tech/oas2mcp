//! The MCP server: advertises one tool per OpenAPI operation and executes a
//! tool call by proxying it as an HTTP request to the upstream API.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, bail};
use arc_swap::ArcSwap;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Icon,
    Implementation, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
    ReadResourceRequestMethod, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult,
    Resource, ResourceContents, ResourcesCapability, ServerCapabilities, ServerConfig,
    SubscriptionFilter, Tool,
};
use rmcp::service::{NotificationContext, Peer, RequestContext, RoleServer, SubscriptionContext};
use rmcp::{ErrorData, ServerHandler};
use serde_json::{Map, Value};
use tokio::sync::watch;
use url::Url;

use crate::auth::{Authorizer, bearer_token};
use crate::cli::Cli;
use crate::filter::{FilterConfig, OperationFilter};
use crate::oauth::{Delegation, TokenProvider};
use crate::openapi::Spec;
use crate::openapi::prune::prune;
use crate::pagination;
use crate::rename::{RenameConfig, ToolRenamer};
use crate::telemetry::{Metrics, Outcome};
use crate::tools::{Param, ParamLocation, ToolSpec, build_tools};

/// URI of the OpenAPI document resource (`--openapi-resource`).
const OPENAPI_URI: &str = "openapi://document";

/// The part of the server that an OpenAPI reload replaces: the resolved tools,
/// their name index, the upstream base URL (which may be derived from the
/// document's `servers`), the API title, the instructions string and, with
/// `--openapi-resource`, the document itself. Swapped atomically as a whole so
/// a reload never exposes a half-updated state.
struct Snapshot {
    tools: Vec<ToolSpec>,
    index: HashMap<String, usize>,
    base_url: Url,
    title: Option<String>,
    instructions: String,
    /// Identifies the tool set, so a `tools/list` cursor cut from an earlier
    /// snapshot is recognised as stale.
    fingerprint: u64,
    /// The document served as a resource; `None` when the resource is off.
    document: Option<Spec>,
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
    /// Advertise each tool with the behaviour hints of its HTTP method.
    annotate_tools: bool,
    /// `tools/list` page size. `None` lists every tool in one reply.
    tools_page_size: Option<NonZeroUsize>,
    /// Tells connected clients the tool list changed. `None` when the document
    /// is never reloaded, so the list cannot change.
    list_changed: Option<ListChanged>,
}

/// Fans a tool-list change out to every connected client. A session opened
/// with `initialize` (protocol before 2026-07-28) is notified through its peer;
/// a 2026-07-28 client only receives it on a `subscriptions/listen` stream,
/// which waits on `generation`.
#[derive(Clone)]
struct ListChanged {
    generation: Arc<watch::Sender<u64>>,
    peers: Arc<Mutex<Vec<Peer<RoleServer>>>>,
}

impl ListChanged {
    fn new() -> Self {
        Self {
            generation: Arc::new(watch::Sender::new(0)),
            peers: Arc::default(),
        }
    }

    /// Remember a session's peer, forgetting the ones whose session is over.
    fn register(&self, peer: Peer<RoleServer>) {
        let mut peers = self.peers.lock().expect("peers mutex");
        peers.retain(|peer| !peer.is_transport_closed());
        peers.push(peer);
        tracing::debug!(
            sessions = peers.len(),
            "session registered for tools/list_changed"
        );
    }

    fn notify(&self) {
        self.generation.send_modify(|generation| *generation += 1);
        let mut peers = self.peers.lock().expect("peers mutex");
        peers.retain(|peer| !peer.is_transport_closed());
        tracing::debug!(
            sessions = peers.len(),
            subscriptions = self.generation.receiver_count(),
            "notifying clients that the tool list changed"
        );
        for peer in peers.iter() {
            let peer = peer.clone();
            tokio::spawn(async move {
                if let Err(err) = peer.notify_tool_list_changed().await {
                    tracing::debug!(error = %err, "could not send tools/list_changed to a session");
                }
            });
        }
    }
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
        tracing::debug!(
            auto_tool_annotations = cli.auto_tool_annotations,
            "configured tool annotations"
        );
        // Mirrors the condition under which `main` spawns the reload loop.
        let reloads = cli.reload_every.is_some() && cli.openapi_url.is_some();

        Ok(Self {
            state: Arc::new(ArcSwap::from_pointee(snapshot)),
            client,
            extra_headers: Arc::new(extra_headers),
            forward_headers: Arc::new(forward_headers),
            authorizer,
            upstream_token,
            metrics,
            annotate_tools: cli.auto_tool_annotations,
            tools_page_size: cli.tools_page_size,
            list_changed: reloads.then(ListChanged::new),
        })
    }

    /// Rebuild the tool set from a freshly fetched document and swap it in
    /// atomically. The static config (auth headers, forwarded header names, the
    /// HTTP client) is untouched. If the new document yields no usable tools,
    /// the swap still happens — that is what the document now says. Connected
    /// clients are told when the advertised tools differ from the previous set.
    pub fn reload(&self, spec: &Spec, cli: &Cli) -> anyhow::Result<()> {
        let snapshot = build_snapshot(spec, cli)?;
        let tools = snapshot.tools.len();
        let previous = self.state.swap(Arc::new(snapshot));
        let changed = !advertise_the_same(&previous.tools, &self.state.load().tools);
        tracing::info!(tools, changed, "reloaded the OpenAPI document");
        if changed && let Some(list_changed) = &self.list_changed {
            list_changed.notify();
        }
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
        let result = match request.send().await {
            Ok(response) => {
                let status = response.status();
                let mime = response
                    .headers()
                    .get(CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .map(mime_essence);
                let kind = body_kind(mime.as_deref());
                tracing::debug!(tool = %spec.name, %status, mime = ?mime, ?kind, "shaping upstream response");
                if kind == BodyKind::Text {
                    let body = response.text().await.unwrap_or_default();
                    shape_response(status, &body)
                } else {
                    let uri = response.url().to_string();
                    let body = response.bytes().await.unwrap_or_default();
                    shape_bytes_response(status, kind, mime.as_deref(), &body, &uri)
                }
            }
            Err(err) => CallToolResult::error(vec![ContentBlock::text(format!(
                "upstream request failed: {err}"
            ))]),
        };
        fit_output_schema(result, spec.output_schema.is_some())
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
        let state = self.state.load();
        let mut server_info =
            Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
                .with_website_url(env!("CARGO_PKG_REPOSITORY"))
                .with_icons(vec![logo_icon()]);
        server_info.title.clone_from(&state.title);

        let mut info = ServerConfig::default();
        let mut capabilities = ServerCapabilities::builder().enable_tools().build();
        if self.list_changed.is_some()
            && let Some(tools) = capabilities.tools.as_mut()
        {
            tools.list_changed = Some(true);
        }
        if state.document.is_some() {
            capabilities.resources = Some(ResourcesCapability::default());
        }
        info.capabilities = capabilities;
        info.server_info = server_info;
        info.instructions = Some(state.instructions.clone());
        info
    }

    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        tracing::info!("client initialized");
        // `initialize` always negotiates a version before 2026-07-28, so this
        // session takes unsolicited notifications.
        if let Some(list_changed) = &self.list_changed {
            list_changed.register(context.peer);
        }
    }

    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        self.list_changed
            .as_ref()
            .map(|_| requested.supported_by(&self.get_info().capabilities))
    }

    async fn listen(&self, context: SubscriptionContext) -> Result<(), ErrorData> {
        let Some(list_changed) = &self.list_changed else {
            context.cancelled().await;
            return Ok(());
        };
        let mut generation = list_changed.generation.subscribe();
        tracing::debug!("subscription opened for tools/list_changed");
        loop {
            tokio::select! {
                () = context.cancelled() => break,
                changed = generation.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    if let Err(err) = context.sink().notify_tool_list_changed().await {
                        tracing::debug!(error = %err, "could not send tools/list_changed to a subscription");
                        break;
                    }
                }
            }
        }
        tracing::debug!("subscription closed");
        Ok(())
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let access = self.caller(&context).access;
        let cursor = request.as_ref().and_then(|params| params.cursor.as_deref());
        self.list_page(&access, cursor)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(self.openapi_resources())
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        if self.state.load().document.is_none() {
            return Err(ErrorData::method_not_found::<ReadResourceRequestMethod>());
        }
        let access = self.caller(&context).access;
        self.read_openapi(&request.uri, &access).map(Into::into)
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

        match self
            .call_until_cancelled(
                spec,
                &state.base_url,
                &args,
                &forwarded,
                &caller,
                context.ct.cancelled(),
            )
            .await
        {
            Some(result) => Ok(result.into()),
            // rmcp drops whatever a cancelled request returns, so this error
            // never reaches the client, as the spec asks.
            None => Err(ErrorData::internal_error("request cancelled", None)),
        }
    }
}

impl OpenApiServer {
    /// Run a tool call unless `cancelled` completes first, and record its
    /// metric. `None` means cancelled: dropping the call future aborts the
    /// upstream request in flight, which rmcp does not do on its own — it only
    /// cancels the request's token.
    async fn call_until_cancelled(
        &self,
        spec: &ToolSpec,
        base_url: &Url,
        args: &Map<String, Value>,
        forwarded: &HeaderMap,
        caller: &Caller,
        cancelled: impl Future<Output = ()>,
    ) -> Option<CallToolResult> {
        let started = Instant::now();
        tokio::select! {
            biased;
            () = cancelled => {
                tracing::info!(tool = %spec.name, "tool call cancelled, upstream request aborted");
                self.metrics.record_call(&spec.name, Outcome::Cancelled, started.elapsed());
                None
            }
            (result, outcome) = self.call(spec, base_url, args, forwarded, caller) => {
                self.metrics.record_call(&spec.name, outcome, started.elapsed());
                Some(result)
            }
        }
    }

    /// Obtain the upstream token when one is configured, then proxy the call.
    async fn call(
        &self,
        spec: &ToolSpec,
        base_url: &Url,
        args: &Map<String, Value>,
        forwarded: &HeaderMap,
        caller: &Caller,
    ) -> (CallToolResult, Outcome) {
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
                            return (
                                CallToolResult::error(vec![ContentBlock::text(
                                    "no verified caller identity to obtain an upstream token for",
                                )]),
                                Outcome::AuthError,
                            );
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
                        return (
                            CallToolResult::error(vec![ContentBlock::text(
                                "could not obtain an upstream OAuth token; see the server logs",
                            )]),
                            Outcome::AuthError,
                        );
                    }
                }
            }
            None => None,
        };

        let result = self
            .execute(spec, base_url, args, forwarded, bearer.as_deref())
            .await;
        let outcome = if result.is_error.unwrap_or(false) {
            Outcome::Error
        } else {
            Outcome::Success
        };
        (result, outcome)
    }
}

impl OpenApiServer {
    /// The page of `tools/list` that `cursor` points to, among the tools
    /// `access` may list.
    ///
    /// The cursor is bound to that exact list: a reload that changed the tool
    /// set, or a caller whose roles changed, turns it stale, and the client is
    /// told to start over rather than silently missing or repeating tools.
    fn list_page(
        &self,
        access: &Access,
        cursor: Option<&str>,
    ) -> Result<ListToolsResult, ErrorData> {
        let state = self.state.load();
        let listed: Vec<&ToolSpec> = state
            .tools
            .iter()
            .filter(|spec| self.is_listed(access, &spec.operation))
            .collect();
        let names: Vec<&str> = listed.iter().map(|spec| spec.name.as_str()).collect();
        let fingerprint = pagination::fingerprint((state.fingerprint, names));
        let (page, next_cursor) =
            pagination::page(&listed, fingerprint, self.tools_page_size, cursor).map_err(
                |err| {
                    tracing::debug!(cursor, error = %err, "refusing a tools/list cursor");
                    ErrorData::invalid_params(Cow::from(err.to_string()), None)
                },
            )?;
        tracing::debug!(
            listed = listed.len(),
            page = page.len(),
            more = next_cursor.is_some(),
            "listing tools"
        );
        let tools = page
            .iter()
            .map(|spec| advertised(spec, self.annotate_tools))
            .collect();
        Ok(ListToolsResult {
            tools,
            next_cursor,
            ..Default::default()
        })
    }

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

    /// The resources on offer: the OpenAPI document, when it is exposed.
    fn openapi_resources(&self) -> ListResourcesResult {
        let state = self.state.load();
        let resources = state
            .document
            .as_ref()
            .map(|spec| {
                Resource::new(OPENAPI_URI, "openapi")
                    .with_title(format!("{} OpenAPI document", spec.info().title))
                    .with_description(
                        "The OpenAPI document of the upstream API, limited to the operations \
                         you can list as tools.",
                    )
                    .with_mime_type("application/json")
            })
            .into_iter()
            .collect();
        ListResourcesResult {
            resources,
            next_cursor: None,
            ..Default::default()
        }
    }

    /// Read the resource at `uri` as a caller with `access`: the OpenAPI
    /// document cut down to the operations that caller lists as tools. The
    /// resource must be exposed.
    fn read_openapi(&self, uri: &str, access: &Access) -> Result<ReadResourceResult, ErrorData> {
        if uri != OPENAPI_URI {
            return Err(ErrorData::resource_not_found(
                format!("unknown resource `{uri}`"),
                Some(serde_json::json!({ "uri": uri })),
            ));
        }
        let state = self.state.load_full();
        let Some(spec) = state.document.as_ref() else {
            return Err(ErrorData::method_not_found::<ReadResourceRequestMethod>());
        };
        let listed: HashSet<(&str, String)> = state
            .tools
            .iter()
            .filter(|tool| self.is_listed(access, &tool.operation))
            .map(|tool| {
                (
                    tool.path_template.as_str(),
                    tool.method.as_str().to_ascii_lowercase(),
                )
            })
            .collect();
        tracing::debug!(
            operations = listed.len(),
            "serving the OpenAPI document cut down to the caller's tools"
        );
        let document = prune(spec, |path, method| {
            listed.contains(&(path, method.to_string()))
        });
        let text = serde_json::to_string(&document).map_err(|err| {
            ErrorData::internal_error(format!("serialising the OpenAPI document: {err}"), None)
        })?;
        // The content depends on who asks: no cache may serve it to another caller.
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(text, OPENAPI_URI).with_mime_type("application/json"),
        ])
        .with_cache_scope(CacheScope::Private))
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

/// The MCP tool a [`ToolSpec`] is listed as, with the hints its HTTP method
/// implies when `annotate` is set.
fn advertised(spec: &ToolSpec, annotate: bool) -> Tool {
    let mut tool = Tool::new(
        spec.name.clone(),
        spec.description.clone().unwrap_or_default(),
        spec.input_schema.clone(),
    );
    if let Some(title) = &spec.title {
        tool = tool.with_title(title.clone());
    }
    if let Some(schema) = &spec.output_schema {
        tool = tool.with_raw_output_schema(schema.clone());
    }
    if annotate {
        tool = tool.with_annotations(spec.annotations());
    }
    tool
}

/// Convert a JWT `exp` (seconds since the Unix epoch) into an [`Instant`].
///
/// `Instant` has no epoch, so the conversion goes through the wall clock: how far
/// away `exp` is from now, added to now. Returns `None` for an expiry already in
/// the past — the verifier rejects those, so it means the clocks disagree, and a
/// zero-length trust window is the safe reading.
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

/// Whether two tool sets look the same to a client: the same `tools/list`
/// entries, in any order. The operation name counts too, since the access rules
/// match it and so decide who sees the tool.
fn advertise_the_same(before: &[ToolSpec], after: &[ToolSpec]) -> bool {
    fn listing(tools: &[ToolSpec]) -> Vec<(&str, Tool)> {
        let mut listing: Vec<_> = tools
            .iter()
            .map(|spec| (spec.operation.as_str(), advertised(spec, true)))
            .collect();
        listing.sort_by(|a, b| a.1.name.cmp(&b.1.name));
        listing
    }
    listing(before) == listing(after)
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
    let tools = build_tools(spec, &filter, &renamer, cli.tool_output_schema);
    if tools.is_empty() {
        tracing::warn!("the OpenAPI document defines no usable operations");
    }
    let index = tools
        .iter()
        .enumerate()
        .map(|(i, t)| (t.name.clone(), i))
        .collect();

    let document = cli.openapi_resource.then(|| spec.clone());

    let instructions = format!(
        "MCP server proxying the \"{}\" API (version {}). \
         Each tool maps to one OpenAPI operation and is executed as an HTTP \
         request against {}. Path/query/header parameters are top-level tool \
         arguments; a JSON request body is passed as the `body` argument.",
        spec.info().title,
        spec.info().version,
        base_url,
    );

    let title = Some(spec.info().title.trim())
        .filter(|t| !t.is_empty())
        .map(str::to_owned);

    let fingerprint = pagination::fingerprint(
        tools
            .iter()
            .map(|tool| {
                (
                    &tool.name,
                    &tool.operation,
                    &tool.title,
                    &tool.description,
                    serde_json::to_string(&tool.input_schema).unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>(),
    );

    Ok(Snapshot {
        tools,
        index,
        base_url,
        title,
        instructions,
        fingerprint,
        document,
    })
}

/// The project logo as a self-contained `data:` URI, so a client can render it
/// without reaching the network.
fn logo_icon() -> Icon {
    const LOGO: &[u8] = include_bytes!("../logo.svg");
    let src = format!("data:image/svg+xml;base64,{}", STANDARD.encode(LOGO));
    Icon::new(src)
        .with_mime_type("image/svg+xml")
        .with_sizes(vec!["any".to_owned()])
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
///
/// Protocol versions up to 2025-11-25 type `structuredContent` as a JSON
/// object, so an array or scalar body is wrapped as `{"result": <body>}`.
fn shape_response(status: reqwest::StatusCode, body: &str) -> CallToolResult {
    let mut result = result_for(
        status,
        vec![ContentBlock::text(format!("HTTP {status}\n\n{body}"))],
    );
    // `CallToolResult` is `#[non_exhaustive]`, hence the build-then-assign.
    result.structured_content = serde_json::from_str::<Value>(body)
        .ok()
        .map(|value| match value {
            Value::Object(_) => value,
            other => serde_json::json!({ "result": other }),
        });
    result
}

/// Drop the `structuredContent` of an error result when the tool declares an
/// output schema: MCP requires it to conform to that schema, which describes
/// the success body, and the specification exempts no result from that. The
/// error body stays in the text block.
fn fit_output_schema(mut result: CallToolResult, has_output_schema: bool) -> CallToolResult {
    if has_output_schema && result.is_error == Some(true) {
        result.structured_content = None;
    }
    result
}

fn result_for(status: reqwest::StatusCode, content: Vec<ContentBlock>) -> CallToolResult {
    if status.is_client_error() || status.is_server_error() {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    }
}

/// How an upstream body reaches the model, decided by its media type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyKind {
    /// Read as text: [`shape_response`].
    Text,
    /// An MCP `image` block.
    Image,
    /// An MCP `audio` block.
    Audio,
    /// An embedded resource carrying the bytes as a base64 `blob`.
    Blob,
    /// No `Content-Type`: text when the body is valid UTF-8, a blob otherwise.
    Unknown,
}

/// The `type/subtype` of a `Content-Type` value, lowercased, parameters dropped.
fn mime_essence(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

fn body_kind(mime: Option<&str>) -> BodyKind {
    let Some(mime) = mime.filter(|mime| !mime.is_empty()) else {
        return BodyKind::Unknown;
    };
    let (kind, subtype) = mime.split_once('/').unwrap_or((mime, ""));
    // Checked before `image/`, so `image/svg+xml` stays readable text.
    let textual = kind == "text"
        || subtype.ends_with("+json")
        || subtype.ends_with("+xml")
        || subtype.ends_with("+yaml")
        || (kind == "application"
            && matches!(
                subtype,
                "json"
                    | "xml"
                    | "yaml"
                    | "x-yaml"
                    | "javascript"
                    | "x-ndjson"
                    | "x-www-form-urlencoded"
            ));
    match kind {
        _ if textual => BodyKind::Text,
        "image" => BodyKind::Image,
        "audio" => BodyKind::Audio,
        _ => BodyKind::Blob,
    }
}

/// Shape a body that is not read as text. The leading text block keeps the
/// status visible to a client that ignores images, audio and resources.
fn shape_bytes_response(
    status: reqwest::StatusCode,
    kind: BodyKind,
    mime: Option<&str>,
    body: &[u8],
    uri: &str,
) -> CallToolResult {
    if body.is_empty() {
        return shape_response(status, "");
    }
    if kind == BodyKind::Unknown
        && let Ok(text) = std::str::from_utf8(body)
    {
        return shape_response(status, text.strip_prefix('\u{feff}').unwrap_or(text));
    }
    let mime = mime
        .filter(|mime| !mime.is_empty())
        .unwrap_or("application/octet-stream");
    let data = STANDARD.encode(body);
    let block = match kind {
        BodyKind::Image => ContentBlock::image(data, mime),
        BodyKind::Audio => ContentBlock::audio(data, mime),
        _ => ContentBlock::resource(ResourceContents::blob(data, uri).with_mime_type(mime)),
    };
    let summary = ContentBlock::text(format!("HTTP {status}, {mime}, {} bytes", body.len()));
    result_for(status, vec![summary, block])
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
    use crate::auth::Authorizer;
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

    fn server_info_for(title: &str) -> Implementation {
        let cli = Cli::try_parse_from(["oas2mcp"]).expect("minimal CLI parses");
        let spec = spec_from(&ONE_GET.replace("title: T", &format!("title: {title:?}")));
        let server = OpenApiServer::from_spec(&spec, &cli, None, Metrics::disabled())
            .expect("server builds");
        server.get_info().server_info
    }

    #[test]
    fn server_info_identifies_the_crate_and_titles_it_after_the_api() {
        let info = server_info_for("Pet Store");
        assert_eq!(info.name, "oas2mcp");
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(info.title.as_deref(), Some("Pet Store"));
        assert_eq!(
            info.website_url.as_deref(),
            Some("https://github.com/therealm-tech/oas2mcp")
        );
    }

    #[test]
    fn server_info_omits_a_blank_api_title() {
        assert_eq!(server_info_for("  ").title, None);
    }

    #[test]
    fn server_title_follows_a_reload() {
        let cli = Cli::try_parse_from(["oas2mcp"]).expect("minimal CLI parses");
        let server = OpenApiServer::from_spec(&spec_from(ONE_GET), &cli, None, Metrics::disabled())
            .expect("server builds");
        let renamed = spec_from(&ONE_GET.replace("title: T", "title: U"));
        server.reload(&renamed, &cli).expect("reload succeeds");
        assert_eq!(server.get_info().server_info.title.as_deref(), Some("U"));
    }

    #[test]
    fn server_icon_is_the_logo_as_a_data_uri() {
        let icons = server_info_for("T").icons.expect("an icon is advertised");
        let [icon] = icons.as_slice() else {
            panic!("exactly one icon, got {icons:?}");
        };
        assert_eq!(icon.mime_type.as_deref(), Some("image/svg+xml"));
        assert_eq!(icon.sizes, Some(vec!["any".to_owned()]));
        let encoded = icon
            .src
            .strip_prefix("data:image/svg+xml;base64,")
            .expect("a base64 SVG data URI");
        let decoded = STANDARD.decode(encoded).expect("valid base64");
        assert_eq!(decoded, include_bytes!("../logo.svg"));
    }

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
        let tool = |name: &str| advertised(&state.tools[state.index[name]], false);

        let a = tool("getA");
        assert_eq!(a.title.as_deref(), Some("Get an A"));
        assert_eq!(a.description.as_deref(), Some("Get an A\n\nReturns the A."));
        assert_eq!(tool("getB").title, None);
    }

    const TWO_GETS: &str = r#"
openapi: 3.0.0
info: { title: T, version: "2" }
servers: [{ url: "https://api.example.com" }]
paths:
  /a: { get: { operationId: getA, responses: { "200": { description: ok } } } }
  /b: { get: { operationId: getB, responses: { "200": { description: ok } } } }
"#;

    fn tools_of(yaml: &str) -> Vec<ToolSpec> {
        let cli = Cli::try_parse_from(["oas2mcp"]).expect("minimal CLI parses");
        build_snapshot(&spec_from(yaml), &cli)
            .expect("snapshot builds")
            .tools
    }

    #[test]
    fn a_new_document_that_advertises_the_same_tools_is_no_change() {
        // Only `info.version` and the path order differ.
        let reordered = r#"
openapi: 3.0.0
info: { title: T, version: "3" }
servers: [{ url: "https://api.example.com" }]
paths:
  /b: { get: { operationId: getB, responses: { "200": { description: ok } } } }
  /a: { get: { operationId: getA, responses: { "200": { description: ok } } } }
"#;
        assert!(advertise_the_same(
            &tools_of(TWO_GETS),
            &tools_of(reordered)
        ));
    }

    #[test]
    fn a_tool_added_described_or_reshaped_is_a_change() {
        let described = r#"
openapi: 3.0.0
info: { title: T, version: "1" }
servers: [{ url: "https://api.example.com" }]
paths:
  /a: { get: { operationId: getA, summary: Fetch A, responses: { "200": { description: ok } } } }
"#;
        let with_param = r#"
openapi: 3.0.0
info: { title: T, version: "1" }
servers: [{ url: "https://api.example.com" }]
paths:
  /a:
    get:
      operationId: getA
      parameters: [{ name: q, in: query, schema: { type: string } }]
      responses: { "200": { description: ok } }
"#;
        let before = tools_of(ONE_GET);
        assert!(!advertise_the_same(&before, &tools_of(TWO_GETS)));
        assert!(!advertise_the_same(&before, &tools_of(described)));
        assert!(!advertise_the_same(&before, &tools_of(with_param)));
    }

    /// A server that reloads, as `--reload-every` with a document URL sets up.
    fn reloading_server() -> (OpenApiServer, Cli) {
        let cli = Cli::try_parse_from([
            "oas2mcp",
            "--openapi-url",
            "https://api.example.com/openapi.yaml",
            "--reload-every",
            "1m",
        ])
        .expect("CLI parses");
        let server = OpenApiServer::from_spec(&spec_from(ONE_GET), &cli, None, Metrics::disabled())
            .expect("server builds");
        (server, cli)
    }

    #[test]
    fn list_changed_is_advertised_only_when_the_document_reloads() {
        let list_changed = |server: &OpenApiServer| {
            server
                .get_info()
                .capabilities
                .tools
                .and_then(|tools| tools.list_changed)
        };
        let (server, _) = reloading_server();
        assert_eq!(list_changed(&server), Some(true));

        // `--reload-every` without a URL reloads nothing.
        let cli = Cli::try_parse_from(["oas2mcp", "--reload-every", "1m"]).expect("CLI parses");
        let server = OpenApiServer::from_spec(&spec_from(ONE_GET), &cli, None, Metrics::disabled())
            .expect("server builds");
        assert_eq!(list_changed(&server), None);
    }

    /// Counts the `notifications/tools/list_changed` a legacy client receives.
    #[derive(Clone)]
    struct Counting(tokio::sync::mpsc::UnboundedSender<()>);

    impl rmcp::ClientHandler for Counting {
        async fn on_tool_list_changed(&self, _context: NotificationContext<rmcp::RoleClient>) {
            let _ = self.0.send(());
        }
    }

    const NOTIFICATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

    #[tokio::test]
    async fn an_initialized_session_is_told_when_the_tools_change() {
        use rmcp::ServiceExt as _;

        let (server, cli) = reloading_server();
        let (server_io, client_io) = tokio::io::duplex(64 * 1024);
        let running = server.clone();
        tokio::spawn(async move {
            if let Ok(service) = running.serve(server_io).await {
                let _ = service.waiting().await;
            }
        });
        let (tx, mut notifications) = tokio::sync::mpsc::unbounded_channel();
        let client = Counting(tx)
            .serve(client_io)
            .await
            .expect("client connects");

        // `notifications/initialized` is handled asynchronously.
        let registered = server.list_changed.clone().expect("the server reloads");
        tokio::time::timeout(NOTIFICATION_TIMEOUT, async {
            while registered.peers.lock().expect("peers mutex").is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the session registers");

        server.reload(&spec_from(ONE_GET), &cli).expect("reload");
        server.reload(&spec_from(TWO_GETS), &cli).expect("reload");
        tokio::time::timeout(NOTIFICATION_TIMEOUT, notifications.recv())
            .await
            .expect("a notification arrives")
            .expect("the channel is open");
        // A round trip flushes anything the unchanged reload could have sent.
        client.list_tools(None).await.expect("tools/list");
        assert!(
            notifications.try_recv().is_err(),
            "one change, one notification"
        );
    }

    #[tokio::test]
    async fn a_subscription_is_told_when_the_tools_change() {
        use rmcp::model::{ProtocolVersion, ServerNotification};
        use rmcp::{ClientLifecycleMode, ClientServiceExt as _, ServiceExt as _};

        let (server, cli) = reloading_server();
        let (server_io, client_io) = tokio::io::duplex(64 * 1024);
        let running = server.clone();
        tokio::spawn(async move {
            if let Ok(service) = running.serve(server_io).await {
                let _ = service.waiting().await;
            }
        });
        let client = ()
            .serve_with_lifecycle(
                client_io,
                ClientLifecycleMode::Discover {
                    preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                },
            )
            .await
            .expect("client connects");

        let wanted = SubscriptionFilter::builder().tools_list_changed().build();
        let mut subscription = client.listen(wanted.clone()).await.expect("listen");
        assert_eq!(subscription.acknowledged(), &wanted);

        server.reload(&spec_from(TWO_GETS), &cli).expect("reload");
        let notification = tokio::time::timeout(NOTIFICATION_TIMEOUT, subscription.next())
            .await
            .expect("a notification arrives")
            .expect("the subscription is healthy");
        assert!(
            matches!(
                notification,
                Some(ServerNotification::ToolListChangedNotification(_))
            ),
            "{notification:?}"
        );
    }

    #[test]
    fn tools_are_annotated_unless_turned_off() {
        let listed = |args: &[&str]| {
            let cli = Cli::try_parse_from(["oas2mcp"].iter().chain(args)).expect("CLI parses");
            let server =
                OpenApiServer::from_spec(&spec_from(ONE_GET), &cli, None, Metrics::disabled())
                    .expect("server builds");
            let state = server.state.load();
            advertised(&state.tools[0], server.annotate_tools)
        };

        let annotated = listed(&[]);
        let hints = annotated.annotations.clone().expect("annotated by default");
        assert_eq!(hints.read_only_hint, Some(true));
        let wire = serde_json::to_value(&annotated).expect("serialises");
        assert_eq!(wire["annotations"]["readOnlyHint"], true);
        assert_eq!(wire["annotations"]["openWorldHint"], true);

        let bare = listed(&["--auto-tool-annotations=false"]);
        assert!(bare.annotations.is_none());
        let wire = serde_json::to_value(&bare).expect("serialises");
        assert!(wire.get("annotations").is_none());
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

    const FIVE_GETS: &str = r#"
openapi: 3.0.0
info: { title: T, version: "1" }
servers: [{ url: "https://api.example.com" }]
paths:
  /a: { get: { operationId: getA, responses: { "200": { description: ok } } } }
  /b: { get: { operationId: getPublicB, responses: { "200": { description: ok } } } }
  /c: { get: { operationId: getC, responses: { "200": { description: ok } } } }
  /d: { get: { operationId: getPublicD, responses: { "200": { description: ok } } } }
  /e: { get: { operationId: getPublicE, responses: { "200": { description: ok } } } }
"#;

    fn paginated_server(page_size: &str, authorizer: Option<Authorizer>) -> OpenApiServer {
        let cli =
            Cli::try_parse_from(["oas2mcp", "--tools-page-size", page_size]).expect("CLI parses");
        OpenApiServer::from_spec(
            &spec_from(FIVE_GETS),
            &cli,
            authorizer.map(Arc::new),
            Metrics::disabled(),
        )
        .expect("server builds")
    }

    /// Walk every page, returning the tool names and the number of pages.
    fn list_all(server: &OpenApiServer, access: &Access) -> (Vec<String>, usize) {
        let mut names = Vec::new();
        let mut cursor: Option<String> = None;
        let mut pages = 0;
        loop {
            let page = server
                .list_page(access, cursor.as_deref())
                .expect("valid cursor");
            names.extend(page.tools.iter().map(|tool| tool.name.to_string()));
            pages += 1;
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => return (names, pages),
            }
        }
    }

    #[test]
    fn pages_concatenate_to_the_full_tool_list() {
        let server = paginated_server("2", None);
        let (names, pages) = list_all(&server, &Access::Unrestricted);
        assert_eq!(
            names,
            ["getA", "getPublicB", "getC", "getPublicD", "getPublicE"]
        );
        assert_eq!(pages, 3);
    }

    #[test]
    fn without_a_page_size_every_tool_comes_in_one_reply() {
        let cli = Cli::try_parse_from(["oas2mcp"]).expect("CLI parses");
        let server =
            OpenApiServer::from_spec(&spec_from(FIVE_GETS), &cli, None, Metrics::disabled())
                .expect("server builds");
        let page = server
            .list_page(&Access::Unrestricted, None)
            .expect("lists");
        assert_eq!(page.tools.len(), 5);
        assert_eq!(page.next_cursor, None);
    }

    #[test]
    fn pagination_runs_over_the_callers_own_list() {
        let authorizer = crate::auth::tests::test_authorizer_with_public_tools(&["^getPublic"]);
        let server = paginated_server("2", Some(authorizer));

        let (names, pages) = list_all(&server, &Access::Anonymous);
        assert_eq!(names, ["getPublicB", "getPublicD", "getPublicE"]);
        assert_eq!(pages, 2);

        let admin = Access::Authenticated(HashSet::from(["admin".to_string()]));
        assert_eq!(list_all(&server, &admin).0.len(), 5);

        // A cursor cut from one caller's list does not walk another's.
        let first = server
            .list_page(&admin, None)
            .expect("lists")
            .next_cursor
            .expect("more pages");
        let err = server
            .list_page(&Access::Anonymous, Some(&first))
            .expect_err("another list");
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn a_garbage_cursor_is_an_invalid_params_error() {
        let server = paginated_server("2", None);
        for cursor in ["", "garbage", "0000000000000000.2"] {
            let err = server
                .list_page(&Access::Unrestricted, Some(cursor))
                .expect_err("not a cursor we issued");
            assert_eq!(
                err.code,
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "{cursor:?}"
            );
        }
    }

    #[test]
    fn a_reload_that_changes_the_tools_turns_a_cursor_stale() {
        let server = paginated_server("2", None);
        let cli = Cli::try_parse_from(["oas2mcp", "--tools-page-size", "2"]).expect("CLI parses");
        let next = server
            .list_page(&Access::Unrestricted, None)
            .expect("lists")
            .next_cursor
            .expect("more pages");

        // Reloading the same document keeps the cursor good.
        server
            .reload(&spec_from(FIVE_GETS), &cli)
            .expect("reload succeeds");
        server
            .list_page(&Access::Unrestricted, Some(&next))
            .expect("same tool set, same cursor");

        server
            .reload(&spec_from(&FIVE_GETS.replace("getC", "getZ")), &cli)
            .expect("reload succeeds");
        let err = server
            .list_page(&Access::Unrestricted, Some(&next))
            .expect_err("the tool set changed");
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(err.message.contains("stale"), "{}", err.message);
    }

    const THREE_OPS: &str = r#"
openapi: 3.0.0
info: { title: T, version: "1" }
servers: [{ url: "https://api.example.com" }]
paths:
  /a:
    get: { operationId: getA, responses: { "200": { description: ok } } }
    delete: { operationId: deleteA, responses: { "204": { description: gone } } }
  /internal: { get: { operationId: getInternal, responses: { "200": { description: ok } } } }
"#;

    /// The operations of the document a caller with `access` reads.
    fn document_operations(server: &OpenApiServer, access: &Access) -> Vec<String> {
        let result = server
            .read_openapi(OPENAPI_URI, access)
            .expect("the document reads");
        let [
            ResourceContents::TextResourceContents {
                text, mime_type, ..
            },
        ] = result.contents.as_slice()
        else {
            panic!("one text content expected, got {:?}", result.contents);
        };
        assert_eq!(mime_type.as_deref(), Some("application/json"));
        let document: Value = serde_json::from_str(text).expect("the document is JSON");
        let mut operations: Vec<String> = document["paths"]
            .as_object()
            .expect("paths is an object")
            .values()
            .flat_map(|item| item.as_object().expect("path item").values())
            .filter_map(|operation| operation.get("operationId")?.as_str().map(String::from))
            .collect();
        operations.sort();
        operations
    }

    fn resource_server(args: &[&str], authorizer: Option<Authorizer>) -> OpenApiServer {
        let cli = Cli::try_parse_from(["oas2mcp"].into_iter().chain(args.iter().copied()))
            .expect("CLI parses");
        OpenApiServer::from_spec(
            &spec_from(THREE_OPS),
            &cli,
            authorizer.map(Arc::new),
            Metrics::disabled(),
        )
        .expect("server builds")
    }

    #[test]
    fn the_openapi_resource_is_off_by_default() {
        let server = resource_server(&[], None);

        assert!(server.get_info().capabilities.resources.is_none());
        assert!(server.openapi_resources().resources.is_empty());
        let err = server
            .read_openapi(OPENAPI_URI, &Access::Unrestricted)
            .expect_err("nothing to read");
        assert_eq!(err.code, rmcp::model::ErrorCode::METHOD_NOT_FOUND);
    }

    #[test]
    fn the_openapi_resource_is_listed_and_advertised_when_on() {
        let server = resource_server(&["--openapi-resource"], None);

        assert!(server.get_info().capabilities.resources.is_some());
        let resources = server.openapi_resources().resources;
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].uri, OPENAPI_URI);
        assert_eq!(resources[0].mime_type.as_deref(), Some("application/json"));
    }

    #[test]
    fn the_document_leaves_out_what_the_filters_drop() {
        let server = resource_server(&["--openapi-resource", "--exclude-regex", "Internal"], None);

        assert_eq!(
            document_operations(&server, &Access::Unrestricted),
            ["deleteA", "getA"]
        );
    }

    #[test]
    fn each_caller_reads_the_operations_it_can_list() {
        let authorizer = crate::auth::tests::test_authorizer_with_public_tools(&["^getA$"]);
        let server = resource_server(&["--openapi-resource"], Some(authorizer));

        assert_eq!(document_operations(&server, &Access::Anonymous), ["getA"]);
        let admin = Access::Authenticated(HashSet::from(["admin".to_string()]));
        assert_eq!(
            document_operations(&server, &admin),
            ["deleteA", "getA", "getInternal"]
        );
        let nobody = Access::Authenticated(HashSet::from(["nobody".to_string()]));
        assert_eq!(document_operations(&server, &nobody), ["getA"]);
    }

    #[test]
    fn anonymous_discovery_shows_every_operation_in_the_document() {
        let authorizer = crate::auth::tests::test_authorizer_with_anonymous_discovery();
        let server = resource_server(&["--openapi-resource"], Some(authorizer));

        assert_eq!(
            document_operations(&server, &Access::Anonymous),
            ["deleteA", "getA", "getInternal"]
        );
    }

    #[test]
    fn the_document_follows_a_reload() {
        let cli = Cli::try_parse_from(["oas2mcp", "--openapi-resource"]).expect("CLI parses");
        let server = OpenApiServer::from_spec(&spec_from(ONE_GET), &cli, None, Metrics::disabled())
            .expect("server builds");
        assert_eq!(
            document_operations(&server, &Access::Unrestricted),
            ["getA"]
        );

        server
            .reload(&spec_from(THREE_OPS), &cli)
            .expect("reload succeeds");
        assert_eq!(
            document_operations(&server, &Access::Unrestricted),
            ["deleteA", "getA", "getInternal"]
        );
    }

    #[test]
    fn an_unknown_resource_uri_is_not_found() {
        let server = resource_server(&["--openapi-resource"], None);

        let err = server
            .read_openapi("openapi://other", &Access::Unrestricted)
            .expect_err("no such resource");
        assert_eq!(err.code, rmcp::model::ErrorCode::RESOURCE_NOT_FOUND);
        assert_eq!(
            err.data,
            Some(serde_json::json!({ "uri": "openapi://other" }))
        );
    }

    /// Build the request one tool call would send, and hand back its headers.
    ///
    /// Goes through the real `build_request`, so the test sees exactly what
    /// the upstream would receive.
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
    fn a_non_object_json_body_is_wrapped_into_an_object() {
        for (body, value) in [
            ("[1,2,3]", serde_json::json!([1, 2, 3])),
            ("42", serde_json::json!(42)),
            (r#""ok""#, serde_json::json!("ok")),
            ("true", serde_json::json!(true)),
            ("null", Value::Null),
        ] {
            let result = shape_response(reqwest::StatusCode::OK, body);
            assert_eq!(
                result.structured_content,
                Some(serde_json::json!({ "result": value })),
                "body: {body:?}"
            );
            assert_eq!(text_of(&result), format!("HTTP 200 OK\n\n{body}"));
        }
    }

    #[tokio::test]
    async fn cancelling_a_call_aborts_the_upstream_request_promptly() {
        use tokio::io::AsyncReadExt as _;

        // An upstream that accepts the request and never answers, and reports
        // when the connection is closed.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a local port");
        let addr = listener.local_addr().expect("local address");
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept the upstream call");
            let _ = accepted_tx.send(());
            let mut buf = [0u8; 1024];
            while socket.read(&mut buf).await.is_ok_and(|n| n > 0) {}
            let _ = closed_tx.send(());
        });

        let cli = Cli::try_parse_from(["oas2mcp"]).expect("minimal CLI parses");
        let spec = spec_from(&format!(
            r#"
openapi: 3.0.0
info: {{ title: T, version: "1" }}
servers: [{{ url: "http://{addr}" }}]
paths:
  /a: {{ get: {{ operationId: getA, responses: {{ "200": {{ description: ok }} }} }} }}
"#
        ));
        let server = OpenApiServer::from_spec(&spec, &cli, None, Metrics::disabled())
            .expect("server builds");
        let state = server.state.load_full();
        let caller = Caller {
            access: Access::Unrestricted,
            traced_claims: Map::new(),
            identity: None,
        };

        let cancelled = async {
            accepted_rx.await.expect("the upstream saw the request");
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            server.call_until_cancelled(
                &state.tools[0],
                &state.base_url,
                &Map::new(),
                &HeaderMap::new(),
                &caller,
                cancelled,
            ),
        )
        .await
        .expect("the call returns as soon as it is cancelled");
        assert!(result.is_none());

        tokio::time::timeout(std::time::Duration::from_secs(5), closed_rx)
            .await
            .expect("the upstream connection is closed")
            .expect("the upstream task reports the close");
    }

    #[tokio::test]
    async fn a_call_that_is_not_cancelled_returns_its_result() {
        let cli = Cli::try_parse_from(["oas2mcp"]).expect("minimal CLI parses");
        // The request cannot be built: a missing path parameter fails before
        // any network access.
        let spec = spec_from(
            r#"
openapi: 3.0.0
info: { title: T, version: "1" }
servers: [{ url: "https://api.example.com" }]
paths:
  /a/{id}:
    get:
      operationId: getA
      parameters: [{ name: id, in: path, required: true, schema: { type: string } }]
      responses: { "200": { description: ok } }
"#,
        );
        let server = OpenApiServer::from_spec(&spec, &cli, None, Metrics::disabled())
            .expect("server builds");
        let state = server.state.load_full();
        let caller = Caller {
            access: Access::Unrestricted,
            traced_claims: Map::new(),
            identity: None,
        };

        let result = server
            .call_until_cancelled(
                &state.tools[0],
                &state.base_url,
                &Map::new(),
                &HeaderMap::new(),
                &caller,
                std::future::pending(),
            )
            .await
            .expect("an uncancelled call completes");
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn the_media_type_decides_how_a_body_is_read() {
        for (mime, kind) in [
            ("text/plain", BodyKind::Text),
            ("text/html", BodyKind::Text),
            ("application/json", BodyKind::Text),
            ("application/problem+json", BodyKind::Text),
            ("application/xml", BodyKind::Text),
            ("application/atom+xml", BodyKind::Text),
            ("application/x-www-form-urlencoded", BodyKind::Text),
            ("application/yaml", BodyKind::Text),
            ("image/svg+xml", BodyKind::Text),
            ("image/png", BodyKind::Image),
            ("image/jpeg", BodyKind::Image),
            ("audio/mpeg", BodyKind::Audio),
            ("application/octet-stream", BodyKind::Blob),
            ("application/pdf", BodyKind::Blob),
            ("application/zip", BodyKind::Blob),
            ("video/mp4", BodyKind::Blob),
        ] {
            assert_eq!(body_kind(Some(mime)), kind, "{mime}");
        }
        assert_eq!(body_kind(None), BodyKind::Unknown);
        assert_eq!(body_kind(Some("")), BodyKind::Unknown);
    }

    #[test]
    fn the_media_type_is_read_without_its_parameters_or_case() {
        assert_eq!(
            mime_essence("Application/JSON; charset=utf-8"),
            "application/json"
        );
        assert_eq!(mime_essence("image/png"), "image/png");
    }

    /// A PNG signature followed by bytes that are not valid UTF-8.
    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0xff];

    #[test]
    fn an_image_body_becomes_an_image_block_after_a_summary() {
        let result = shape_bytes_response(
            reqwest::StatusCode::OK,
            BodyKind::Image,
            Some("image/png"),
            PNG,
            "https://api.example.com/a.png",
        );

        assert_eq!(text_of(&result), "HTTP 200 OK, image/png, 10 bytes");
        let image = result.content[1].as_image().expect("an image block");
        assert_eq!(image.mime_type, "image/png");
        assert_eq!(STANDARD.decode(&image.data).expect("base64"), PNG);
        assert_eq!(result.structured_content, None);
        assert_eq!(result.is_error, Some(false));
    }

    #[test]
    fn an_audio_body_becomes_an_audio_block() {
        let result = shape_bytes_response(
            reqwest::StatusCode::OK,
            BodyKind::Audio,
            Some("audio/mpeg"),
            PNG,
            "https://api.example.com/a.mp3",
        );

        assert_eq!(text_of(&result), "HTTP 200 OK, audio/mpeg, 10 bytes");
        let audio = result.content[1].as_audio().expect("an audio block");
        assert_eq!(audio.mime_type, "audio/mpeg");
        assert_eq!(STANDARD.decode(&audio.data).expect("base64"), PNG);
    }

    /// The `(uri, mimeType, bytes)` of a result's embedded blob resource.
    fn blob_of(result: &CallToolResult) -> (String, Option<String>, Vec<u8>) {
        let resource = result.content[1].as_resource().expect("a resource block");
        let ResourceContents::BlobResourceContents {
            uri,
            mime_type,
            blob,
            ..
        } = &resource.resource
        else {
            panic!("expected a blob resource, got {resource:?}");
        };
        let bytes = STANDARD.decode(blob).expect("base64");
        (uri.clone(), mime_type.clone(), bytes)
    }

    #[test]
    fn another_binary_body_becomes_an_embedded_blob_named_by_the_request_url() {
        let result = shape_bytes_response(
            reqwest::StatusCode::OK,
            BodyKind::Blob,
            Some("application/pdf"),
            b"%PDF-1.7\n\xe2\xe3",
            "https://api.example.com/report.pdf",
        );

        assert_eq!(text_of(&result), "HTTP 200 OK, application/pdf, 11 bytes");
        assert_eq!(
            blob_of(&result),
            (
                "https://api.example.com/report.pdf".to_string(),
                Some("application/pdf".to_string()),
                b"%PDF-1.7\n\xe2\xe3".to_vec()
            )
        );
    }

    #[test]
    fn a_binary_error_body_keeps_is_error() {
        let result = shape_bytes_response(
            reqwest::StatusCode::BAD_GATEWAY,
            BodyKind::Blob,
            Some("application/octet-stream"),
            PNG,
            "https://api.example.com/a",
        );
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            text_of(&result),
            "HTTP 502 Bad Gateway, application/octet-stream, 10 bytes"
        );
    }

    #[test]
    fn an_untyped_body_is_text_when_it_is_utf8() {
        let result = shape_bytes_response(
            reqwest::StatusCode::OK,
            BodyKind::Unknown,
            None,
            br#"{"ok":true}"#,
            "https://api.example.com/a",
        );
        assert_eq!(text_of(&result), "HTTP 200 OK\n\n{\"ok\":true}");
        assert_eq!(result.content.len(), 1);
        assert_eq!(
            result.structured_content,
            Some(serde_json::json!({"ok": true}))
        );
    }

    #[test]
    fn an_untyped_body_that_is_not_utf8_is_an_octet_stream_blob() {
        let result = shape_bytes_response(
            reqwest::StatusCode::OK,
            BodyKind::Unknown,
            None,
            PNG,
            "https://api.example.com/a",
        );
        assert_eq!(
            text_of(&result),
            "HTTP 200 OK, application/octet-stream, 10 bytes"
        );
        assert_eq!(
            blob_of(&result).1.as_deref(),
            Some("application/octet-stream")
        );
    }

    #[test]
    fn an_empty_binary_body_is_just_the_status() {
        let result = shape_bytes_response(
            reqwest::StatusCode::NO_CONTENT,
            BodyKind::Image,
            Some("image/png"),
            b"",
            "https://api.example.com/a",
        );
        assert_eq!(text_of(&result), "HTTP 204 No Content\n\n");
        assert_eq!(result.content.len(), 1);
    }

    #[test]
    fn a_tool_with_an_output_schema_drops_the_structured_error_body() {
        let body = r#"{"error":"not found"}"#;
        let result = fit_output_schema(shape_response(reqwest::StatusCode::NOT_FOUND, body), true);
        assert_eq!(result.is_error, Some(true));
        assert_eq!(result.structured_content, None);
        assert_eq!(text_of(&result), format!("HTTP 404 Not Found\n\n{body}"));

        // A success keeps it: that is what the schema describes.
        let result =
            fit_output_schema(shape_response(reqwest::StatusCode::OK, r#"{"id":1}"#), true);
        assert_eq!(
            result.structured_content,
            Some(serde_json::json!({"id": 1}))
        );
    }

    const WITH_OUTPUT: &str = r#"
openapi: 3.0.0
info: { title: T, version: "1" }
servers: [{ url: "https://api.example.com" }]
paths:
  /a:
    get:
      operationId: getA
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema: { type: object, properties: { id: { type: integer } } }
"#;

    fn output_schema_of(args: &[&str]) -> Option<Arc<Map<String, Value>>> {
        let cli = Cli::try_parse_from(std::iter::once("oas2mcp").chain(args.iter().copied()))
            .expect("CLI parses");
        let server =
            OpenApiServer::from_spec(&spec_from(WITH_OUTPUT), &cli, None, Metrics::disabled())
                .expect("server builds");
        server.state.load().tools[0].output_schema.clone()
    }

    #[test]
    fn output_schemas_are_declared_only_with_the_flag() {
        assert!(output_schema_of(&[]).is_none());
        let schema = output_schema_of(&["--tool-output-schema"]).expect("the flag declares one");
        assert_eq!(schema["properties"]["id"]["type"], "integer");
    }
}
