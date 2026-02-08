use anyhow::Result;
use axum::{
    extract::{ws::WebSocketUpgrade, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::cors::CorsLayer;

use crate::ancillary::{AncillaryManager, WorkManager};
use crate::plugins::PluginManager;
use crate::security::SecurityContext;
use crate::services::Services;
use tokio::sync::RwLock;
use toren_lib::{
    Assignment, AssignmentManager, AssignmentStatus, Config, SegmentManager, WorkspaceManager,
};

mod ancillary_ws;
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
    pub work_manager: Arc<WorkManager>,
}

#[allow(clippy::too_many_arguments)]
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
    mut work_manager: WorkManager,
) -> Result<()> {
    let assignments = Arc::new(RwLock::new(assignment_manager));

    // Give work manager a reference to assignments for status persistence
    work_manager.set_assignments(assignments.clone());

    let state = AppState {
        config: Arc::new(config),
        services,
        security: Arc::new(security_ctx),
        plugins: Arc::new(plugin_manager),
        ancillaries: Arc::new(ancillary_manager),
        assignments,
        segments: Arc::new(std::sync::RwLock::new(segment_manager)),
        workspaces: workspace_manager.map(Arc::new),
        work_manager: Arc::new(work_manager),
    };

    let app = Router::new()
        .route("/health", get(health_check))
        .route("/pair", post(pair_device))
        .route("/ws", get(ws_handler))
        .route("/ws/ancillaries/:id", get(ancillary_ws_handler))
        .route("/api/fs/read", post(handlers::fs_read))
        .route("/api/fs/write", post(handlers::fs_write))
        .route("/api/fs/list", post(handlers::fs_list))
        .route("/api/vcs/status", post(handlers::vcs_status))
        .route("/api/vcs/diff", post(handlers::vcs_diff))
        .route("/api/plugins/list", get(handlers::plugins_list))
        .route("/api/plugins/execute", post(handlers::plugins_execute))
        .route("/api/ancillaries/list", get(ancillaries_list))
        .route("/api/ancillaries/:id/start", post(ancillary_start_work))
        .route("/api/ancillaries/:id/stop", post(ancillary_stop_work))
        .route("/api/assignments", get(assignments_list))
        .route("/api/assignments", post(assignments_create))
        .route("/api/assignments/:id", get(assignments_get))
        .route(
            "/api/assignments/:id",
            axum::routing::delete(assignments_delete),
        )
        .route(
            "/api/assignments/:id/status",
            post(assignments_update_status),
        )
        .route(
            "/api/assignments/:id/complete",
            post(assignments_complete),
        )
        .route("/api/assignments/:id/abort", post(assignments_abort))
        .route("/api/assignments/:id/resume", post(assignments_resume))
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
    if !state
        .security
        .validate_pairing_token(&request.pairing_token)
    {
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

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(|socket| ws_handler::handle_websocket(socket, state))
}

#[derive(Debug, Deserialize)]
struct AncillaryWsQuery {
    from_seq: Option<u64>,
}

async fn ancillary_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(ancillary_id): Path<String>,
    Query(query): Query<AncillaryWsQuery>,
) -> impl IntoResponse {
    // URL decode the ancillary ID (spaces become %20)
    let ancillary_id = urlencoding::decode(&ancillary_id)
        .map(|s| s.into_owned())
        .unwrap_or(ancillary_id);

    ws.on_upgrade(move |socket| {
        ancillary_ws::handle_ancillary_ws(socket, state, ancillary_id, query.from_seq)
    })
}

#[derive(Debug, Deserialize)]
struct StartWorkRequest {
    /// Assignment ID to start work on
    assignment_id: String,
}

async fn ancillary_start_work(
    State(state): State<AppState>,
    Path(ancillary_id): Path<String>,
    Json(request): Json<StartWorkRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    // URL decode the ancillary ID
    let ancillary_id = urlencoding::decode(&ancillary_id)
        .map(|s| s.into_owned())
        .unwrap_or(ancillary_id);

    // Get the assignment
    let assignment = {
        let mut assignments = state.assignments.write().await;
        assignments.get(&request.assignment_id).cloned()
    };

    let assignment = assignment.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Assignment not found"})),
        )
    })?;

    // Check if ancillary already has active work
    if state.work_manager.has_active_work(&ancillary_id).await {
        return Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "Ancillary already has active work"})),
        ));
    }

    // Start work
    match state
        .work_manager
        .start_work(ancillary_id.clone(), assignment)
        .await
    {
        Ok(work) => {
            let status = work.status().await;
            Ok(Json(serde_json::json!({
                "success": true,
                "ancillary_id": ancillary_id,
                "status": status.to_string()
            })))
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )),
    }
}

async fn ancillary_stop_work(
    State(state): State<AppState>,
    Path(ancillary_id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // URL decode the ancillary ID
    let ancillary_id = urlencoding::decode(&ancillary_id)
        .map(|s| s.into_owned())
        .unwrap_or(ancillary_id);

    match state.work_manager.stop_work(&ancillary_id).await {
        Some(_) => Ok(Json(serde_json::json!({
            "success": true,
            "ancillary_id": ancillary_id
        }))),
        None => Err(StatusCode::NOT_FOUND),
    }
}

async fn ancillaries_list(State(state): State<AppState>) -> impl IntoResponse {
    let ancillaries = state.ancillaries.list();
    Json(serde_json::json!({
        "ancillaries": ancillaries,
        "count": ancillaries.len()
    }))
}

async fn segments_list(State(state): State<AppState>) -> impl IntoResponse {
    let segments = state.segments.read().unwrap();
    let roots = segments.roots();
    let all_segments = segments.list_all();

    Json(serde_json::json!({
        "roots": roots,
        "roots_count": roots.len(),
        "segments": all_segments
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
    let segments = state.segments.write().unwrap();

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
    let ws_mgr = state
        .workspaces
        .as_ref()
        .ok_or(StatusCode::NOT_IMPLEMENTED)?;

    let segment_path = {
        let segments = state.segments.read().unwrap();
        segments.find_by_name(&segment).map(|s| s.path.clone())
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
    let ws_mgr = state
        .workspaces
        .as_ref()
        .ok_or(StatusCode::NOT_IMPLEMENTED)?;

    let segment_path = {
        let segments = state.segments.read().unwrap();
        segments
            .find_by_name(&request.segment)
            .map(|s| s.path.clone())
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
    let mut assignments = state.assignments.write().await;
    let all = assignments.list();

    Json(serde_json::json!({
        "assignments": all,
        "count": all.len()
    }))
}

async fn assignments_get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<AssignmentResponse>, StatusCode> {
    let mut assignments = state.assignments.write().await;

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
    if let Some(assignment) = by_bead.into_iter().find(|a| {
        matches!(
            a.status,
            AssignmentStatus::Pending | AssignmentStatus::Active
        )
    }) {
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
    let ws_mgr = state.workspaces.as_ref().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({"error": "workspace_root not configured"})),
    ))?;

    // Get segment path
    let segment_path = {
        let segments = state.segments.read().unwrap();
        segments
            .find_by_name(&request.segment)
            .map(|s| s.path.clone())
    };

    let segment_path = segment_path.ok_or((
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"error": format!("Segment not found: {}", request.segment)})),
    ))?;

    let mut assignments = state.assignments.write().await;

    // Determine bead ID - either from existing or create from prompt
    let (bead_id, original_prompt, bead_title) = if let Some(ref prompt) = request.prompt {
        // Create bead from prompt
        let title = request.title.clone().unwrap_or_else(|| {
            prompt
                .lines()
                .next()
                .unwrap_or(prompt)
                .chars()
                .take(80)
                .collect()
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

        (new_bead_id, Some(prompt.clone()), Some(title))
    } else if let Some(bead_id) = request.bead_id.clone() {
        // Claim existing bead
        toren_lib::tasks::beads::claim_bead(&bead_id, "claude", &segment_path).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("Failed to claim bead: {}", e)})),
            )
        })?;

        // Fetch bead title for display
        let title = toren_lib::tasks::beads::fetch_bead(&bead_id, &segment_path)
            .ok()
            .map(|t| t.title);

        (bead_id, None, title)
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

    // Create workspace (with setup hooks)
    let ws_path = ws_mgr
        .create_workspace_with_setup(
            &segment_path,
            &request.segment,
            &ws_name,
            Some(ancillary_num),
        )
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Failed to create workspace: {}", e)})),
            )
        })?;

    // Create assignment
    let assignment = if let Some(prompt) = original_prompt {
        assignments
            .create_from_prompt(
                &ancillary_id,
                &bead_id,
                &prompt,
                &request.segment,
                ws_path,
                bead_title,
            )
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(
                        serde_json::json!({"error": format!("Failed to create assignment: {}", e)}),
                    ),
                )
            })?
    } else {
        assignments
            .create_from_bead(
                &ancillary_id,
                &bead_id,
                &request.segment,
                ws_path,
                bead_title,
            )
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(
                        serde_json::json!({"error": format!("Failed to create assignment: {}", e)}),
                    ),
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

// ==================== Assignment Lifecycle Endpoints ====================

/// Helper to resolve an assignment by ID, ancillary ID, or bead ID
fn resolve_assignment(assignments: &mut AssignmentManager, id: &str) -> Option<Assignment> {
    // Try by assignment ID
    if let Some(a) = assignments.get(id) {
        return Some(a.clone());
    }
    // Try by ancillary ID (active assignment)
    if let Some(a) = assignments.get_active_for_ancillary(id) {
        return Some(a.clone());
    }
    // Try by bead ID (first match of any status)
    let by_bead = assignments.get_by_bead(id);
    if let Some(a) = by_bead.into_iter().next() {
        return Some(a.clone());
    }
    None
}

#[derive(Debug, Deserialize)]
struct CompleteRequest {
    /// Whether to push changes via jj git push
    #[serde(default)]
    push: bool,
    /// Whether to keep the bead open (default: close it)
    #[serde(default)]
    keep_open: bool,
}

async fn assignments_complete(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<CompleteRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let ws_mgr = state.workspaces.as_ref().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({"error": "workspace_root not configured"})),
    ))?;

    let mut assignments = state.assignments.write().await;

    let assignment = resolve_assignment(&mut assignments, &id).ok_or((
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"error": "Assignment not found"})),
    ))?;

    // Stop active work if running
    let _ = state
        .work_manager
        .stop_work(&assignment.ancillary_id)
        .await;

    // Get segment path
    let segment_path = {
        let segments = state.segments.read().unwrap();
        segments
            .find_by_name(&assignment.segment)
            .map(|s| s.path.clone())
    };

    let segment_path = segment_path.ok_or((
        StatusCode::NOT_FOUND,
        Json(
            serde_json::json!({"error": format!("Segment not found: {}", assignment.segment)}),
        ),
    ))?;

    let opts = toren_lib::CompleteOptions {
        push: request.push,
        keep_open: request.keep_open,
        segment_path: &segment_path,
    };

    let result =
        toren_lib::complete_assignment(&assignment, &mut assignments, ws_mgr, &opts).map_err(
            |e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": e.to_string()})),
                )
            },
        )?;

    Ok(Json(serde_json::json!({
        "success": true,
        "revision": result.revision,
        "pushed": result.pushed,
    })))
}

#[derive(Debug, Deserialize)]
struct AbortRequest {
    /// Whether to close the bead (default: reopen it)
    #[serde(default)]
    close_bead: bool,
}

async fn assignments_abort(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<AbortRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let ws_mgr = state.workspaces.as_ref().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({"error": "workspace_root not configured"})),
    ))?;

    let mut assignments = state.assignments.write().await;

    let assignment = resolve_assignment(&mut assignments, &id).ok_or((
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"error": "Assignment not found"})),
    ))?;

    // Stop active work if running
    let _ = state
        .work_manager
        .stop_work(&assignment.ancillary_id)
        .await;

    // Get segment path
    let segment_path = {
        let segments = state.segments.read().unwrap();
        segments
            .find_by_name(&assignment.segment)
            .map(|s| s.path.clone())
    };

    let segment_path = segment_path.ok_or((
        StatusCode::NOT_FOUND,
        Json(
            serde_json::json!({"error": format!("Segment not found: {}", assignment.segment)}),
        ),
    ))?;

    let opts = toren_lib::AbortOptions {
        close_bead: request.close_bead,
        segment_path: &segment_path,
    };

    toren_lib::abort_assignment(&assignment, &mut assignments, ws_mgr, &opts).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
    })?;

    Ok(Json(serde_json::json!({
        "success": true,
        "bead_closed": request.close_bead,
    })))
}

#[derive(Debug, Deserialize)]
struct ResumeRequest {
    /// Custom instruction/prompt for the resumed work
    #[serde(default)]
    instruction: Option<String>,
    /// Whether to auto-start SDK work after resume preparation
    #[serde(default = "default_true")]
    start_work: bool,
}

fn default_true() -> bool {
    true
}

async fn assignments_resume(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<ResumeRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let ws_mgr = state.workspaces.as_ref().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({"error": "workspace_root not configured"})),
    ))?;

    let mut assignments = state.assignments.write().await;

    let assignment = resolve_assignment(&mut assignments, &id).ok_or((
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"error": "Assignment not found"})),
    ))?;

    // Get segment path
    let segment_path = {
        let segments = state.segments.read().unwrap();
        segments
            .find_by_name(&assignment.segment)
            .map(|s| s.path.clone())
    };

    let segment_path = segment_path.ok_or((
        StatusCode::NOT_FOUND,
        Json(
            serde_json::json!({"error": format!("Segment not found: {}", assignment.segment)}),
        ),
    ))?;

    let opts = toren_lib::ResumeOptions {
        instruction: request.instruction.as_deref(),
        segment_path: &segment_path,
        segment_name: &assignment.segment,
    };

    let resume_result =
        toren_lib::prepare_resume(&assignment, &mut assignments, ws_mgr, &opts).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
        })?;

    // Re-read the updated assignment (status may have changed)
    let updated_assignment = assignments.get(&assignment.id).cloned().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({"error": "Assignment not found after resume preparation"})),
    ))?;

    // Optionally start SDK work
    let work_started = if request.start_work {
        // Check if ancillary already has active work
        if state
            .work_manager
            .has_active_work(&assignment.ancillary_id)
            .await
        {
            false
        } else {
            // Use the assignment with the resume prompt as source
            let mut resume_assignment = updated_assignment.clone();
            resume_assignment.source = toren_lib::AssignmentSource::Prompt {
                original_prompt: resume_result.prompt.clone(),
            };

            match state
                .work_manager
                .start_work(assignment.ancillary_id.clone(), resume_assignment)
                .await
            {
                Ok(_) => true,
                Err(e) => {
                    return Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(
                            serde_json::json!({"error": format!("Failed to start work: {}", e)}),
                        ),
                    ));
                }
            }
        }
    } else {
        false
    };

    Ok(Json(serde_json::json!({
        "success": true,
        "workspace_recreated": resume_result.workspace_recreated,
        "prompt": resume_result.prompt,
        "work_started": work_started,
        "assignment": updated_assignment,
    })))
}
