use anyhow::Result;
use axum::{
    extract::{
        ws::WebSocketUpgrade,
        Path, State,
    },
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::cors::CorsLayer;

use crate::ancillary::AncillaryManager;
use crate::plugins::PluginManager;
use crate::security::SecurityContext;
use crate::services::Services;
use tokio::sync::RwLock;
use toren_lib::{
    Assignment, AssignmentManager, AssignmentStatus, Config, SegmentManager, WorkspaceManager,
};

mod handlers;
mod ws_handler;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub services: Services,
    pub security: Arc<SecurityContext>,
    pub plugins: Arc<PluginManager>,
    pub ancillaries: Arc<AncillaryManager>,
    pub assignments: Arc<RwLock<AssignmentManager>>,
    pub segments: Arc<std::sync::RwLock<SegmentManager>>,
    pub workspaces: Option<Arc<WorkspaceManager>>,
}

pub async fn serve(
    addr: &str,
    config: Config,
    services: Services,
    security_ctx: SecurityContext,
    plugin_manager: PluginManager,
    ancillary_manager: AncillaryManager,
    assignment_manager: AssignmentManager,
    segment_manager: SegmentManager,
    workspace_manager: Option<WorkspaceManager>,
) -> Result<()> {
    let state = AppState {
        config: Arc::new(config),
        services,
        security: Arc::new(security_ctx),
        plugins: Arc::new(plugin_manager),
        ancillaries: Arc::new(ancillary_manager),
        assignments: Arc::new(RwLock::new(assignment_manager)),
        segments: Arc::new(std::sync::RwLock::new(segment_manager)),
        workspaces: workspace_manager.map(Arc::new),
    };

    let app = Router::new()
        .route("/health", get(health_check))
        .route("/pair", post(pair_device))
        .route("/ws", get(ws_handler))
        .route("/api/fs/read", post(handlers::fs_read))
        .route("/api/fs/write", post(handlers::fs_write))
        .route("/api/fs/list", post(handlers::fs_list))
        .route("/api/vcs/status", post(handlers::vcs_status))
        .route("/api/vcs/diff", post(handlers::vcs_diff))
        .route("/api/plugins/list", get(handlers::plugins_list))
        .route("/api/plugins/execute", post(handlers::plugins_execute))
        .route("/api/ancillaries/list", get(ancillaries_list))
        .route("/api/assignments", get(assignments_list))
        .route("/api/assignments", post(assignments_create))
        .route("/api/assignments/:id", get(assignments_get))
        .route("/api/assignments/:id", axum::routing::delete(assignments_delete))
        .route("/api/assignments/:id/status", post(assignments_update_status))
        .route("/api/segments/list", get(segments_list))
        .route("/api/segments/create", post(segments_create))
        .route("/api/workspaces/list/:segment", get(workspaces_list))
        .route("/api/workspaces/cleanup", post(workspaces_cleanup))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health_check() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

#[derive(Debug, Deserialize)]
struct PairRequest {
    pairing_token: String,
}

#[derive(Debug, Serialize)]
struct PairResponse {
    session_token: String,
    session_id: String,
}

async fn pair_device(
    State(state): State<AppState>,
    Json(request): Json<PairRequest>,
) -> Result<Json<PairResponse>, StatusCode> {
    if !state.security.validate_pairing_token(&request.pairing_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let session = state
        .security
        .create_session()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(PairResponse {
        session_token: session.token,
        session_id: session.id,
    }))
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| ws_handler::handle_websocket(socket, state))
}

async fn ancillaries_list(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let ancillaries = state.ancillaries.list();
    Json(serde_json::json!({
        "ancillaries": ancillaries,
        "count": ancillaries.len()
    }))
}

async fn segments_list(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let segments = state.segments.read().unwrap();
    let segment_list = segments.list();
    let roots = segments.roots();

    Json(serde_json::json!({
        "segments": segment_list,
        "roots": roots,
        "count": segment_list.len()
    }))
}

#[derive(Debug, Deserialize)]
struct CreateSegmentRequest {
    name: String,
    root: PathBuf,
}

async fn segments_create(
    State(state): State<AppState>,
    Json(request): Json<CreateSegmentRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut segments = state.segments.write().unwrap();

    match segments.create_segment(&request.name, &request.root) {
        Ok(segment) => Ok(Json(serde_json::json!({
            "success": true,
            "segment": segment
        }))),
        Err(e) => {
            eprintln!("Failed to create segment: {}", e);
            Err(StatusCode::BAD_REQUEST)
        }
    }
}

async fn workspaces_list(
    State(state): State<AppState>,
    Path(segment): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let ws_mgr = state.workspaces.as_ref().ok_or(StatusCode::NOT_IMPLEMENTED)?;

    let segment_path = {
        let segments = state.segments.read().unwrap();
        segments.get(&segment).map(|s| s.path.clone())
    };

    let segment_path = segment_path.ok_or(StatusCode::NOT_FOUND)?;

    match ws_mgr.list_workspaces(&segment_path) {
        Ok(workspaces) => Ok(Json(serde_json::json!({
            "segment": segment,
            "workspaces": workspaces,
            "count": workspaces.len()
        }))),
        Err(e) => {
            eprintln!("Failed to list workspaces: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

#[derive(Debug, Deserialize)]
struct WorkspaceCleanupRequest {
    segment: String,
    workspace: String,
}

async fn workspaces_cleanup(
    State(state): State<AppState>,
    Json(request): Json<WorkspaceCleanupRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let ws_mgr = state.workspaces.as_ref().ok_or(StatusCode::NOT_IMPLEMENTED)?;

    let segment_path = {
        let segments = state.segments.read().unwrap();
        segments.get(&request.segment).map(|s| s.path.clone())
    };

    let segment_path = segment_path.ok_or(StatusCode::NOT_FOUND)?;

    // Check if workspace is in use
    let ws_path = ws_mgr.workspace_path(&request.segment, &request.workspace);
    if let Some(ancillary_id) = state.ancillaries.is_workspace_in_use(&ws_path) {
        return Ok(Json(serde_json::json!({
            "success": false,
            "error": format!("Workspace is in use by ancillary {}", ancillary_id)
        })));
    }

    match ws_mgr.cleanup_workspace(&segment_path, &request.segment, &request.workspace) {
        Ok(()) => Ok(Json(serde_json::json!({
            "success": true,
            "message": format!("Workspace {} cleaned up", request.workspace)
        }))),
        Err(e) => {
            eprintln!("Failed to cleanup workspace: {}", e);
            Ok(Json(serde_json::json!({
                "success": false,
                "error": e.to_string()
            })))
        }
    }
}

// ==================== Assignment API ====================

#[derive(Debug, Deserialize)]
struct CreateAssignmentRequest {
    /// Create from existing bead ID
    #[serde(default)]
    bead_id: Option<String>,
    /// Create from prompt (auto-creates bead)
    #[serde(default)]
    prompt: Option<String>,
    /// Title for prompt-based creation
    #[serde(default)]
    title: Option<String>,
    /// Segment name
    segment: String,
}

#[derive(Debug, Serialize)]
struct AssignmentResponse {
    assignment: Assignment,
}

async fn assignments_list(State(state): State<AppState>) -> impl IntoResponse {
    let assignments = state.assignments.read().await;
    let list = assignments.list_active();

    Json(serde_json::json!({
        "assignments": list,
        "count": list.len()
    }))
}

async fn assignments_get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<AssignmentResponse>, StatusCode> {
    let assignments = state.assignments.read().await;

    // Try to find by assignment ID first
    if let Some(assignment) = assignments.get(&id) {
        return Ok(Json(AssignmentResponse {
            assignment: assignment.clone(),
        }));
    }

    // Try to find by ancillary ID
    if let Some(assignment) = assignments.get_active_for_ancillary(&id) {
        return Ok(Json(AssignmentResponse {
            assignment: assignment.clone(),
        }));
    }

    // Try to find by bead ID (return first active)
    let by_bead = assignments.get_by_bead(&id);
    if let Some(assignment) = by_bead
        .into_iter()
        .find(|a| matches!(a.status, AssignmentStatus::Pending | AssignmentStatus::Active))
    {
        return Ok(Json(AssignmentResponse {
            assignment: assignment.clone(),
        }));
    }

    Err(StatusCode::NOT_FOUND)
}

async fn assignments_create(
    State(state): State<AppState>,
    Json(request): Json<CreateAssignmentRequest>,
) -> Result<Json<AssignmentResponse>, (StatusCode, Json<serde_json::Value>)> {
    let ws_mgr = state
        .workspaces
        .as_ref()
        .ok_or((StatusCode::NOT_IMPLEMENTED, Json(serde_json::json!({"error": "workspace_root not configured"}))))?;

    // Get segment path
    let segment_path = {
        let segments = state.segments.read().unwrap();
        segments.get(&request.segment).map(|s| s.path.clone())
    };

    let segment_path = segment_path.ok_or((
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"error": format!("Segment not found: {}", request.segment)})),
    ))?;

    let mut assignments = state.assignments.write().await;

    // Determine bead ID - either from existing or create from prompt
    let (bead_id, original_prompt) = if let Some(ref prompt) = request.prompt {
        // Create bead from prompt
        let title = request.title.clone().unwrap_or_else(|| {
            prompt.lines().next().unwrap_or(prompt).chars().take(80).collect()
        });

        let new_bead_id = toren_lib::tasks::beads::create_and_claim_bead(
            &title,
            Some(prompt),
            "claude",
            &segment_path,
        )
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Failed to create bead: {}", e)})),
            )
        })?;

        (new_bead_id, Some(prompt.clone()))
    } else if let Some(bead_id) = request.bead_id.clone() {
        // Claim existing bead
        toren_lib::tasks::beads::claim_bead(&bead_id, "claude", &segment_path).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("Failed to claim bead: {}", e)})),
            )
        })?;

        (bead_id, None)
    } else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Either bead_id or prompt must be specified"})),
        ));
    };

    // Find next available ancillary
    let ancillary_id =
        assignments.next_available_ancillary(&request.segment, state.config.ancillary.pool_size);
    let ancillary_num = toren_lib::ancillary_number(&ancillary_id).unwrap_or(1);

    // Generate workspace name from ancillary number word
    let ws_name = toren_lib::number_to_word(ancillary_num).to_lowercase();

    // Create workspace
    let ws_path = ws_mgr
        .create_workspace(&segment_path, &request.segment, &ws_name)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Failed to create workspace: {}", e)})),
            )
        })?;

    // Create assignment
    let assignment = if let Some(prompt) = original_prompt {
        assignments
            .create_from_prompt(&ancillary_id, &bead_id, &prompt, &request.segment, ws_path)
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": format!("Failed to create assignment: {}", e)})),
                )
            })?
    } else {
        assignments
            .create_from_bead(&ancillary_id, &bead_id, &request.segment, ws_path)
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": format!("Failed to create assignment: {}", e)})),
                )
            })?
    };

    Ok(Json(AssignmentResponse { assignment }))
}

#[derive(Debug, Deserialize)]
struct UpdateStatusRequest {
    status: String,
}

async fn assignments_update_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<UpdateStatusRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let status = match request.status.as_str() {
        "pending" => AssignmentStatus::Pending,
        "active" => AssignmentStatus::Active,
        "completed" => AssignmentStatus::Completed,
        "aborted" => AssignmentStatus::Aborted,
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    let mut assignments = state.assignments.write().await;

    // Try to find by assignment ID first, then by ancillary ID
    let assignment_id = if assignments.get(&id).is_some() {
        id.clone()
    } else if let Some(a) = assignments.get_active_for_ancillary(&id) {
        a.id.clone()
    } else {
        return Err(StatusCode::NOT_FOUND);
    };

    assignments
        .update_status(&assignment_id, status)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(serde_json::json!({"success": true})))
}

async fn assignments_delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut assignments = state.assignments.write().await;

    // Try to find by assignment ID first
    if assignments.get(&id).is_some() {
        assignments
            .remove(&id)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        return Ok(Json(serde_json::json!({"success": true, "removed": 1})));
    }

    // Try by ancillary ID
    let dismissed = assignments
        .dismiss_ancillary(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !dismissed.is_empty() {
        return Ok(Json(
            serde_json::json!({"success": true, "removed": dismissed.len()}),
        ));
    }

    // Try by bead ID
    let dismissed = assignments
        .dismiss_bead(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !dismissed.is_empty() {
        return Ok(Json(
            serde_json::json!({"success": true, "removed": dismissed.len()}),
        ));
    }

    Err(StatusCode::NOT_FOUND)
}
