//! Access logging for the HTTP transports.
//!
//! `rmcp` rejects a malformed request inside its own tower service, before any
//! handler here runs, and most of those rejections state their reason in the
//! response body and nowhere else — a missing `Mcp-Session-Id`, an unsupported
//! `MCP-Protocol-Version`, an `Accept` header without `text/event-stream`. A few
//! are logged by `rmcp` itself, most are not, so without this layer a client
//! stuck on `400 Bad Request` leaves no trace at all on the server side.
//!
//! Every response is logged: `debug` when it succeeded, `warn` for a `4xx`,
//! `error` for a `5xx`, with the reason body echoed on the failures.

use std::time::Instant;

use axum::body::Body;
use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use http::HeaderMap;
use http::header::{ACCEPT, CONTENT_TYPE, HOST};
use http_body::Body as _;

/// `rmcp`'s session header. Its value is a capability — whoever holds it can
/// speak into that session — so only its presence is ever logged.
const HEADER_SESSION_ID: &str = "mcp-session-id";

/// Protocol version negotiated by the client. A mismatch here is one of the
/// `400`s this layer exists to explain.
const HEADER_PROTOCOL_VERSION: &str = "mcp-protocol-version";

/// Cap on the error body echoed into the log line. The rejection reasons are a
/// single short sentence; anything bigger is not an explanation, and reading it
/// would mean buffering a response we have no business buffering.
const MAX_REASON_BYTES: usize = 1024;

/// Log one request/response pair, leaving the response otherwise untouched.
pub(crate) async fn log_requests(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let headers = request.headers();
    // Only the headers that decide whether `rmcp` accepts the request. The
    // `Authorization` header is deliberately absent: it carries a bearer token.
    let host = header(headers, HOST);
    let protocol_version = header(headers, HEADER_PROTOCOL_VERSION);
    let accept = header(headers, ACCEPT);
    let content_type = header(headers, CONTENT_TYPE);
    let session = headers.contains_key(HEADER_SESSION_ID);

    let started = Instant::now();
    let response = next.run(request).await;
    let elapsed_ms = started.elapsed().as_millis();
    let status = response.status();

    if status.is_success() || status.is_redirection() || status.is_informational() {
        tracing::debug!(%method, path, status = status.as_u16(), elapsed_ms, "served HTTP request");
        return response;
    }

    let (response, reason) = take_reason(response).await;
    let reason = reason.as_deref().unwrap_or("");
    if status.is_client_error() {
        tracing::warn!(
            %method,
            path,
            status = status.as_u16(),
            elapsed_ms,
            reason,
            ?host,
            ?protocol_version,
            ?accept,
            ?content_type,
            session,
            "rejected HTTP request"
        );
    } else {
        tracing::error!(
            %method,
            path,
            status = status.as_u16(),
            elapsed_ms,
            reason,
            "HTTP request failed"
        );
    }
    response
}

fn header(headers: &HeaderMap, name: impl http::header::AsHeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Read the reason text out of a rejected response, handing the response back
/// intact.
///
/// Only a body whose length is known in advance and small is read: it is
/// buffered whole and put back as-is, so a streamed body — an SSE flow, a large
/// proxied payload — is never consumed by the logger.
async fn take_reason(response: Response) -> (Response, Option<String>) {
    let bounded = response
        .body()
        .size_hint()
        .upper()
        .is_some_and(|len| len <= MAX_REASON_BYTES as u64);
    if !bounded {
        return (response, None);
    }

    let (parts, body) = response.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_REASON_BYTES).await else {
        // The body errored mid-read, so there is nothing left to hand back.
        return (Response::from_parts(parts, Body::empty()), None);
    };
    let reason = String::from_utf8_lossy(&bytes).trim().to_string();
    let response = Response::from_parts(parts, Body::from(bytes));
    (response, (!reason.is_empty()).then_some(reason))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;

    #[tokio::test]
    async fn a_short_error_body_is_read_and_put_back() {
        let response = Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Body::from("Bad Request: Session ID is required"))
            .expect("valid response");

        let (response, reason) = take_reason(response).await;
        assert_eq!(
            reason.as_deref(),
            Some("Bad Request: Session ID is required")
        );
        // The client still gets the explanation it was going to get.
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body is readable");
        assert_eq!(&body[..], b"Bad Request: Session ID is required");
    }

    #[tokio::test]
    async fn a_streamed_body_is_left_alone() {
        // An SSE response has no known length; buffering it would hold the whole
        // stream in memory and stall the client until the server hung up.
        let stream = futures::stream::pending::<Result<Vec<u8>, std::io::Error>>();
        let response = Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Body::from_stream(stream))
            .expect("valid response");

        let (response, reason) = take_reason(response).await;
        assert!(reason.is_none());
        assert!(response.body().size_hint().upper().is_none());
    }

    #[tokio::test]
    async fn an_oversized_body_is_left_alone() {
        let response = Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Body::from("x".repeat(MAX_REASON_BYTES + 1)))
            .expect("valid response");

        let (_, reason) = take_reason(response).await;
        assert!(reason.is_none());
    }

    #[tokio::test]
    async fn an_empty_body_yields_no_reason() {
        let response = Response::builder()
            .status(StatusCode::NOT_ACCEPTABLE)
            .body(Body::empty())
            .expect("valid response");

        let (_, reason) = take_reason(response).await;
        assert!(reason.is_none());
    }
}
