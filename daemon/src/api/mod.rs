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
use crate::caddy::CaddyManager;
use crate::plugins::PluginManager;
use crate::security::SecurityContext;
use crate::services::Services;
use tokio::sync::RwLock;
use toren_lib::{
    Assignment, AssignmentManager, CompositeStatus, Config, SegmentManager, WorkspaceManager,
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
    pub caddy: Option<Arc<CaddyManager>>,
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
    caddy_manager: Option<CaddyManager>,
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
        caddy: caddy_manager.map(Arc::new),
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
        .route("/api/workspaces/proxy", post(workspaces_proxy))
        .route("/api/proxy/routes", get(proxy_routes_list))
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

    let proxy_config = state.caddy.as_ref().map(|c| c.proxy_config());
    match ws_mgr.cleanup_workspace(&segment_path, &request.segment, &request.workspace, proxy_config) {
        Ok(_result) => Ok(Json(serde_json::json!({
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

#[derive(Debug, Deserialize)]
struct WorkspaceProxyRequest {
    segment: String,
    workspace: String,
    /// Explicit port mappings (e.g., ["80:30001", "443:30002"])
    #[serde(default)]
    port_mappings: Vec<String>,
    /// Override TLS setting for explicit port mappings
    tls: Option<bool>,
}

async fn workspaces_proxy(
    State(state): State<AppState>,
    Json(request): Json<WorkspaceProxyRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let ws_mgr = state.workspaces.as_ref().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({"error": "workspace_root not configured"})),
    ))?;

    let caddy = state.caddy.as_ref().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({"error": "proxy not enabled"})),
    ))?;

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

    // Verify workspace exists
    if !ws_mgr.workspace_exists(&request.segment, &request.workspace) {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("Workspace not found: {}", request.workspace)})),
        ));
    }

    let proxy_config = caddy.proxy_config();
    let directives = if request.port_mappings.is_empty() {
        // Re-evaluate .toren.kdl proxy directives
        ws_mgr
            .evaluate_proxy_directives(
                &segment_path,
                &request.segment,
                &request.workspace,
                Some(proxy_config),
            )
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": format!("Failed to evaluate proxy directives: {}", e)})),
                )
            })?
    } else {
        // Construct directives from explicit port mappings
        let domain = &proxy_config.domain;
        let repo_name = segment_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
        let default_host = format!("{}.{}.{}", request.workspace, repo_name, domain);
        let default_tls = request.tls.unwrap_or(proxy_config.tls);

        let mut directives = Vec::new();
        for mapping in &request.port_mappings {
            let parts: Vec<&str> = mapping.splitn(2, ':').collect();
            if parts.len() != 2 {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": format!("Invalid port mapping: {} (expected port:upstream)", mapping)})),
                ));
            }
            let port: u16 = parts[0].parse().map_err(|_| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": format!("Invalid port: {}", parts[0])})),
                )
            })?;
            directives.push(toren_lib::workspace_setup::ProxyDirective {
                host: default_host.clone(),
                upstream: parts[1].to_string(),
                tls: default_tls,
                port,
            });
        }
        directives
    };

    // Register routes in Caddy
    if let Err(e) = caddy.add_routes(&directives).await {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to add Caddy routes: {}", e)})),
        ));
    }

    Ok(Json(serde_json::json!({
        "success": true,
        "directives": directives,
        "count": directives.len(),
    })))
}

async fn proxy_routes_list(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let caddy = state.caddy.as_ref().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({"error": "proxy not enabled"})),
    ))?;

    let routes = caddy.list_routes().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to list routes: {}", e)})),
        )
    })?;

    let count = routes.len();
    Ok(Json(serde_json::json!({
        "routes": routes,
        "count": count,
    })))
}

// ==================== Composite Status Helper ====================

/// Enriched assignment with composite status signals
#[derive(Debug, Serialize)]
struct EnrichedAssignment {
    #[serde(flatten)]
    assignment: Assignment,
    /// Composite status signals derived from observable state
    #[serde(flatten)]
    composite: CompositeStatus,
}

/// Compute composite status for an assignment
async fn compute_composite_status(
    assignment: &Assignment,
    state: &AppState,
) -> CompositeStatus {
    // 1. Agent activity — check work manager first, then Claude session logs
    let agent_activity = if state.work_manager.has_active_work(&assignment.ancillary_id).await {
        "busy".to_string()
    } else {
        // Fall back to Claude session log recency check
        toren_lib::composite_status::detect_agent_activity(&assignment.workspace_path)
    };

    // 2. Has changes — from jj workspace
    let has_changes = toren_lib::composite_status::workspace_has_changes(&assignment.workspace_path);

    // 3. Bead status + assignee — from bd
    let segment_path = {
        let segments = state.segments.read().unwrap();
        segments.find_by_name(&assignment.segment).map(|s| s.path.clone())
    };

    let (bead_status, bead_assignee) = if let Some(seg_path) = segment_path {
        match toren_lib::tasks::beads::fetch_bead_info(&assignment.bead_id, &seg_path) {
            Ok(info) => (info.status, info.assignee),
            Err(_) => ("unknown".to_string(), String::new()),
        }
    } else {
        ("unknown".to_string(), String::new())
    };

    CompositeStatus {
        agent_activity,
        has_changes,
        bead_status,
        bead_assignee,
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

async fn assignments_list(State(state): State<AppState>) -> impl IntoResponse {
    let mut assignments = state.assignments.write().await;
    let all: Vec<Assignment> = assignments.list().into_iter().cloned().collect();
    drop(assignments); // Release lock before async work

    // Enrich each assignment with composite status
    let mut enriched = Vec::with_capacity(all.len());
    for assignment in all {
        let composite = compute_composite_status(&assignment, &state).await;
        enriched.push(EnrichedAssignment {
            assignment,
            composite,
        });
    }

    Json(serde_json::json!({
        "assignments": enriched,
        "count": enriched.len()
    }))
}

async fn assignments_get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut assignments = state.assignments.write().await;

    // Try to find by assignment ID, ancillary ID, or bead ID
    let assignment = assignments
        .get(&id)
        .cloned()
        .or_else(|| assignments.get_active_for_ancillary(&id).cloned())
        .or_else(|| assignments.get_by_bead(&id).into_iter().next().cloned());

    drop(assignments);

    let assignment = assignment.ok_or(StatusCode::NOT_FOUND)?;
    let composite = compute_composite_status(&assignment, &state).await;

    Ok(Json(serde_json::json!({
        "assignment": EnrichedAssignment { assignment, composite }
    })))
}

async fn assignments_create(
    State(state): State<AppState>,
    Json(request): Json<CreateAssignmentRequest>,
) -> Result<Json<EnrichedAssignment>, (StatusCode, Json<serde_json::Value>)> {
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

    // Find next available ancillary, accounting for existing workspaces on disk
    let existing_workspaces = ws_mgr
        .list_workspaces(&segment_path)
        .unwrap_or_default();
    let ancillary_id = assignments.next_available_ancillary(
        &request.segment,
        state.config.ancillary.pool_size,
        &existing_workspaces,
    );
    let ancillary_num = toren_lib::ancillary_number(&ancillary_id).unwrap_or(1);

    // Generate workspace name from ancillary number word
    let ws_name = toren_lib::number_to_word(ancillary_num).to_lowercase();

    // Create workspace (with setup hooks)
    let proxy_config = state.caddy.as_ref().map(|c| c.proxy_config());
    let (ws_path, setup_result) = ws_mgr
        .create_workspace_with_setup(
            &segment_path,
            &request.segment,
            &ws_name,
            ancillary_num,
            proxy_config,
        )
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Failed to create workspace: {}", e)})),
            )
        })?;

    // Add Caddy routes if proxy directives were generated
    if let Some(ref caddy) = state.caddy {
        if let Err(e) = caddy.add_routes(&setup_result.proxy_directives).await {
            tracing::warn!("Failed to add Caddy routes: {}", e);
        }
    }

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

    let composite = compute_composite_status(&assignment, &state).await;
    Ok(Json(EnrichedAssignment { assignment, composite }))
}

#[derive(Debug, Deserialize)]
struct UpdateStatusRequest {
    /// Kept for API compatibility — all assignments are Active now
    #[allow(dead_code)]
    status: String,
}

async fn assignments_update_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(_request): Json<UpdateStatusRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // All assignments are active — status updates are no-ops.
    // Terminal transitions happen via complete/abort endpoints.
    let mut assignments = state.assignments.write().await;

    // Verify the assignment exists
    let exists = assignments.get(&id).is_some()
        || assignments.get_active_for_ancillary(&id).is_some();

    if !exists {
        return Err(StatusCode::NOT_FOUND);
    }

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
    /// Whether to kill processes running in the workspace before cleanup
    #[serde(default)]
    kill: bool,
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

    let proxy_config = state.caddy.as_ref().map(|c| c.proxy_config());
    let opts = toren_lib::CompleteOptions {
        push: request.push,
        keep_open: request.keep_open,
        segment_path: &segment_path,
        proxy_config,
        kill: request.kill,
    };

    let result =
        toren_lib::complete_assignment(&assignment, &mut assignments, ws_mgr, &opts).map_err(
            |e| {
                let status = if e.downcast_ref::<toren_lib::WorkspaceProcessesRunning>().is_some() {
                    StatusCode::CONFLICT
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                };
                (status, Json(serde_json::json!({"error": e.to_string()})))
            },
        )?;

    // Remove Caddy routes from destroy directives
    if let Some(ref caddy) = state.caddy {
        if let Err(e) = caddy.remove_routes(&result.destroy_directives).await {
            tracing::warn!("Failed to remove Caddy routes: {}", e);
        }
    }

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
    /// Whether to kill processes running in the workspace before cleanup
    #[serde(default)]
    kill: bool,
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

    let proxy_config = state.caddy.as_ref().map(|c| c.proxy_config());
    let opts = toren_lib::AbortOptions {
        close_bead: request.close_bead,
        segment_path: &segment_path,
        proxy_config,
        kill: request.kill,
    };

    let abort_result = toren_lib::abort_assignment(&assignment, &mut assignments, ws_mgr, &opts).map_err(|e| {
        let status = if e.downcast_ref::<toren_lib::WorkspaceProcessesRunning>().is_some() {
            StatusCode::CONFLICT
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        (status, Json(serde_json::json!({"error": e.to_string()})))
    })?;

    // Remove Caddy routes from destroy directives
    if let Some(ref caddy) = state.caddy {
        if let Err(e) = caddy.remove_routes(&abort_result.destroy_directives).await {
            tracing::warn!("Failed to remove Caddy routes: {}", e);
        }
    }

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

    let proxy_config = state.caddy.as_ref().map(|c| c.proxy_config());
    let opts = toren_lib::ResumeOptions {
        instruction: request.instruction.as_deref(),
        segment_path: &segment_path,
        segment_name: &assignment.segment,
        proxy_config,
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
