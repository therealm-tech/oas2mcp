//! Legacy HTTP+SSE transport.
//!
//! This transport is deprecated by the MCP specification (superseded by
//! Streamable HTTP) and is no longer provided by `rmcp`, but it is kept here
//! for compatibility with older clients. We drive an `rmcp` server over a
//! channel-backed [`SinkStreamTransport`]:
//!
//! - `GET /sse` opens the event stream; the first event (`endpoint`) tells the
//!   client where to POST messages, then server→client JSON-RPC messages are
//!   streamed as `message` events.
//! - `POST /messages?sessionId=...` carries one client→server JSON-RPC message.

use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context as _;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::routing::{get, post};
use futures::channel::mpsc;
use futures::{SinkExt as _, StreamExt as _};
use rmcp::ServiceExt as _;
use rmcp::service::{RoleServer, RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::sink_stream::SinkStreamTransport;
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::server::OpenApiServer;
use crate::transport::shutdown_signal;

type Incoming = RxJsonRpcMessage<RoleServer>;
type Outgoing = TxJsonRpcMessage<RoleServer>;

#[derive(Clone)]
struct SseState {
    server: OpenApiServer,
    sessions: Arc<Mutex<HashMap<String, mpsc::Sender<Incoming>>>>,
    next_id: Arc<AtomicU64>,
    streams: CancellationToken,
}

pub async fn serve(bind: SocketAddr, server: OpenApiServer) -> anyhow::Result<()> {
    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!(%bind, "legacy SSE MCP endpoints listening at GET /sse and POST /messages");
    serve_until(listener, server, shutdown_signal()).await
}

async fn serve_until(
    listener: TcpListener,
    server: OpenApiServer,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    // A graceful shutdown waits for every response to end, and an event stream
    // only ends when its client leaves: closing them is what lets it complete.
    let streams = CancellationToken::new();
    let state = SseState {
        server,
        sessions: Arc::new(Mutex::new(HashMap::new())),
        next_id: Arc::new(AtomicU64::new(1)),
        streams: streams.clone(),
    };

    let app = axum::Router::new()
        .route("/sse", get(open_stream))
        .route("/messages", post(post_message))
        .layer(axum::middleware::from_fn(
            crate::transport::access_log::log_requests,
        ))
        .with_state(state);

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.await;
            streams.cancel();
        })
        .await
        .context("SSE server failed")?;
    Ok(())
}

/// Opens the SSE stream for one session and spawns an MCP server bound to it.
async fn open_stream(
    State(state): State<SseState>,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let session_id = format!("{:016x}", state.next_id.fetch_add(1, Ordering::Relaxed));

    // Channels bridging the HTTP layer and the MCP service:
    //   client --POST--> in_tx ===> in_rx --(stream)--> service
    //   service --(sink)--> out_tx ===> out_rx --SSE--> client
    let (in_tx, in_rx) = mpsc::channel::<Incoming>(64);
    let (out_tx, out_rx) = mpsc::channel::<Outgoing>(64);

    state
        .sessions
        .lock()
        .expect("sessions mutex")
        .insert(session_id.clone(), in_tx);

    let transport = SinkStreamTransport::new(out_tx, in_rx);
    let server = state.server.clone();
    tokio::spawn(async move {
        match server.serve(transport).await {
            Ok(running) => {
                let _ = running.waiting().await;
            }
            Err(error) => tracing::warn!(%error, "SSE session failed to start"),
        }
    });

    tracing::debug!(session = %session_id, "SSE session opened");

    // Removes the session when the client disconnects (the stream is dropped),
    // which closes `in_tx` and lets the spawned service terminate.
    let guard = SessionGuard {
        id: session_id.clone(),
        sessions: state.sessions.clone(),
    };

    let endpoint = format!("/messages?sessionId={session_id}");
    let init =
        futures::stream::once(async move { Ok(Event::default().event("endpoint").data(endpoint)) });
    let messages = futures::stream::unfold(
        (out_rx, guard),
        |(mut rx, guard): (mpsc::Receiver<Outgoing>, SessionGuard)| async move {
            let message = rx.next().await?;
            let data = serde_json::to_string(&message).unwrap_or_default();
            Some((
                Ok(Event::default().event("message").data(data)),
                (rx, guard),
            ))
        },
    );

    Sse::new(
        init.chain(messages)
            .take_until(state.streams.clone().cancelled_owned()),
    )
}

#[derive(Deserialize)]
struct MessageQuery {
    #[serde(rename = "sessionId")]
    session_id: String,
}

/// Delivers one client→server JSON-RPC message to its session.
async fn post_message(
    State(state): State<SseState>,
    Query(query): Query<MessageQuery>,
    Json(message): Json<Incoming>,
) -> StatusCode {
    let sender = state
        .sessions
        .lock()
        .expect("sessions mutex")
        .get(&query.session_id)
        .cloned();

    match sender {
        Some(mut sender) => match sender.send(message).await {
            Ok(()) => StatusCode::ACCEPTED,
            Err(_) => StatusCode::GONE,
        },
        None => StatusCode::NOT_FOUND,
    }
}

/// Removes a session from the registry when its SSE stream is dropped.
struct SessionGuard {
    id: String,
    sessions: Arc<Mutex<HashMap<String, mpsc::Sender<Incoming>>>>,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.sessions
            .lock()
            .expect("sessions mutex")
            .remove(&self.id);
        tracing::debug!(session = %self.id, "SSE session closed");
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use clap::Parser as _;
    use tokio::sync::oneshot;

    use super::*;
    use crate::cli::Cli;
    use crate::openapi::Spec;
    use crate::telemetry::Metrics;

    fn server() -> OpenApiServer {
        let raw = serde_yaml_ng::from_str(
            r#"
openapi: 3.0.0
info: { title: T, version: "1" }
servers: [{ url: "https://api.example.com" }]
paths:
  /a: { get: { operationId: getA, responses: { "200": { description: ok } } } }
"#,
        )
        .expect("valid YAML");
        let spec = Spec::from_value(raw).expect("supported document");
        let cli = Cli::try_parse_from(["oas2mcp"]).expect("minimal CLI parses");
        OpenApiServer::from_spec(&spec, &cli, None, Metrics::disabled()).expect("server builds")
    }

    #[tokio::test]
    async fn shutdown_closes_the_open_streams() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local address");
        let (stop, stopped) = oneshot::channel::<()>();
        let running = tokio::spawn(serve_until(listener, server(), async {
            let _ = stopped.await;
        }));

        let mut stream = reqwest::get(format!("http://{addr}/sse"))
            .await
            .expect("stream opens");
        let endpoint = stream
            .chunk()
            .await
            .expect("first event")
            .expect("not closed");
        assert!(String::from_utf8_lossy(&endpoint).contains("event: endpoint"));

        stop.send(()).expect("server still running");
        tokio::time::timeout(Duration::from_secs(5), running)
            .await
            .expect("the server stops while a client is still connected")
            .expect("server task")
            .expect("server exits cleanly");
        assert!(
            matches!(stream.chunk().await, Ok(None)),
            "the stream is closed"
        );
    }
}
