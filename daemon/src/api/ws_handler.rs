use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{error, info, warn};

use super::AppState;
use crate::ancillary::AncillaryStatus;
use crate::services::command::CommandRequest;
use toren_lib::tasks;

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum WsRequest {
    Auth {
        token: String,
        #[serde(default)]
        ancillary_id: Option<String>,
        #[serde(default)]
        segment: Option<String>,
        /// Optional workspace name - if provided, a jj workspace will be created/used
        #[serde(default)]
        workspace: Option<String>,
        /// Optional task ID - if provided, fetches task from beads and sets as instruction
        #[serde(default)]
        task_id: Option<String>,
    },
    Command { request: CommandRequest },
    FileRead { path: String },
    VcsStatus { path: String },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum WsResponse {
    AuthSuccess {
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        instruction: Option<String>,
    },
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
                Ok(WsRequest::Auth { token, ancillary_id: aid, segment, workspace, task_id }) => {
                    if state.security.validate_session(&token) {
                        authenticated = true;
                        let mut working_dir_response: Option<String> = None;

                        // Register ancillary if ID provided
                        if let (Some(id), Some(seg)) = (aid.clone(), segment.clone()) {
                            // Get segment path
                            let segment_path = {
                                let segments = state.segments.read().unwrap();
                                segments.get(&seg).map(|s| s.path.clone())
                            };

                            let (ws_name, working_dir) = match (&workspace, &segment_path) {
                                (Some(ws), Some(seg_path)) => {
                                    // Workspace requested - try to create/use it
                                    if let Some(ref ws_mgr) = state.workspaces {
                                        match ws_mgr.create_workspace(seg_path, &seg, ws) {
                                            Ok(ws_path) => {
                                                // Check if workspace is already in use
                                                if let Some(other_id) = state.ancillaries.is_workspace_in_use(&ws_path) {
                                                    let response = WsResponse::AuthFailure {
                                                        reason: format!("Workspace {} is already in use by ancillary {}", ws, other_id),
                                                    };
                                                    if let Ok(json) = serde_json::to_string(&response) {
                                                        let _ = sender.send(Message::Text(json)).await;
                                                    }
                                                    break;
                                                }
                                                working_dir_response = Some(ws_path.display().to_string());
                                                (Some(ws.clone()), ws_path)
                                            }
                                            Err(e) => {
                                                let response = WsResponse::AuthFailure {
                                                    reason: format!("Failed to create workspace: {}", e),
                                                };
                                                if let Ok(json) = serde_json::to_string(&response) {
                                                    let _ = sender.send(Message::Text(json)).await;
                                                }
                                                break;
                                            }
                                        }
                                    } else {
                                        let response = WsResponse::AuthFailure {
                                            reason: "Workspace requested but workspace_root not configured".to_string(),
                                        };
                                        if let Ok(json) = serde_json::to_string(&response) {
                                            let _ = sender.send(Message::Text(json)).await;
                                        }
                                        break;
                                    }
                                }
                                (None, Some(seg_path)) => {
                                    // No workspace - use segment path directly
                                    // Check if segment is already in use
                                    if let Some(other_id) = state.ancillaries.is_workspace_in_use(seg_path) {
                                        let response = WsResponse::AuthFailure {
                                            reason: format!("Segment {} is already in use by ancillary {}", seg, other_id),
                                        };
                                        if let Ok(json) = serde_json::to_string(&response) {
                                            let _ = sender.send(Message::Text(json)).await;
                                        }
                                        break;
                                    }
                                    working_dir_response = Some(seg_path.display().to_string());
                                    (None, seg_path.clone())
                                }
                                (_, None) => {
                                    let response = WsResponse::AuthFailure {
                                        reason: format!("Segment not found: {}", seg),
                                    };
                                    if let Ok(json) = serde_json::to_string(&response) {
                                        let _ = sender.send(Message::Text(json)).await;
                                    }
                                    break;
                                }
                            };

                            state.ancillaries.register(
                                id.clone(),
                                seg,
                                token.clone(),
                                ws_name,
                                working_dir.clone(),
                            );
                            ancillary_id = Some(id.clone());
                            info!("Ancillary {} registered", id);

                            // If task_id provided, fetch task and set instruction
                            if let Some(ref tid) = task_id {
                                match tasks::fetch_task(tid, &working_dir) {
                                    Ok(task) => {
                                        let prompt = tasks::generate_prompt(
                                            &task,
                                            &state.config.ancillary.task_prompt_template,
                                        );
                                        state.ancillaries.set_instruction(&id, Some(prompt.clone()));
                                        info!("Ancillary {} instruction set from task {}", id, tid);
                                    }
                                    Err(e) => {
                                        warn!("Failed to fetch task {}: {}", tid, e);
                                    }
                                }
                            }
                        }

                        // Get the instruction if it was set (from task_id)
                        let instruction = ancillary_id.as_ref().and_then(|id| {
                            state.ancillaries.get(id).and_then(|a| a.current_instruction.clone())
                        });

                        let response = WsResponse::AuthSuccess {
                            session_id: token.clone(),
                            working_dir: working_dir_response,
                            instruction,
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
