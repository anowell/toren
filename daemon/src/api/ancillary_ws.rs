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
//! Three things this connection is, beyond a pipe:
//!
//! * **A viewer**, registered for as long as it is open. A mirror costs rmux a connection and an
//!   output subscription, so it lives while somebody is watching and is swept when nobody is.
//! * **A candidate for owning the pane's size**, which it becomes by typing. One PTY has one
//!   geometry; a viewer that does not own it is told so, and told what the geometry is, so it can
//!   scale rather than fight for it.
//! * **A terminal whose answers must not escape**. A browser's xterm.js replies to queries like
//!   any terminal, and those replies would arrive at a pane that asked once and was answered long
//!   ago. They are dropped on the way in.
//!
//! The socket is also kept alive from here: a browser answers protocol pings itself, and sends its
//! own as JSON because the browser API will not send a protocol one. Without either, a connection
//! a proxy or a sleeping phone silently dropped looks exactly like an idle agent.

use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio::time::MissedTickBehavior;
use toren_mirror::QueryFilter;
use tracing::{info, warn};

use super::AppState;
use crate::services::pane_runner::{Geometry, ViewerId};

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WsRequest {
    /// Keystrokes, forwarded to the pane verbatim.
    Data { data: String },
    /// This viewer's geometry. Honoured only while this viewer owns the pane's size.
    Resize { cols: u16, rows: u16 },
    /// Make this viewer the one that sizes the pane, and take this geometry.
    TakeSize { cols: u16, rows: u16 },
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
    /// Pane liveness, sent on connect and when it changes: `attached`, `degraded`, or `ended`.
    Status {
        status: String,
        session: String,
    },
    /// The pane's geometry, and whether this viewer is the one setting it.
    ///
    /// A viewer that is not sizing the pane needs both halves: what the grid actually is, so it
    /// can scale to fit rather than reflow, and that it is not the one deciding, so it can offer
    /// to become so.
    Size {
        cols: u16,
        rows: u16,
        owned: bool,
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

    // Registering as a viewer is what keeps the mirror alive; the mirror is swept once the last
    // viewer has been gone for a moment.
    let Some(viewer) = state.panes.attach_viewer(&ancillary_id).await else {
        send_json(
            &mut sender,
            &WsResponse::Error {
                message: format!("Pane for {} went away while attaching", ancillary_id),
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

    serve(
        &mut sender,
        &mut receiver,
        &state,
        &ancillary_id,
        viewer,
        &session,
        &mirror,
    )
    .await;

    state.panes.detach_viewer(&ancillary_id, viewer).await;
    info!("Client detached from {}", ancillary_id);
}

/// The socket's whole life, so the caller can release the viewer on every exit path.
async fn serve<S, R>(
    sender: &mut S,
    receiver: &mut R,
    state: &AppState,
    ancillary_id: &str,
    viewer: ViewerId,
    session: &str,
    mirror: &toren_mirror::PaneMirror,
) where
    S: SinkExt<Message> + Unpin,
    R: StreamExt<Item = Result<Message, axum::Error>> + Unpin,
{
    // Taken together, so nothing is missed or repeated. The backfill opens with a paint of the
    // pane's screen, so a client attaching long after the pane started still lands on it exactly.
    let (backfill, mut live) = mirror.attach().await;
    let mut epoch = backfill.epoch;
    let mut state_changes = mirror.state();

    let opening = state_changes.borrow().as_str().to_string();
    send_json(
        sender,
        &WsResponse::Status {
            status: opening,
            session: session.to_string(),
        },
    )
    .await;

    // Followed rather than asked for. A resize is debounced before it reaches the PTY, so asking
    // straight after requesting one answers with the geometry it is about to replace — and tells
    // the first viewer of an unowned pane that it does not own a size it is in the middle of
    // being given.
    let Some(mut geometry) = state.panes.geometry(ancillary_id).await else {
        return;
    };
    let opening_geometry = *geometry.borrow_and_update();
    report_size(opening_geometry, viewer, sender).await;

    if !backfill.bytes.is_empty()
        && sender
            .send(Message::Binary(frame(epoch, &backfill.bytes)))
            .await
            .is_err()
    {
        return;
    }

    // This viewer's terminal answers queries like any other. Its answers stop here.
    let mut typed = QueryFilter::inbound();

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
                            Ok(WsRequest::Ping) => send_json(sender, &WsResponse::Pong).await,
                            Ok(WsRequest::Resync) => {
                                epoch = resync(state, ancillary_id, epoch, sender).await;
                            }
                            Ok(request) => {
                                if let Err(e) = apply(state, ancillary_id, viewer, &mut typed, request).await {
                                    warn!("{}: {}", ancillary_id, e);
                                    send_json(sender, &WsResponse::Error { message: e.to_string() }).await;
                                }
                            }
                            Err(e) => {
                                warn!("{}: unparseable control message: {}", ancillary_id, e);
                                send_json(sender, &WsResponse::Error { message: e.to_string() }).await;
                            }
                        }
                    }
                    Some(Ok(Message::Binary(bytes))) => {
                        // For clients that send keystrokes as raw bytes.
                        let text = typed.push_text(&String::from_utf8_lossy(&bytes));
                        if !text.is_empty() {
                            if let Err(e) = state.panes.send_input(ancillary_id, viewer, &text).await {
                                warn!("{}: failed to forward input: {}", ancillary_id, e);
                            }
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
                            epoch = resync(state, ancillary_id, epoch, sender).await;
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
                        epoch = resync(state, ancillary_id, epoch, sender).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            Ok(()) = geometry.changed() => {
                let current = *geometry.borrow_and_update();
                report_size(current, viewer, sender).await;
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
            Ok(()) = state_changes.changed() => {
                let current = state_changes.borrow_and_update().clone();
                send_json(
                    sender,
                    &WsResponse::Status {
                        status: current.as_str().to_string(),
                        session: session.to_string(),
                    },
                )
                .await;
                // A mirror that cannot follow its pane is not a pane that is over: the screen
                // stops moving, the socket stays, and it starts again when the mirror does.
                if !current.is_ended() {
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
                break;
            }
        }
    }
}

/// Pane bytes as the browser reads them: the epoch they belong to, then the bytes themselves.
fn frame(epoch: u32, bytes: &[u8]) -> Vec<u8> {
    let mut framed = Vec::with_capacity(bytes.len() + 4);
    framed.extend_from_slice(&epoch.to_be_bytes());
    framed.extend_from_slice(bytes);
    framed
}

async fn report_size<S>(geometry: Geometry, viewer: ViewerId, sender: &mut S)
where
    S: SinkExt<Message> + Unpin,
{
    send_json(
        sender,
        &WsResponse::Size {
            cols: geometry.cols,
            rows: geometry.rows,
            owned: geometry.owned_by(viewer),
        },
    )
    .await;
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
                    message: format!("Failed to resync: {:#}", e),
                },
            )
            .await;
            current
        }
    }
}

async fn apply(
    state: &AppState,
    ancillary_id: &str,
    viewer: ViewerId,
    typed: &mut QueryFilter,
    request: WsRequest,
) -> anyhow::Result<()> {
    match request {
        WsRequest::Data { data } => {
            let data = typed.push_text(&data);
            if data.is_empty() {
                return Ok(());
            }
            state.panes.send_input(ancillary_id, viewer, &data).await
        }
        // Not filtered: this is a button, not a keystroke, so it cannot be a leaked reply.
        WsRequest::Interrupt => {
            state
                .panes
                .send_input(ancillary_id, viewer, INTERRUPT)
                .await
        }
        WsRequest::Resize { cols, rows } => state
            .panes
            .resize(ancillary_id, viewer, cols, rows)
            .await
            .map(|_| ()),
        WsRequest::TakeSize { cols, rows } => {
            state
                .panes
                .take_size(ancillary_id, viewer, cols, rows)
                .await
        }
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
