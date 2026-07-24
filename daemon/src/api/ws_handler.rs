use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{error, info, warn};

use super::AppState;
use crate::ancillary::AncillaryStatus;
use crate::services::command::CommandRequest;
use toren_lib::PlaceRegistry;

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum WsRequest {
    Auth {
        token: String,
        /// Segment to work in. When set with `workspace`, the connection resolves that place.
        #[serde(default)]
        segment: Option<String>,
        /// Workspace name within the segment.
        #[serde(default)]
        workspace: Option<String>,
    },
    Command {
        request: CommandRequest,
    },
    FileRead {
        path: String,
    },
    VcsStatus {
        path: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum WsResponse {
    AuthSuccess {
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        segment: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
    },
    AuthFailure {
        reason: String,
    },
    CommandOutput {
        output: crate::services::command::CommandOutput,
    },
    FileContent {
        content: String,
    },
    VcsStatus {
        status: crate::services::vcs::VcsStatus,
    },
    Error {
        message: String,
    },
}

pub async fn handle_websocket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let mut authenticated = false;
    // Connection-tracking key, `segment/workspace`, registered for the life of the socket.
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
                Ok(WsRequest::Auth {
                    token,
                    segment,
                    workspace,
                }) => {
                    if !state.security.validate_session(&token) {
                        let response = WsResponse::AuthFailure {
                            reason: "Invalid token".to_string(),
                        };
                        if let Ok(json) = serde_json::to_string(&response) {
                            let _ = sender.send(Message::Text(json)).await;
                        }
                        warn!("WebSocket auth failed");
                        break;
                    }

                    authenticated = true;

                    // A terminal/agent connection names a place by segment (+ workspace); the
                    // working directory is that place's path, or the segment root without one.
                    let mut working_dir: Option<String> = None;
                    if let Some(ref seg_name) = segment {
                        match resolve_working_dir(&state, seg_name, workspace.as_deref()) {
                            Ok(dir) => {
                                let key = match &workspace {
                                    Some(ws) => format!("{}/{}", seg_name, ws),
                                    None => seg_name.clone(),
                                };
                                state.ancillaries.register(
                                    key.clone(),
                                    seg_name.clone(),
                                    token.clone(),
                                    workspace.clone(),
                                    dir.clone(),
                                );
                                ancillary_id = Some(key);
                                working_dir = Some(dir.display().to_string());
                            }
                            Err(reason) => {
                                let response = WsResponse::AuthFailure { reason };
                                if let Ok(json) = serde_json::to_string(&response) {
                                    let _ = sender.send(Message::Text(json)).await;
                                }
                                break;
                            }
                        }
                    }

                    let response = WsResponse::AuthSuccess {
                        session_id: token.clone(),
                        segment,
                        workspace,
                        working_dir,
                    };
                    if let Ok(json) = serde_json::to_string(&response) {
                        let _ = sender.send(Message::Text(json)).await;
                    }

                    info!("WebSocket authenticated");
                }
                Ok(req) if authenticated => {
                    handle_authenticated_request(req, &state, &mut sender, ancillary_id.as_deref())
                        .await;
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

    // Cleanup on disconnect
    if let Some(ref id) = ancillary_id {
        state.ancillaries.unregister(id);
    }

    info!("WebSocket connection closed");
}

/// The working directory a `segment` (+ optional `workspace`) resolves to, or a reason it can't.
fn resolve_working_dir(
    state: &AppState,
    segment: &str,
    workspace: Option<&str>,
) -> Result<PathBuf, String> {
    let registry = PlaceRegistry::new(&state.config)
        .map_err(|e| format!("Failed to build place registry: {:#}", e))?;
    let seg = registry
        .segment(Some(segment))
        .map_err(|_| format!("Segment not found: {}", segment))?;

    match workspace {
        Some(ws) => {
            let place = registry.get(&seg, ws);
            Ok(place.path)
        }
        None => Ok(seg.path),
    }
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
                state
                    .ancillaries
                    .update_status(id, AncillaryStatus::Executing);
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
