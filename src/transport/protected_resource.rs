//! The `/mcp` endpoint as an OAuth 2.0 protected resource, per the MCP
//! authorization spec: a bearer challenge on unauthenticated requests, and the
//! Protected Resource Metadata (RFC 9728) the challenge points clients to.

use std::sync::Arc;

use anyhow::bail;
use axum::Router;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use axum::routing::get;
use http::header::WWW_AUTHENTICATE;
use http::{HeaderValue, StatusCode};
use serde_json::json;
use url::Url;

use crate::auth::{Authorizer, bearer_token};

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
            bail!("--oauth-resource `{resource}` is not an http(s) URL");
        }
        if resource.fragment().is_some() {
            bail!("--oauth-resource `{resource}` must not carry a fragment (RFC 9728 §1.2)");
        }
        if authorization_servers.is_empty() {
            bail!(
                "--oauth-resource needs at least one --oauth-expected-issuer: those issuers are \
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
    pub fn protect(self, mcp: Router) -> Router {
        let this = Arc::new(self);
        let metadata = axum::Json(this.metadata.clone());
        let mut router = mcp.layer(axum::middleware::from_fn_with_state(
            this.clone(),
            require_bearer,
        ));
        for path in &this.metadata_paths {
            let metadata = metadata.clone();
            router = router.route(path, get(move || async move { metadata }));
        }
        tracing::info!(
            metadata = this.metadata_url,
            "serving OAuth protected resource metadata; unauthenticated MCP requests get a 401 challenge"
        );
        router
    }

    fn challenge(&self, invalid_token: bool, reason: &'static str) -> Response {
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

async fn require_bearer(
    State(resource): State<Arc<ProtectedResource>>,
    request: Request,
    next: Next,
) -> Response {
    let Some(token) = bearer_token(request.headers()) else {
        tracing::debug!("challenging an MCP request that carries no bearer token");
        return resource.challenge(false, "Unauthorized: missing bearer token");
    };
    if let Err(err) = resource.authorizer.verify(token) {
        tracing::warn!(error = %format!("{err:#}"), "challenging an MCP request: JWT verification failed");
        return resource.challenge(true, "Unauthorized: invalid bearer token");
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use http::header::AUTHORIZATION;
    use serde_json::Value;
    use tower::ServiceExt as _;

    use super::*;
    use crate::auth::tests::{TEST_KID, in_one_hour, sign, test_authorizer};

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
        resource(url).expect("valid resource").protect(mcp)
    }

    async fn send(app: &Router, method: &str, path: &str, token: Option<&str>) -> Response {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            request = request.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        app.clone()
            .oneshot(request.body(Body::empty()).expect("valid request"))
            .await
            .expect("infallible")
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
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("readable body");
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
