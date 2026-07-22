//! Bridges an ancillary's rmux agent pane to a browser terminal.
//!
//! `Binary` frames carry raw pane bytes both ways; `Text` frames carry JSON control messages
//! ([`WsRequest`] / [`WsResponse`]). A connecting client gets the pane's output so far, then live
//! output, with no gap or overlap between the two.

use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::AppState;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WsRequest {
    /// Keystrokes, forwarded to the pane verbatim.
    Data { data: String },
    Resize { cols: u16, rows: u16 },
    /// Ctrl-C, distinct from `Data` so the UI can offer a button for it.
    Interrupt,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WsResponse {
    /// Pane liveness, sent on connect and when it changes.
    Status { status: String, session: String },
    Error { message: String },
}

/// ETX.
const INTERRUPT: &str = "\u{3}";

pub async fn handle_ancillary_ws(socket: WebSocket, state: AppState, ancillary_id: String) {
    let (mut sender, mut receiver) = socket.split();

    let Some(mirror) = state.panes.mirror(&ancillary_id).await else {
        send_json(
            &mut sender,
            &WsResponse::Error {
                message: format!("No agent pane for ancillary: {}", ancillary_id),
            },
        )
        .await;
        return;
    };

    let session = state
        .panes
        .session_of(&ancillary_id)
        .await
        .unwrap_or_default();

    info!("Client attached to {} (rmux session {})", ancillary_id, session);

    // Taken together, so nothing is missed or repeated.
    let (backfill, mut live) = mirror.attach().await;
    let mut ended = mirror.ended();

    send_json(
        &mut sender,
        &WsResponse::Status {
            status: if mirror.has_ended() { "ended" } else { "attached" }.to_string(),
            session: session.clone(),
        },
    )
    .await;

    if !backfill.is_empty() && sender.send(Message::Binary(backfill)).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            incoming = receiver.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if let Err(e) = handle_request(&state, &ancillary_id, &text).await {
                            warn!("{}: {}", ancillary_id, e);
                            send_json(&mut sender, &WsResponse::Error { message: e.to_string() }).await;
                        }
                    }
                    Some(Ok(Message::Binary(bytes))) => {
                        // For clients that send keystrokes as raw bytes.
                        let text = String::from_utf8_lossy(&bytes).into_owned();
                        if let Err(e) = state.panes.send_input(&ancillary_id, &text).await {
                            warn!("{}: failed to forward input: {}", ancillary_id, e);
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        warn!("WebSocket error for {}: {}", ancillary_id, e);
                        break;
                    }
                    None => break,
                }
            }
            chunk = live.recv() => {
                match chunk {
                    Ok(bytes) => {
                        if sender.send(Message::Binary(bytes.to_vec())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        // Say so rather than render a stream with a hole in it.
                        warn!("{}: client lagged, {} chunks skipped", ancillary_id, skipped);
                        send_json(
                            &mut sender,
                            &WsResponse::Error {
                                message: format!("Dropped {} output chunks — reload to resync", skipped),
                            },
                        )
                        .await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            // The pane died or was replaced; the client would otherwise wait forever.
            Ok(()) = ended.changed() => {
                if !*ended.borrow() {
                    continue;
                }
                send_json(
                    &mut sender,
                    &WsResponse::Status {
                        status: "ended".to_string(),
                        session: session.clone(),
                    },
                )
                .await;
                break;
            }
        }
    }

    info!("Client detached from {}", ancillary_id);
}

async fn handle_request(state: &AppState, ancillary_id: &str, text: &str) -> anyhow::Result<()> {
    match serde_json::from_str::<WsRequest>(text)? {
        WsRequest::Data { data } => state.panes.send_input(ancillary_id, &data).await,
        WsRequest::Interrupt => state.panes.send_input(ancillary_id, INTERRUPT).await,
        WsRequest::Resize { cols, rows } => state.panes.resize(ancillary_id, cols, rows).await,
    }
}

async fn send_json<S>(sender: &mut S, response: &WsResponse)
where
    S: SinkExt<Message> + Unpin,
{
    if let Ok(json) = serde_json::to_string(response) {
        let _ = sender.send(Message::Text(json)).await;
    }
}
