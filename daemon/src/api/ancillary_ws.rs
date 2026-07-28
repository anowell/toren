//! Bridges an ancillary's rmux agent pane to a browser terminal.
//!
//! `Binary` frames carry raw pane bytes both ways; `Text` frames carry JSON control messages
//! ([`WsRequest`] / [`WsResponse`]). A connecting client gets the pane's output so far, then live
//! output, with no gap or overlap between the two.
//!
//! Every binary frame *out* opens with a big-endian `u32` epoch. A mirror re-seeds by painting the
//! whole screen, so bytes from before a paint are wrong rather than merely late; both ends drop
//! them by comparing epochs, which is what makes a resync a resync rather than a race. Frames
//! *in* are keystrokes and carry no header.
//!
//! The socket is also kept alive from here: a browser answers protocol pings itself, and sends its
//! own as JSON because the browser API will not send a protocol one. Without either, a connection
//! a proxy or a sleeping phone silently dropped looks exactly like an idle agent.

use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio::time::MissedTickBehavior;
use tracing::{info, warn};

use super::AppState;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WsRequest {
    /// Keystrokes, forwarded to the pane verbatim.
    Data {
        data: String,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    /// Ctrl-C, distinct from `Data` so the UI can offer a button for it.
    Interrupt,
    /// Repaint me: the client believes its terminal no longer matches the pane.
    Resync,
    /// Client-side keepalive. The browser cannot send a protocol ping, so it sends this.
    Ping,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WsResponse {
    /// Pane liveness, sent on connect and when it changes.
    Status {
        status: String,
        session: String,
    },
    Error {
        message: String,
    },
    Pong,
}

/// ETX.
const INTERRUPT: &str = "\u{3}";

/// How often an otherwise idle socket is pinged. A pane can go hours without a byte, so this is
/// the only thing that notices a client that went away without closing.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(20);

/// How long a socket may go without a word from its client before it is treated as gone. Several
/// pings' worth, so a slow phone is not hung up on.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(75);

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

    info!(
        "Client attached to {} (rmux session {})",
        ancillary_id, session
    );

    // Taken together, so nothing is missed or repeated. The backfill opens with a paint of the
    // pane's screen, so a client attaching long after the pane started still lands on it exactly.
    let (backfill, mut live) = mirror.attach().await;
    let mut epoch = backfill.epoch;
    let mut state_changes = mirror.state();

    send_json(
        &mut sender,
        &WsResponse::Status {
            status: if mirror.has_ended() {
                "ended"
            } else {
                "attached"
            }
            .to_string(),
            session: session.clone(),
        },
    )
    .await;

    if !backfill.bytes.is_empty()
        && sender
            .send(Message::Binary(frame(epoch, &backfill.bytes)))
            .await
            .is_err()
    {
        return;
    }

    let mut keepalive = tokio::time::interval(KEEPALIVE_INTERVAL);
    keepalive.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // The first tick is immediate, and pinging a socket that just opened proves nothing.
    keepalive.tick().await;
    let mut last_heard = Instant::now();

    loop {
        tokio::select! {
            incoming = receiver.next() => {
                last_heard = Instant::now();
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<WsRequest>(&text) {
                            Ok(WsRequest::Ping) => send_json(&mut sender, &WsResponse::Pong).await,
                            Ok(WsRequest::Resync) => {
                                epoch = resync(&state, &ancillary_id, epoch, &mut sender).await;
                            }
                            Ok(request) => {
                                if let Err(e) = apply(&state, &ancillary_id, request).await {
                                    warn!("{}: {}", ancillary_id, e);
                                    send_json(&mut sender, &WsResponse::Error { message: e.to_string() }).await;
                                }
                            }
                            Err(e) => {
                                warn!("{}: unparseable control message: {}", ancillary_id, e);
                                send_json(&mut sender, &WsResponse::Error { message: e.to_string() }).await;
                            }
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
                    Ok(sent) => {
                        // Bytes describing a screen this client has already been moved off.
                        if sent.epoch < epoch {
                            continue;
                        }
                        // Further behind than a paint costs: send it where the pane is, not
                        // everything that happened on the way there.
                        if mirror.bytes_behind(&sent) > toren_mirror::LAG_BUDGET_BYTES {
                            warn!(
                                "{}: client {} bytes behind, repainting",
                                ancillary_id,
                                mirror.bytes_behind(&sent)
                            );
                            epoch = resync(&state, &ancillary_id, epoch, &mut sender).await;
                            continue;
                        }
                        epoch = sent.epoch;
                        if sender.send(Message::Binary(frame(sent.epoch, &sent.bytes))).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        // The bytes that would fix the client's screen are the ones it just lost,
                        // so describe the screen instead of streaming into a corrupted one.
                        warn!("{}: client lagged, {} chunks skipped", ancillary_id, skipped);
                        epoch = resync(&state, &ancillary_id, epoch, &mut sender).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = keepalive.tick() => {
                if last_heard.elapsed() > CLIENT_TIMEOUT {
                    info!("{}: client silent for {:?}, closing", ancillary_id, last_heard.elapsed());
                    break;
                }
                if sender.send(Message::Ping(Vec::new())).await.is_err() {
                    break;
                }
            }
            // The pane died or was replaced; the client would otherwise wait forever.
            Ok(()) = state_changes.changed() => {
                if !state_changes.borrow().is_ended() {
                    continue;
                }
                // A pane is marked ended after the status line it ends with is queued, so both
                // arms are ready here and this one may be picked first. Take what is waiting, or
                // the client closes on a pane that never said how it went.
                while let Ok(sent) = live.try_recv() {
                    if sent.epoch < epoch {
                        continue;
                    }
                    epoch = sent.epoch;
                    if sender.send(Message::Binary(frame(sent.epoch, &sent.bytes))).await.is_err() {
                        break;
                    }
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

/// Pane bytes as the browser reads them: the epoch they belong to, then the bytes themselves.
fn frame(epoch: u32, bytes: &[u8]) -> Vec<u8> {
    let mut framed = Vec::with_capacity(bytes.len() + 4);
    framed.extend_from_slice(&epoch.to_be_bytes());
    framed.extend_from_slice(bytes);
    framed
}

/// Repaint the pane and adopt the epoch that paint opens, so everything already queued behind it
/// is dropped rather than drawn over it. The paint itself arrives on the live subscription like
/// any other frame.
async fn resync<S>(state: &AppState, key: &str, current: u32, sender: &mut S) -> u32
where
    S: SinkExt<Message> + Unpin,
{
    match state.panes.resync(key).await {
        Ok(epoch) => epoch,
        Err(e) => {
            warn!("{}: failed to repaint the pane: {:#}", key, e);
            send_json(
                sender,
                &WsResponse::Error {
                    message: format!("Failed to resync: {}", e),
                },
            )
            .await;
            current
        }
    }
}

async fn apply(state: &AppState, ancillary_id: &str, request: WsRequest) -> anyhow::Result<()> {
    match request {
        WsRequest::Data { data } => state.panes.send_input(ancillary_id, &data).await,
        WsRequest::Interrupt => state.panes.send_input(ancillary_id, INTERRUPT).await,
        WsRequest::Resize { cols, rows } => state.panes.resize(ancillary_id, cols, rows).await,
        // Answered by the caller, which owns the socket's epoch and its replies.
        WsRequest::Resync | WsRequest::Ping => Ok(()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_carries_its_epoch_ahead_of_its_bytes() {
        assert_eq!(frame(0, b"hi"), b"\x00\x00\x00\x00hi");
        assert_eq!(frame(258, b"hi"), b"\x00\x00\x01\x02hi");
    }

    #[test]
    fn an_empty_frame_is_still_addressed() {
        assert_eq!(frame(1, b""), b"\x00\x00\x00\x01");
    }
}
