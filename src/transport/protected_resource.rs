//! The `/mcp` endpoint as an OAuth 2.0 protected resource, per the MCP
//! authorization spec: a bearer challenge on unauthenticated requests, and the
//! Protected Resource Metadata (RFC 9728) the challenge points clients to.
//!
//! With public tools configured, a request without a token is let in and only
//! challenged when it calls a tool that is not public. The handler enforces
//! access either way; the challenge here is what prompts a client to log in.

use std::sync::Arc;

use anyhow::bail;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use axum::routing::get;
use http::header::WWW_AUTHENTICATE;
use http::{HeaderValue, StatusCode};
use serde_json::json;
use url::Url;

use super::MAX_REQUEST_BODY_BYTES;
use crate::auth::{Authorizer, bearer_token};

/// Whether the tool advertised under a name is public. The access rules match
/// operation names, which only the server can map a tool name back to.
pub type IsPublic = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// RFC 9728 §3's well-known URI suffix.
const WELL_KNOWN: &str = "/.well-known/oauth-protected-resource";

pub struct ProtectedResource {
    authorizer: Arc<Authorizer>,
    metadata: serde_json::Value,
    /// Absolute URL of the metadata, advertised in the challenge.
    metadata_url: String,
    /// Local paths the metadata is served on: RFC 9728's path-suffixed one, and
    /// the bare well-known path that clients fall back to.
    metadata_paths: Vec<String>,
}

impl ProtectedResource {
    /// `resource` is the canonical URL clients reach `/mcp` under; the
    /// authorization servers are the issuers the incoming JWTs are checked
    /// against, since those are the only ones whose tokens are accepted here.
    pub fn new(
        resource: &Url,
        authorization_servers: &[String],
        authorizer: Arc<Authorizer>,
    ) -> anyhow::Result<Self> {
        if !matches!(resource.scheme(), "http" | "https") {
            bail!("--inbound-resource `{resource}` is not an http(s) URL");
        }
        if resource.fragment().is_some() {
            bail!("--inbound-resource `{resource}` must not carry a fragment (RFC 9728 §1.2)");
        }
        if authorization_servers.is_empty() {
            bail!(
                "--inbound-resource needs at least one --inbound-expected-issuer: those issuers are \
                 the authorization servers clients are sent to for a token"
            );
        }

        let suffixed = match resource.path() {
            "/" => WELL_KNOWN.to_string(),
            path => format!("{WELL_KNOWN}{path}"),
        };
        let mut metadata_url = resource.clone();
        metadata_url.set_path(&suffixed);
        metadata_url.set_query(None);
        let mut metadata_paths = vec![suffixed];
        if metadata_paths[0] != WELL_KNOWN {
            metadata_paths.push(WELL_KNOWN.to_string());
        }

        Ok(Self {
            authorizer,
            metadata: json!({
                "resource": resource.as_str(),
                "authorization_servers": authorization_servers,
                "bearer_methods_supported": ["header"],
            }),
            metadata_url: metadata_url.into(),
            metadata_paths,
        })
    }

    /// Wrap `mcp` so every request to it needs a valid bearer token, and add
    /// the metadata routes beside it, which stay public.
    pub fn protect(self, mcp: Router, is_public: IsPublic) -> Router {
        let metadata = axum::Json(self.metadata.clone());
        let metadata_paths = self.metadata_paths.clone();
        let this = Arc::new(Gate {
            resource: self,
            is_public,
        });
        let mut router = mcp.layer(axum::middleware::from_fn_with_state(
            this.clone(),
            require_bearer,
        ));
        for path in &metadata_paths {
            let metadata = metadata.clone();
            router = router.route(path, get(move || async move { metadata }));
        }
        tracing::info!(
            metadata = this.resource.metadata_url,
            "serving OAuth protected resource metadata; unauthenticated MCP requests get a 401 challenge"
        );
        router
    }

    fn challenge(&self, invalid_token: bool, reason: String) -> Response {
        // RFC 6750 §3: no `error` when the request simply carried no credentials.
        let error = if invalid_token {
            "error=\"invalid_token\", "
        } else {
            ""
        };
        let value = format!("Bearer {error}resource_metadata=\"{}\"", self.metadata_url);
        let mut response = (StatusCode::UNAUTHORIZED, reason).into_response();
        if let Ok(value) = HeaderValue::from_str(&value) {
            response.headers_mut().insert(WWW_AUTHENTICATE, value);
        }
        response
    }
}

/// The middleware's state: the resource, and how to tell a public tool.
struct Gate {
    resource: ProtectedResource,
    is_public: IsPublic,
}

async fn require_bearer(State(gate): State<Arc<Gate>>, request: Request, next: Next) -> Response {
    let resource = &gate.resource;
    let Some(token) = bearer_token(request.headers()) else {
        if resource.authorizer.has_public_tools() {
            return admit_anonymous(&gate, request, next).await;
        }
        tracing::debug!("challenging an MCP request that carries no bearer token");
        return resource.challenge(false, "Unauthorized: missing bearer token".into());
    };
    if let Err(err) = resource.authorizer.verify(token) {
        tracing::warn!(error = %format!("{err:#}"), "challenging an MCP request: JWT verification failed");
        return resource.challenge(true, "Unauthorized: invalid bearer token".into());
    }
    next.run(request).await
}

/// Let an anonymous request through unless it calls a tool that is not public.
/// The body has to be read for that, and is handed on intact.
async fn admit_anonymous(gate: &Gate, request: Request, next: Next) -> Response {
    let (parts, body) = request.into_parts();
    let Ok(body) = axum::body::to_bytes(body, MAX_REQUEST_BODY_BYTES).await else {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("Payload Too Large: request body exceeds {MAX_REQUEST_BODY_BYTES} bytes"),
        )
            .into_response();
    };
    if let Some(tool) = called_tool(&body).filter(|tool| !(gate.is_public)(tool)) {
        tracing::debug!(
            tool,
            "challenging an anonymous call to a tool that is not public"
        );
        return gate.resource.challenge(
            false,
            format!("Unauthorized: tool `{tool}` needs a bearer token"),
        );
    }
    next.run(Request::from_parts(parts, Body::from(body))).await
}

/// The tool a JSON-RPC `tools/call` request names. `None` for any other message,
/// and for a body that is not JSON-RPC at all, which `rmcp` rejects on its own.
fn called_tool(body: &Bytes) -> Option<String> {
    let message: serde_json::Value = serde_json::from_slice(body).ok()?;
    if message.get("method")?.as_str()? != "tools/call" {
        return None;
    }
    message.pointer("/params/name")?.as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use http::header::AUTHORIZATION;
    use serde_json::Value;
    use tower::ServiceExt as _;

    use super::*;
    use crate::auth::tests::{
        TEST_KID, in_one_hour, sign, test_authorizer, test_authorizer_with_public_tools,
    };

    const ISSUER: &str = "https://idp.example.com/realms/main";

    fn resource(url: &str) -> anyhow::Result<ProtectedResource> {
        ProtectedResource::new(
            &Url::parse(url).expect("valid URL"),
            &[ISSUER.to_string()],
            Arc::new(test_authorizer()),
        )
    }

    fn app(url: &str) -> Router {
        let mcp = Router::new().route("/mcp", axum::routing::post(|| async { "reached" }));
        let authorizer = Arc::new(test_authorizer());
        resource(url)
            .expect("valid resource")
            .protect(mcp, Arc::new(move |tool: &str| authorizer.is_public(tool)))
    }

    /// An app with public tools, whose endpoint echoes the body it received.
    fn app_with_public_tools(patterns: &[&str]) -> Router {
        let mcp = Router::new().route("/mcp", axum::routing::post(|body: Bytes| async { body }));
        let authorizer = Arc::new(test_authorizer_with_public_tools(patterns));
        let is_public = authorizer.clone();
        ProtectedResource::new(
            &Url::parse("https://mcp.example.com/mcp").expect("valid URL"),
            &[ISSUER.to_string()],
            authorizer,
        )
        .expect("valid resource")
        .protect(mcp, Arc::new(move |tool: &str| is_public.is_public(tool)))
    }

    async fn send(app: &Router, method: &str, path: &str, token: Option<&str>) -> Response {
        send_body(app, method, path, token, Body::empty()).await
    }

    async fn send_body(
        app: &Router,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Body,
    ) -> Response {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            request = request.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        app.clone()
            .oneshot(request.body(body).expect("valid request"))
            .await
            .expect("infallible")
    }

    fn rpc(method: &str, params: Value) -> String {
        json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }).to_string()
    }

    async fn body_of(response: Response) -> Bytes {
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("readable body")
    }

    fn challenge_of(response: &Response) -> &str {
        response
            .headers()
            .get(WWW_AUTHENTICATE)
            .expect("a 401 carries a challenge")
            .to_str()
            .expect("ASCII header")
    }

    #[tokio::test]
    async fn a_request_without_a_token_is_challenged_towards_the_metadata() {
        let app = app("https://mcp.example.com/mcp");
        let response = send(&app, "POST", "/mcp", None).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            challenge_of(&response),
            "Bearer resource_metadata=\"https://mcp.example.com/.well-known/oauth-protected-resource/mcp\""
        );
    }

    #[tokio::test]
    async fn an_invalid_token_is_challenged_as_such() {
        let app = app("https://mcp.example.com/mcp");
        let response = send(&app, "POST", "/mcp", Some("not.a.jwt")).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(
            challenge_of(&response)
                .starts_with("Bearer error=\"invalid_token\", resource_metadata="),
            "{}",
            challenge_of(&response)
        );
    }

    #[tokio::test]
    async fn a_valid_token_reaches_the_endpoint_whatever_its_roles() {
        // Roles decide which tools are visible, not whether the caller is
        // authenticated: a verified token with no matching role is let through.
        let app = app("https://mcp.example.com/mcp");
        let token = sign(json!(["nobody"]), in_one_hour(), Some(TEST_KID));
        let response = send(&app, "POST", "/mcp", Some(&token)).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn the_metadata_is_public_on_both_well_known_paths() {
        let app = app("https://mcp.example.com/mcp");
        for path in [
            "/.well-known/oauth-protected-resource/mcp",
            "/.well-known/oauth-protected-resource",
        ] {
            let response = send(&app, "GET", path, None).await;
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            let body = body_of(response).await;
            let metadata: Value = serde_json::from_slice(&body).expect("JSON metadata");
            assert_eq!(
                metadata,
                json!({
                    "resource": "https://mcp.example.com/mcp",
                    "authorization_servers": [ISSUER],
                    "bearer_methods_supported": ["header"],
                })
            );
        }
    }

    #[tokio::test]
    async fn with_public_tools_an_anonymous_client_gets_in_and_its_body_intact() {
        let app = app_with_public_tools(&["^get_public"]);
        for message in [
            rpc("initialize", json!({})),
            rpc("tools/list", json!({})),
            rpc("tools/call", json!({ "name": "get_public_stats" })),
            "not json".to_string(),
        ] {
            let response = send_body(&app, "POST", "/mcp", None, Body::from(message.clone())).await;
            assert_eq!(response.status(), StatusCode::OK, "{message}");
            assert_eq!(body_of(response).await, message.as_bytes(), "{message}");
        }
    }

    #[tokio::test]
    async fn with_public_tools_an_anonymous_call_to_another_tool_is_challenged() {
        let app = app_with_public_tools(&["^get_public"]);
        let call = rpc("tools/call", json!({ "name": "delete_pet" }));
        let response = send_body(&app, "POST", "/mcp", None, Body::from(call)).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(
            challenge_of(&response).starts_with("Bearer resource_metadata="),
            "{}",
            challenge_of(&response)
        );
    }

    #[tokio::test]
    async fn with_public_tools_an_invalid_token_is_still_challenged() {
        // A bad token is not downgraded to anonymous: the client would otherwise
        // never learn that its token needs replacing.
        let app = app_with_public_tools(&["^get_public"]);
        let list = rpc("tools/list", json!({}));
        let response = send_body(&app, "POST", "/mcp", Some("not.a.jwt"), Body::from(list)).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(challenge_of(&response).contains("error=\"invalid_token\""));
    }

    #[tokio::test]
    async fn an_anonymous_body_over_the_limit_is_refused() {
        let app = app_with_public_tools(&["^get_public"]);
        let huge = Body::from(vec![b' '; MAX_REQUEST_BODY_BYTES + 1]);
        let response = send_body(&app, "POST", "/mcp", None, huge).await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[test]
    fn a_resource_at_the_root_has_a_single_metadata_path() {
        let resource = resource("https://mcp.example.com/").expect("valid resource");
        assert_eq!(resource.metadata_paths, [WELL_KNOWN]);
        assert_eq!(
            resource.metadata_url,
            "https://mcp.example.com/.well-known/oauth-protected-resource"
        );
    }

    #[test]
    fn a_misconfigured_resource_is_refused() {
        assert!(resource("https://mcp.example.com/mcp#frag").is_err());
        assert!(resource("ftp://mcp.example.com/mcp").is_err());
        let no_issuer = ProtectedResource::new(
            &Url::parse("https://mcp.example.com/mcp").expect("valid URL"),
            &[],
            Arc::new(test_authorizer()),
        );
        assert!(no_issuer.is_err());
    }
}
