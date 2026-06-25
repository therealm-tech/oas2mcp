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

use crate::server::OpenApiServer;
use crate::transport::shutdown_signal;

type Incoming = RxJsonRpcMessage<RoleServer>;
type Outgoing = TxJsonRpcMessage<RoleServer>;

#[derive(Clone)]
struct SseState {
    server: OpenApiServer,
    sessions: Arc<Mutex<HashMap<String, mpsc::Sender<Incoming>>>>,
    next_id: Arc<AtomicU64>,
}

pub async fn serve(bind: SocketAddr, server: OpenApiServer) -> anyhow::Result<()> {
    let state = SseState {
        server,
        sessions: Arc::new(Mutex::new(HashMap::new())),
        next_id: Arc::new(AtomicU64::new(1)),
    };

    let app = axum::Router::new()
        .route("/sse", get(open_stream))
        .route("/messages", post(post_message))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!(%bind, "legacy SSE MCP endpoints listening at GET /sse and POST /messages");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
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

    Sse::new(init.chain(messages))
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
