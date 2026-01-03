use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{error, info, warn};

use super::AppState;
use crate::ancillary::AncillaryStatus;
use crate::services::command::CommandRequest;

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum WsRequest {
    Auth {
        token: String,
        #[serde(default)]
        ancillary_id: Option<String>,
        #[serde(default)]
        segment: Option<String>,
    },
    Command { request: CommandRequest },
    FileRead { path: String },
    VcsStatus { path: String },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum WsResponse {
    AuthSuccess { session_id: String },
    AuthFailure { reason: String },
    CommandOutput { output: crate::services::command::CommandOutput },
    FileContent { content: String },
    VcsStatus { status: crate::services::vcs::VcsStatus },
    Error { message: String },
}

pub async fn handle_websocket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let mut authenticated = false;
    let mut ancillary_id: Option<String> = None;

    info!("New WebSocket connection");

    while let Some(msg) = receiver.next().await {
        let msg = match msg {
            Ok(msg) => msg,
            Err(e) => {
                error!("WebSocket error: {}", e);
                break;
            }
        };

        if let Message::Text(text) = msg {
            let request: Result<WsRequest, _> = serde_json::from_str(&text);

            match request {
                Ok(WsRequest::Auth { token, ancillary_id: aid, segment }) => {
                    if state.security.validate_session(&token) {
                        authenticated = true;

                        // Register ancillary if ID provided
                        if let (Some(id), Some(seg)) = (aid.clone(), segment.clone()) {
                            state.ancillaries.register(id.clone(), seg, token.clone());
                            ancillary_id = Some(id.clone());
                            info!("Ancillary {} registered", id);
                        }

                        let response = WsResponse::AuthSuccess {
                            session_id: token.clone(),
                        };

                        if let Ok(json) = serde_json::to_string(&response) {
                            let _ = sender.send(Message::Text(json)).await;
                        }

                        info!("WebSocket authenticated");
                    } else {
                        let response = WsResponse::AuthFailure {
                            reason: "Invalid token".to_string(),
                        };

                        if let Ok(json) = serde_json::to_string(&response) {
                            let _ = sender.send(Message::Text(json)).await;
                        }

                        warn!("WebSocket auth failed");
                        break;
                    }
                }
                Ok(req) if authenticated => {
                    handle_authenticated_request(req, &state, &mut sender, ancillary_id.as_deref()).await;
                }
                Ok(_) => {
                    let response = WsResponse::Error {
                        message: "Not authenticated".to_string(),
                    };
                    if let Ok(json) = serde_json::to_string(&response) {
                        let _ = sender.send(Message::Text(json)).await;
                    }
                }
                Err(e) => {
                    error!("Failed to parse request: {}", e);
                    let response = WsResponse::Error {
                        message: format!("Invalid request: {}", e),
                    };
                    if let Ok(json) = serde_json::to_string(&response) {
                        let _ = sender.send(Message::Text(json)).await;
                    }
                }
            }
        }
    }

    // Unregister ancillary on disconnect
    if let Some(id) = ancillary_id {
        state.ancillaries.unregister(&id);
    }

    info!("WebSocket connection closed");
}

async fn handle_authenticated_request(
    request: WsRequest,
    state: &AppState,
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
    ancillary_id: Option<&str>,
) {
    match request {
        WsRequest::Auth { .. } => unreachable!(),

        WsRequest::Command { request } => {
            // Update status to Executing
            if let Some(id) = ancillary_id {
                state.ancillaries.update_status(id, AncillaryStatus::Executing);
            }

            match state.services.command.execute(request).await {
                Ok(mut rx) => {
                    while let Some(output) = rx.recv().await {
                        let response = WsResponse::CommandOutput { output };
                        if let Ok(json) = serde_json::to_string(&response) {
                            if sender.send(Message::Text(json)).await.is_err() {
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    let response = WsResponse::Error {
                        message: e.to_string(),
                    };
                    if let Ok(json) = serde_json::to_string(&response) {
                        let _ = sender.send(Message::Text(json)).await;
                    }
                }
            }

            // Update status back to Idle
            if let Some(id) = ancillary_id {
                state.ancillaries.update_status(id, AncillaryStatus::Idle);
            }
        }

        WsRequest::FileRead { path } => {
            let path = PathBuf::from(&path);
            match state.services.filesystem.read_file(&path) {
                Ok(content) => {
                    let response = WsResponse::FileContent { content };
                    if let Ok(json) = serde_json::to_string(&response) {
                        let _ = sender.send(Message::Text(json)).await;
                    }
                }
                Err(e) => {
                    let response = WsResponse::Error {
                        message: e.to_string(),
                    };
                    if let Ok(json) = serde_json::to_string(&response) {
                        let _ = sender.send(Message::Text(json)).await;
                    }
                }
            }
        }

        WsRequest::VcsStatus { path } => {
            let path = PathBuf::from(&path);
            match state.services.vcs.status(&path) {
                Ok(status) => {
                    let response = WsResponse::VcsStatus { status };
                    if let Ok(json) = serde_json::to_string(&response) {
                        let _ = sender.send(Message::Text(json)).await;
                    }
                }
                Err(e) => {
                    let response = WsResponse::Error {
                        message: e.to_string(),
                    };
                    if let Ok(json) = serde_json::to_string(&response) {
                        let _ = sender.send(Message::Text(json)).await;
                    }
                }
            }
        }
    }
}
