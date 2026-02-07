use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{error, info, warn};

use super::AppState;
use crate::ancillary::AncillaryStatus;
use crate::services::command::CommandRequest;
use toren_lib::{tasks, AssignmentStatus};

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum WsRequest {
    Auth {
        token: String,
        /// Ancillary ID to connect as (e.g., "Toren One")
        /// If provided, looks up assignment for this ancillary
        #[serde(default)]
        ancillary_id: Option<String>,
        /// Segment - only used if no assignment found (legacy mode)
        #[serde(default)]
        segment: Option<String>,
        /// Workspace name - only used if no assignment found (legacy mode)
        #[serde(default)]
        workspace: Option<String>,
        /// Task ID - only used if no assignment found (legacy mode)
        #[serde(default)]
        task_id: Option<String>,
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
        ancillary_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        assignment_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        bead_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        instruction: Option<String>,
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
    let mut ancillary_id: Option<String> = None;
    let mut assignment_id: Option<String> = None;

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
                    ancillary_id: aid,
                    segment,
                    workspace,
                    task_id,
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

                    // Try to connect via assignment first
                    if let Some(ref id) = aid {
                        match connect_via_assignment(&state, id, &token).await {
                            Ok((response, aid_clone, assign_id)) => {
                                ancillary_id = Some(aid_clone);
                                assignment_id = Some(assign_id);
                                if let Ok(json) = serde_json::to_string(&response) {
                                    let _ = sender.send(Message::Text(json)).await;
                                }
                                info!("WebSocket authenticated via assignment for {}", id);
                                continue;
                            }
                            Err(Some(reason)) => {
                                // Assignment lookup failed with specific error
                                let response = WsResponse::AuthFailure { reason };
                                if let Ok(json) = serde_json::to_string(&response) {
                                    let _ = sender.send(Message::Text(json)).await;
                                }
                                break;
                            }
                            Err(None) => {
                                // No assignment found, fall through to legacy mode
                                info!("No assignment found for {}, trying legacy mode", id);
                            }
                        }
                    }

                    // Legacy mode: create workspace on-the-fly (for backwards compatibility)
                    let mut working_dir_response: Option<String> = None;

                    if let (Some(id), Some(seg)) = (aid.clone(), segment.clone()) {
                        let segment_path = {
                            let segments = state.segments.read().unwrap();
                            segments.find_by_name(&seg).map(|s| s.path.clone())
                        };

                        let (ws_name, working_dir) = match (&workspace, &segment_path) {
                            (Some(ws), Some(seg_path)) => {
                                if let Some(ref ws_mgr) = state.workspaces {
                                    match ws_mgr.create_workspace(seg_path, &seg, ws) {
                                        Ok(ws_path) => {
                                            if let Some(other_id) =
                                                state.ancillaries.is_workspace_in_use(&ws_path)
                                            {
                                                let response = WsResponse::AuthFailure {
                                                    reason: format!("Workspace {} is already in use by ancillary {}", ws, other_id),
                                                };
                                                if let Ok(json) = serde_json::to_string(&response) {
                                                    let _ = sender.send(Message::Text(json)).await;
                                                }
                                                break;
                                            }
                                            working_dir_response =
                                                Some(ws_path.display().to_string());
                                            (Some(ws.clone()), ws_path)
                                        }
                                        Err(e) => {
                                            let response = WsResponse::AuthFailure {
                                                reason: format!(
                                                    "Failed to create workspace: {}",
                                                    e
                                                ),
                                            };
                                            if let Ok(json) = serde_json::to_string(&response) {
                                                let _ = sender.send(Message::Text(json)).await;
                                            }
                                            break;
                                        }
                                    }
                                } else {
                                    let response = WsResponse::AuthFailure {
                                        reason:
                                            "Workspace requested but workspace_root not configured"
                                                .to_string(),
                                    };
                                    if let Ok(json) = serde_json::to_string(&response) {
                                        let _ = sender.send(Message::Text(json)).await;
                                    }
                                    break;
                                }
                            }
                            (None, Some(seg_path)) => {
                                if let Some(other_id) =
                                    state.ancillaries.is_workspace_in_use(seg_path)
                                {
                                    let response = WsResponse::AuthFailure {
                                        reason: format!(
                                            "Segment {} is already in use by ancillary {}",
                                            seg, other_id
                                        ),
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
                            ws_name.map(|s| s.to_string()),
                            working_dir.clone(),
                        );
                        ancillary_id = Some(id.clone());
                        info!("Ancillary {} registered (legacy mode)", id);

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

                    // Get the instruction if it was set
                    let instruction = ancillary_id.as_ref().and_then(|id| {
                        state
                            .ancillaries
                            .get(id)
                            .and_then(|a| a.current_instruction.clone())
                    });

                    let response = WsResponse::AuthSuccess {
                        session_id: token.clone(),
                        ancillary_id: ancillary_id.clone(),
                        assignment_id: None,
                        bead_id: None,
                        working_dir: working_dir_response,
                        instruction,
                    };

                    if let Ok(json) = serde_json::to_string(&response) {
                        let _ = sender.send(Message::Text(json)).await;
                    }

                    info!("WebSocket authenticated (legacy mode)");
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

    // Update assignment status back to Pending on disconnect
    if let Some(ref assign_id) = assignment_id {
        let mut assignments = state.assignments.write().await;
        let _ = assignments.update_status(assign_id, AssignmentStatus::Pending);
    }

    info!("WebSocket connection closed");
}

/// Connect using an existing assignment
async fn connect_via_assignment(
    state: &AppState,
    ancillary_id: &str,
    session_token: &str,
) -> Result<(WsResponse, String, String), Option<String>> {
    let mut assignments = state.assignments.write().await;

    // Look up active assignment for this ancillary
    let assignment = assignments
        .get_active_for_ancillary(ancillary_id)
        .cloned()
        .ok_or(None)?; // None = no assignment found, fall through to legacy

    let working_dir = &assignment.workspace_path;

    // Check if workspace exists, recreate if needed
    if !working_dir.exists() {
        if let Some(ref ws_mgr) = state.workspaces {
            let segment_path = {
                let segments = state.segments.read().unwrap();
                segments.find_by_name(&assignment.segment).map(|s| s.path)
            };

            if let Some(seg_path) = segment_path {
                let ws_name = working_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&assignment.bead_id);

                if let Err(e) = ws_mgr.create_workspace(&seg_path, &assignment.segment, ws_name) {
                    return Err(Some(format!("Failed to recreate workspace: {}", e)));
                }
                info!("Recreated workspace for assignment {}", assignment.id);
            }
        }
    }

    // Check for workspace collision with other connected ancillaries
    if let Some(other_id) = state.ancillaries.is_workspace_in_use(working_dir) {
        if other_id != ancillary_id {
            return Err(Some(format!(
                "Workspace is already in use by ancillary {}",
                other_id
            )));
        }
    }

    // Register the ancillary for connection tracking
    state.ancillaries.register(
        ancillary_id.to_string(),
        assignment.segment.clone(),
        session_token.to_string(),
        Some(assignment.bead_id.clone()),
        working_dir.clone(),
    );

    // Generate instruction from bead
    let instruction = match tasks::fetch_task(&assignment.bead_id, working_dir) {
        Ok(task) => {
            let prompt =
                tasks::generate_prompt(&task, &state.config.ancillary.task_prompt_template);
            state
                .ancillaries
                .set_instruction(ancillary_id, Some(prompt.clone()));
            Some(prompt)
        }
        Err(e) => {
            warn!("Failed to fetch task {}: {}", assignment.bead_id, e);
            None
        }
    };

    // Update assignment status to Active
    let _ = assignments.update_status(&assignment.id, AssignmentStatus::Active);

    let response = WsResponse::AuthSuccess {
        session_id: session_token.to_string(),
        ancillary_id: Some(ancillary_id.to_string()),
        assignment_id: Some(assignment.id.clone()),
        bead_id: Some(assignment.bead_id.clone()),
        working_dir: Some(working_dir.display().to_string()),
        instruction,
    };

    Ok((response, ancillary_id.to_string(), assignment.id.clone()))
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
