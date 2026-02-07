use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::ancillary::{ClientInput, WorkEvent, WorkStatus};

use super::AppState;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WsRequest {
    /// Send a message to Claude
    Message { content: String },
    /// Interrupt the current work
    Interrupt,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WsResponse {
    /// A work event from the ancillary
    Event { event: WorkEvent },
    /// Replay complete, now streaming live
    ReplayComplete { current_seq: u64 },
    /// Current status of the ancillary
    Status {
        status: String,
        ancillary_id: String,
    },
    /// Error message
    Error { message: String },
}

/// Handle WebSocket connection for observing/interacting with ancillary work
pub async fn handle_ancillary_ws(
    socket: WebSocket,
    state: AppState,
    ancillary_id: String,
    from_seq: Option<u64>,
) {
    let (mut sender, mut receiver) = socket.split();
    let client_id = uuid::Uuid::new_v4().to_string();

    info!(
        "Client {} connected to ancillary {} (from_seq: {:?})",
        client_id, ancillary_id, from_seq
    );

    // Get the active work for this ancillary
    let work = match state.work_manager.get_work(&ancillary_id).await {
        Some(work) => work,
        None => {
            let response = WsResponse::Error {
                message: format!("No active work for ancillary: {}", ancillary_id),
            };
            if let Ok(json) = serde_json::to_string(&response) {
                let _ = sender.send(Message::Text(json)).await;
            }
            return;
        }
    };

    // Log client connected
    let _ = work
        .send_input(ClientInput::Message {
            content: format!("[Client {} connected]", client_id),
            client_id: client_id.clone(),
        })
        .await;

    // Send current status
    let status = work.status().await;
    let response = WsResponse::Status {
        status: status.to_string(),
        ancillary_id: ancillary_id.clone(),
    };
    if let Ok(json) = serde_json::to_string(&response) {
        let _ = sender.send(Message::Text(json)).await;
    }

    // Replay events from the requested sequence
    let from_seq = from_seq.unwrap_or(0);
    match work.read_log_from(from_seq).await {
        Ok(events) => {
            for event in events {
                let response = WsResponse::Event { event };
                if let Ok(json) = serde_json::to_string(&response) {
                    if sender.send(Message::Text(json)).await.is_err() {
                        return;
                    }
                }
            }
        }
        Err(e) => {
            warn!("Failed to read work log: {}", e);
        }
    }

    // Signal replay complete
    let (mut event_rx, current_seq) = work.subscribe();
    let response = WsResponse::ReplayComplete { current_seq };
    if let Ok(json) = serde_json::to_string(&response) {
        let _ = sender.send(Message::Text(json)).await;
    }

    // Now stream live events and handle client input
    let input_sender = work.input_sender();

    loop {
        tokio::select! {
            // Handle incoming messages from client
            msg = receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<WsRequest>(&text) {
                            Ok(WsRequest::Message { content }) => {
                                let _ = input_sender.send(ClientInput::Message {
                                    content,
                                    client_id: client_id.clone(),
                                }).await;
                            }
                            Ok(WsRequest::Interrupt) => {
                                info!("Client {} requested interrupt", client_id);
                                let _ = input_sender.send(ClientInput::Interrupt).await;
                            }
                            Err(e) => {
                                warn!("Failed to parse client message: {}", e);
                                let response = WsResponse::Error {
                                    message: format!("Invalid request: {}", e),
                                };
                                if let Ok(json) = serde_json::to_string(&response) {
                                    let _ = sender.send(Message::Text(json)).await;
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        info!("Client {} disconnected", client_id);
                        break;
                    }
                    Some(Err(e)) => {
                        error!("WebSocket error: {}", e);
                        break;
                    }
                    _ => {}
                }
            }
            // Forward work events to client
            event = event_rx.recv() => {
                match event {
                    Ok(event) => {
                        let response = WsResponse::Event { event };
                        if let Ok(json) = serde_json::to_string(&response) {
                            if sender.send(Message::Text(json)).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        // Channel closed or lagged
                        warn!("Event channel error: {}", e);
                        // Don't break - might just be lag
                    }
                }
            }
        }

        // Check if work is done
        let status = work.status().await;
        if matches!(status, WorkStatus::Completed | WorkStatus::Failed { .. }) {
            // Send final status
            let response = WsResponse::Status {
                status: status.to_string(),
                ancillary_id: ancillary_id.clone(),
            };
            if let Ok(json) = serde_json::to_string(&response) {
                let _ = sender.send(Message::Text(json)).await;
            }
            break;
        }
    }

    info!("Client {} session ended", client_id);
}
