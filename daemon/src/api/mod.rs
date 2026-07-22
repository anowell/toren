use anyhow::Result;
use axum::{
    extract::{ws::WebSocketUpgrade, Path, State},
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
use crate::services::pane_runner::{PaneRunner, PaneStatus};
use crate::services::Services;
use tokio::sync::RwLock;
use toren_lib::{
    Agent, Assignment, AssignmentManager, CompositeStatus, Config, SegmentManager,
    WorkspaceManager,
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
    pub rhai_plugins: Arc<toren_lib::PluginManager>,
    pub ancillaries: Arc<AncillaryManager>,
    pub assignments: Arc<RwLock<AssignmentManager>>,
    pub segments: Arc<std::sync::RwLock<SegmentManager>>,
    pub workspaces: Option<Arc<WorkspaceManager>>,
    pub panes: Arc<PaneRunner>,
    pub agent: Arc<Agent>,
}

#[allow(clippy::too_many_arguments)]
pub async fn serve(
    addr: &str,
    config: Config,
    services: Services,
    security_ctx: SecurityContext,
    plugin_manager: PluginManager,
    rhai_plugins: toren_lib::PluginManager,
    ancillary_manager: AncillaryManager,
    assignment_manager: AssignmentManager,
    segment_manager: SegmentManager,
    workspace_manager: Option<WorkspaceManager>,
    pane_runner: PaneRunner,
    agent: Agent,
) -> Result<()> {
    let assignments = Arc::new(RwLock::new(assignment_manager));

    let state = AppState {
        config: Arc::new(config),
        services,
        security: Arc::new(security_ctx),
        plugins: Arc::new(plugin_manager),
        rhai_plugins: Arc::new(rhai_plugins),
        ancillaries: Arc::new(ancillary_manager),
        assignments,
        segments: Arc::new(std::sync::RwLock::new(segment_manager)),
        workspaces: workspace_manager.map(Arc::new),
        panes: Arc::new(pane_runner),
        agent: Arc::new(agent),
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
        .route(
            "/api/assignments/:id/action/:name",
            post(assignment_action),
        )
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

async fn ancillary_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(ancillary_id): Path<String>,
) -> impl IntoResponse {
    // URL decode the ancillary ID (spaces become %20)
    let ancillary_id = urlencoding::decode(&ancillary_id)
        .map(|s| s.into_owned())
        .unwrap_or(ancillary_id);

    // Adopts a `breq do` session, and re-points a mirror whose pane has since been replaced.
    adopt_current_session(&state, &ancillary_id).await;

    ws.on_upgrade(move |socket| ancillary_ws::handle_ancillary_ws(socket, state, ancillary_id))
}

async fn adopt_current_session(state: &AppState, ancillary_id: &str) {
    let assignment = {
        let mut assignments = state.assignments.write().await;
        assignments.get_active_for_ancillary(ancillary_id).cloned()
    };

    let Some(assignment) = assignment else { return };

    match state
        .panes
        .ensure_current(
            ancillary_id,
            &assignment.segment,
            &assignment.workspace_path,
            &assignment.id,
        )
        .await
    {
        Ok(session) => tracing::debug!("Mirroring current pane of rmux session {}", session),
        Err(e) => tracing::debug!("No live pane for {}: {}", ancillary_id, e),
    }
}

#[derive(Debug, Deserialize)]
struct StartWorkRequest {
    /// Assignment ID to start work on
    assignment_id: String,
    /// Optional agent override (e.g., "claude", "codex:o3"). Uses daemon default if unset.
    #[serde(default)]
    agent: Option<String>,
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

    // The pane is shared with any attached terminal.
    if state
        .panes
        .status(&assignment.segment, &assignment.workspace_path)
        .await
        == PaneStatus::Working
    {
        return Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "Ancillary already has an agent running"})),
        ));
    }

    // Resolve agent: per-request override or daemon default
    let agent = if let Some(ref agent_str) = request.agent {
        Agent::parse(agent_str).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": e.to_string()})),
            )
        })?
    } else {
        (*state.agent).clone()
    };

    start_agent(&state, &ancillary_id, &assignment, &agent, None)
        .await
        .map(|session| {
            Json(serde_json::json!({
                "success": true,
                "ancillary_id": ancillary_id,
                "session": session,
            }))
        })
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("{:#}", e)})),
            )
        })
}

/// Spawn an agent into the ancillary's rmux session and begin mirroring it.
///
/// `prompt_override` is for resume, which has already rendered a continuation prompt.
async fn start_agent(
    state: &AppState,
    ancillary_id: &str,
    assignment: &Assignment,
    agent: &Agent,
    prompt_override: Option<String>,
) -> Result<String> {
    let prompt = prompt_override.unwrap_or_else(|| assignment_prompt(assignment, &state.config));

    // Nothing is watching this terminal to answer permission prompts.
    let argv = agent.build_argv(&prompt, None, true);

    let session = state
        .panes
        .start_agent(
            ancillary_id,
            &assignment.segment,
            &assignment.workspace_path,
            &argv,
            &assignment.id,
        )
        .await?;

    capture_session_id(state, assignment, std::time::SystemTime::now());
    Ok(session)
}

/// The prompt an assignment starts its agent with: its own text, or the `act` intent rendered
/// against the task, as `breq do -i act` would.
fn assignment_prompt(assignment: &Assignment, config: &Config) -> String {
    if let toren_lib::AssignmentSource::Prompt { original_prompt } = &assignment.source {
        return original_prompt.clone();
    }

    let task_id = assignment.task_id.clone().unwrap_or_default();
    let title = assignment
        .task_title
        .clone()
        .unwrap_or_else(|| task_id.clone());

    let ctx = toren_lib::WorkspaceContext {
        ws: toren_lib::WorkspaceInfo {
            name: assignment
                .workspace_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string(),
            num: assignment.ancillary_num.unwrap_or(0),
            path: assignment.workspace_path.display().to_string(),
        },
        repo: toren_lib::RepoInfo {
            root: String::new(),
            name: assignment.segment.clone(),
        },
        task: Some(toren_lib::TaskInfo {
            id: task_id.clone(),
            title,
            description: None,
            url: assignment.task_url.clone(),
            source: assignment.task_source.clone(),
        }),
        vars: std::collections::HashMap::new(),
    };

    config
        .intents
        .get("act")
        .and_then(|template| toren_lib::render_template(template, &ctx).ok())
        .unwrap_or_else(|| format!("implement {}", task_id))
}

/// Record the agent's Claude session id on the assignment, once it exists.
///
/// The agent runs in a terminal, so there is no message to read the id out of; Claude names its
/// session log `<session-id>.jsonl`, so watch for one instead. Best-effort: a non-Claude agent
/// just leaves `session_id` unset.
fn capture_session_id(state: &AppState, assignment: &Assignment, started_at: std::time::SystemTime) {
    let assignments = state.assignments.clone();
    let assignment_id = assignment.id.clone();
    let workspace_path = assignment.workspace_path.clone();

    tokio::spawn(async move {
        for _ in 0..30 {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;

            // Unbounded, a workspace with prior logs hands back the previous run's id.
            let Some(session_id) = toren_lib::composite_status::latest_claude_session_id_since(
                &workspace_path,
                started_at,
            ) else {
                continue;
            };

            let mut mgr = assignments.write().await;
            if mgr.get(&assignment_id).and_then(|a| a.session_id.clone()).as_deref()
                == Some(session_id.as_str())
            {
                return;
            }
            if mgr
                .update_session_id(&assignment_id, Some(session_id.clone()))
                .is_ok()
            {
                tracing::info!(
                    "Captured session_id {} for assignment {}",
                    session_id,
                    assignment_id
                );
            }
            return;
        }
    });
}

async fn ancillary_stop_work(
    State(state): State<AppState>,
    Path(ancillary_id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // URL decode the ancillary ID
    let ancillary_id = urlencoding::decode(&ancillary_id)
        .map(|s| s.into_owned())
        .unwrap_or(ancillary_id);

    // From the assignment, not from what this process tracks: a `breq do` agent is equally
    // stoppable, and reporting success without killing it would be a lie.
    let assignment = {
        let mut assignments = state.assignments.write().await;
        assignments.get_active_for_ancillary(&ancillary_id).cloned()
    };
    let assignment = assignment.ok_or(StatusCode::NOT_FOUND)?;

    let stopped = state
        .panes
        .stop_agent(&ancillary_id, &assignment.segment, &assignment.workspace_path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !stopped {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(Json(serde_json::json!({
        "success": true,
        "ancillary_id": ancillary_id
    })))
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

    match ws_mgr.cleanup_workspace(
        &segment_path,
        &request.segment,
        &request.workspace,
        toren_lib::workspace::CleanupMode::Abort,
    ) {
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
    // 1. Agent activity — a live rmux pane is authoritative; session logs also catch agents
    //    started outside toren entirely.
    let pane_status = state
        .panes
        .status(&assignment.segment, &assignment.workspace_path)
        .await;
    let agent_activity = if pane_status == PaneStatus::Working {
        "busy".to_string()
    } else {
        // Fall back to Claude session log recency check
        toren_lib::composite_status::detect_agent_activity(&assignment.workspace_path)
    };

    // 2. Has changes (VCS-agnostic)
    let has_changes = toren_lib::composite_status::workspace_has_changes(
        &assignment.workspace_path,
        assignment.base_branch.as_deref(),
    );

    // 3. Task status + assignee — from task resolver
    let segment_path = {
        let segments = state.segments.read().unwrap();
        segments.find_by_name(&assignment.segment).map(|s| s.path.clone())
    };

    let (task_status, task_assignee) = if let (Some(ref seg_path), Some(ref task_id)) = (&segment_path, &assignment.task_id) {
        let ctx = toren_lib::PluginContext::new(Some(seg_path.clone()), None);
        let result = if let Some(source) = assignment.task_source.as_deref() {
            // Source is known — direct lookup
            state.rhai_plugins.resolve_info(source, task_id, ctx)
        } else {
            // Source unknown — search across all task plugins
            let sources = state.rhai_plugins.effective_sources(&state.config.tasks.sources);
            state.rhai_plugins.resolve_info_multi(&sources, task_id, ctx)
        };
        match result {
            Ok(info) => (
                info.status.unwrap_or_else(|| "unknown".to_string()),
                info.assignee.unwrap_or_default(),
            ),
            Err(_) => ("unknown".to_string(), String::new()),
        }
    } else {
        ("unknown".to_string(), String::new())
    };

    CompositeStatus {
        agent_activity,
        has_changes,
        task_status,
        task_assignee,
    }
}

// ==================== Assignment API ====================

#[derive(Debug, Deserialize)]
struct CreateAssignmentRequest {
    /// Create from existing task ID
    #[serde(default, alias = "bead_id")]
    task_id: Option<String>,
    /// Create from prompt (auto-creates task)
    #[serde(default)]
    prompt: Option<String>,
    /// Title for prompt-based creation
    #[serde(default)]
    task_title: Option<String>,
    /// Task URL
    #[serde(default)]
    task_url: Option<String>,
    /// Task source (e.g., "runes")
    #[serde(default)]
    task_source: Option<String>,
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
        .or_else(|| assignments.get_by_task_id(&id).into_iter().next().cloned());

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

    // Determine task ID - either from existing or create from prompt
    let plugin_mgr = &state.rhai_plugins;

    let (task_id, original_prompt, task_title, resolved_source) = if let Some(ref prompt) = request.prompt {
        // Create task from prompt — requires a task source
        let create_source = request.task_source.clone()
            .or_else(|| state.config.tasks.default_source().map(|s| s.to_string()))
            .ok_or_else(|| (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "task_source required when creating from prompt (no default configured)"})),
            ))?;

        let title = request.task_title.clone().unwrap_or_else(|| {
            prompt
                .lines()
                .next()
                .unwrap_or(prompt)
                .chars()
                .take(80)
                .collect()
        });

        let ctx = toren_lib::PluginContext::new(Some(segment_path.clone()), None);
        let new_task_id = plugin_mgr
            .resolve_create(&create_source, &title, Some(prompt), ctx)
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": format!("Failed to create task: {}", e)})),
                )
            })?;

        // Claim the newly created task
        let ctx = toren_lib::PluginContext::new(Some(segment_path.clone()), None);
        plugin_mgr
            .resolve_claim(&create_source, &new_task_id, "claude", ctx)
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": format!("Failed to claim task: {}", e)})),
                )
            })?;

        (new_task_id, Some(prompt.clone()), Some(title), Some(create_source))
    } else if let Some(task_id) = request.task_id.clone() {
        // Look up which source has this task, then claim it
        let task_source = request.task_source.as_deref()
            .or_else(|| state.config.tasks.default_source());
        if let Some(source) = task_source {
            let ctx = toren_lib::PluginContext::new(Some(segment_path.clone()), None);
            plugin_mgr
                .resolve_claim(source, &task_id, "claude", ctx)
                .map_err(|e| {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({"error": format!("Failed to claim task: {}", e)})),
                    )
                })?;
        }

        // Fetch task title for display — search across sources if needed
        let ctx = toren_lib::PluginContext::new(Some(segment_path.clone()), None);
        let (title, discovered_source) = if let Some(source) = request.task_source.as_deref() {
            let title = plugin_mgr.resolve_info(source, &task_id, ctx).ok().map(|t| t.title);
            (title, Some(source.to_string()))
        } else {
            let sources = plugin_mgr.effective_sources(&state.config.tasks.sources);
            match plugin_mgr.resolve_info_multi(&sources, &task_id, ctx) {
                Ok(task) => (Some(task.title.clone()), Some(task.source)),
                Err(_) => (None, state.config.tasks.default_source().map(|s| s.to_string())),
            }
        };

        (task_id, None, title, discovered_source)
    } else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Either task_id or prompt must be specified"})),
        ));
    };

    // Find next available ancillary, accounting for existing workspaces on disk
    let existing_workspaces = ws_mgr
        .list_workspaces(&segment_path)
        .unwrap_or_default();
    let ancillary_id = assignments.next_available_ancillary(
        &request.segment,
        state.config.ancillaries.max_per_segment,
        &existing_workspaces,
    );
    let ancillary_num = toren_lib::ancillary_number(&ancillary_id).unwrap_or(1);

    // Record base branch (for git worktrees; None for jj)
    let base_branch = ws_mgr.active_branch(&segment_path);

    // Generate workspace name from ancillary number word
    let ws_name = toren_lib::number_to_word(ancillary_num).to_lowercase();

    // Create workspace (with setup hooks)
    let (ws_path, _setup_result) = ws_mgr
        .create_workspace_with_setup(
            &segment_path,
            &request.segment,
            &ws_name,
            ancillary_num,
        )
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Failed to create workspace: {}", e)})),
            )
        })?;

    // Create assignment
    let source = if let Some(prompt) = original_prompt {
        toren_lib::AssignmentSource::Prompt {
            original_prompt: prompt,
        }
    } else {
        toren_lib::AssignmentSource::Reference
    };

    let assignment = assignments
        .create(
            &ancillary_id,
            Some(&task_id),
            source,
            &request.segment,
            ws_path,
            task_title,
            base_branch,
            request.task_url.as_deref(),
            resolved_source.as_deref(),
        )
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(
                    serde_json::json!({"error": format!("Failed to create assignment: {}", e)}),
                ),
            )
        })?;

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

    // Try by task ID
    let dismissed = assignments
        .dismiss_task_id(&id)
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
    // Try by task ID (first match)
    let by_task = assignments.get_by_task_id(id);
    if let Some(a) = by_task.into_iter().next() {
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

    // Stop the agent if one is running; the workspace is about to be cleaned up.
    let _ = state
        .panes
        .stop_agent(
            &assignment.ancillary_id,
            &assignment.segment,
            &assignment.workspace_path,
        )
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

    // Render auto-commit message from hardcoded template
    let auto_commit_message = toren_lib::render_auto_commit_message(
        toren_lib::DEFAULT_AUTO_COMMIT_MESSAGE,
        &assignment,
        &assignment.segment,
        &segment_path,
    );

    let opts = toren_lib::CompleteOptions {
        push: request.push,
        keep_task_open: request.keep_open,
        segment_path: &segment_path,
        kill: request.kill,
        auto_commit_message,
        plugin_mgr: &state.rhai_plugins,
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

    // Stop the agent if one is running; the workspace is about to be cleaned up.
    let _ = state
        .panes
        .stop_agent(
            &assignment.ancillary_id,
            &assignment.segment,
            &assignment.workspace_path,
        )
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
        close_task: request.close_bead,
        segment_path: &segment_path,
        kill: request.kill,
        plugin_mgr: &state.rhai_plugins,
    };

    toren_lib::abort_assignment(&assignment, &mut assignments, ws_mgr, &opts).map_err(|e| {
        let status = if e.downcast_ref::<toren_lib::WorkspaceProcessesRunning>().is_some() {
            StatusCode::CONFLICT
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        (status, Json(serde_json::json!({"error": e.to_string()})))
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
    /// Optional agent override (e.g., "claude", "codex:o3"). Uses daemon default if unset.
    #[serde(default)]
    agent: Option<String>,
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
        plugin_mgr: &state.rhai_plugins,
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

    // Resolve agent: per-request override or daemon default
    let agent = if let Some(ref agent_str) = request.agent {
        Agent::parse(agent_str).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": e.to_string()})),
            )
        })?
    } else {
        (*state.agent).clone()
    };

    // Optionally relaunch the agent with the resume prompt.
    let work_started = if request.start_work {
        if state
            .panes
            .status(&updated_assignment.segment, &updated_assignment.workspace_path)
            .await
            == PaneStatus::Working
        {
            false
        } else {
            start_agent(
                &state,
                &assignment.ancillary_id,
                &updated_assignment,
                &agent,
                Some(resume_result.prompt.clone()),
            )
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": format!("Failed to start agent: {:#}", e)})),
                )
            })?;
            true
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

// ==================== Rhai Plugin Action Endpoint ====================

#[derive(Debug, Deserialize)]
struct AssignmentActionRequest {
    #[serde(default)]
    args: Vec<String>,
}

async fn assignment_action(
    State(state): State<AppState>,
    Path((id, name)): Path<(String, String)>,
    Json(request): Json<AssignmentActionRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    // Resolve assignment to get segment context
    let mut assignments = state.assignments.write().await;
    let assignment = resolve_assignment(&mut assignments, &id).ok_or((
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"error": "Assignment not found"})),
    ))?;
    drop(assignments);

    // Check plugin exists
    if !state.rhai_plugins.has(&name) {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("Plugin '{}' not found", name)})),
        ));
    }

    // Resolve segment path for plugin context
    let (seg_path, seg_name) = {
        let segments = state.segments.read().unwrap();
        match segments.find_by_name(&assignment.segment) {
            Some(s) => (Some(s.path.clone()), Some(s.name.clone())),
            None => (None, None),
        }
    };

    let mut ctx = toren_lib::PluginContext::new(seg_path, seg_name);
    ctx.task_sources = state.config.tasks.sources.clone();

    // Run plugin in a blocking task (Rhai execution is synchronous)
    let rhai_plugins = state.rhai_plugins.clone();
    let args = request.args;
    let plugin_name = name.clone();

    let result = tokio::task::spawn_blocking(move || {
        rhai_plugins.run(&plugin_name, &args, ctx)
    })
    .await
    .map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({"error": format!("Task join error: {}", e)})),
    ))?;

    match result {
        Ok(toren_lib::PluginResult::Ok) => Ok(Json(serde_json::json!({
            "success": true,
        }))),
        Ok(toren_lib::PluginResult::Action(deferred)) => {
            // Daemon can't exec — return action details for caller to handle
            let action_json = match deferred {
                toren_lib::DeferredAction::Do {
                    task_id,
                    task_title,
                    task_url,
                    task_source,
                    prompt,
                    intent,
                } => serde_json::json!({
                    "type": "do",
                    "task_id": task_id,
                    "task_title": task_title,
                    "task_url": task_url,
                    "task_source": task_source,
                    "prompt": prompt,
                    "intent": intent,
                }),
            };
            Ok(Json(serde_json::json!({
                "success": true,
                "action": action_json,
            })))
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("{:#}", e)})),
        )),
    }
}
