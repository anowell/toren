use anyhow::Result;
use axum::{
    extract::{ws::WebSocketUpgrade, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::cors::CorsLayer;

use crate::ancillary::AncillaryManager;
use crate::security::SecurityContext;
use crate::services::pane_runner::PaneRunner;
use crate::services::Services;
use toren_lib::{
    AgentSpec, CollectOptions, Config, Place, PlaceRegistry, SegmentManager, Sets, WorkspaceManager,
};

mod ancillary_ws;
mod handlers;
mod ws_handler;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub services: Services,
    pub security: Arc<SecurityContext>,
    pub rhai_plugins: Arc<toren_lib::PluginManager>,
    pub ancillaries: Arc<AncillaryManager>,
    pub segments: Arc<std::sync::RwLock<SegmentManager>>,
    // Kept for parity with the CLI's workspace model; request handlers build a fresh
    // `PlaceRegistry` (which carries its own `WorkspaceManager`) rather than reading this.
    #[allow(dead_code)]
    pub workspaces: Option<Arc<WorkspaceManager>>,
    pub panes: Arc<PaneRunner>,
}

#[allow(clippy::too_many_arguments)]
pub async fn serve(
    addr: &str,
    config: Config,
    services: Services,
    security_ctx: SecurityContext,
    rhai_plugins: toren_lib::PluginManager,
    ancillary_manager: AncillaryManager,
    segment_manager: SegmentManager,
    workspace_manager: Option<WorkspaceManager>,
    pane_runner: PaneRunner,
) -> Result<()> {
    let state = AppState {
        config: Arc::new(config),
        services,
        security: Arc::new(security_ctx),
        rhai_plugins: Arc::new(rhai_plugins),
        ancillaries: Arc::new(ancillary_manager),
        segments: Arc::new(std::sync::RwLock::new(segment_manager)),
        workspaces: workspace_manager.map(Arc::new),
        panes: Arc::new(pane_runner),
    };

    let app = Router::new()
        .route("/health", get(health_check))
        .route("/pair", post(pair_device))
        .route("/ws", get(ws_handler))
        .route("/ws/workspaces/:segment/:name", get(workspace_ws_handler))
        .route(
            "/ws/workspaces/:segment/:name/:window",
            get(workspace_window_ws_handler),
        )
        .route("/api/fs/read", post(handlers::fs_read))
        .route("/api/fs/write", post(handlers::fs_write))
        .route("/api/fs/list", post(handlers::fs_list))
        .route("/api/vcs/status", post(handlers::vcs_status))
        .route("/api/vcs/diff", post(handlers::vcs_diff))
        .route("/api/agents", get(agents_list))
        .route("/api/ancillaries/list", get(ancillaries_list))
        .route("/api/segments/list", get(segments_list))
        .route("/api/segments/create", post(segments_create))
        .route("/api/workspaces", get(workspaces_list_all))
        .route("/api/workspaces/:segment", get(workspaces_list_segment))
        .route("/api/workspaces/:segment/:name", get(workspace_get))
        .route(
            "/api/workspaces/:segment/:name/start",
            post(workspace_start),
        )
        .route("/api/workspaces/:segment/:name/stop", post(workspace_stop))
        .route(
            "/api/workspaces/:segment/:name/shell",
            post(workspace_open_shell),
        )
        .route(
            "/api/workspaces/:segment/:name/sessions",
            get(workspace_sessions),
        )
        .route(
            "/api/workspaces/:segment/:name/windows/:window/close",
            post(workspace_close_window),
        )
        .layer(CorsLayer::permissive())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health_check() -> impl IntoResponse {
    Json(json!({
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

// ==================== Workspace terminal mirror ====================

/// Attach a browser terminal to a workspace's default window: the agent if one exists, else its
/// first shell. `/ws/workspaces/:segment/:name/:window` targets a specific window.
async fn workspace_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path((segment, name)): Path<(String, String)>,
) -> impl IntoResponse {
    let key = adopt_current_pane(&state, &segment, &name, None).await;
    ws.on_upgrade(move |socket| ancillary_ws::handle_ancillary_ws(socket, state, key))
}

/// Attach to a specific window of a workspace's session (`agent`, `shell`, `shell-2`, …).
async fn workspace_window_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path((segment, name, window)): Path<(String, String, String)>,
) -> impl IntoResponse {
    let key = adopt_current_pane(&state, &segment, &name, Some(&window)).await;
    ws.on_upgrade(move |socket| ancillary_ws::handle_ancillary_ws(socket, state, key))
}

/// The window a bare workspace attach lands on: the agent if it exists, else the first live
/// shell, else the `shell` slot (which may be dead — the mirror handler reports that cleanly).
fn default_window(session: &str) -> String {
    if toren_lib::rmux::window_exists(session, toren_lib::rmux::AGENT_WINDOW) {
        return toren_lib::rmux::AGENT_WINDOW.to_string();
    }
    toren_lib::rmux::list_windows(session)
        .unwrap_or_default()
        .into_iter()
        .find(|w| w.starts_with(toren_lib::rmux::SHELL_WINDOW))
        .unwrap_or_else(|| toren_lib::rmux::SHELL_WINDOW.to_string())
}

/// A mirror tracking key scoped to one window of one session.
///
/// Per-window so a workspace can mirror its agent and several shells at once without them
/// clobbering each other's recorders.
fn window_key(session: &str, window: &str) -> String {
    format!("{}:{}", session, window)
}

/// Point a per-window mirror at the pane running right now, returning its tracking key.
///
/// Returns the key even when no pane is live — the mirror handler reports the absence to the
/// client rather than failing the upgrade.
async fn adopt_current_pane(
    state: &AppState,
    segment: &str,
    name: &str,
    window: Option<&str>,
) -> String {
    let Ok(registry) = PlaceRegistry::new(&state.config) else {
        return String::new();
    };
    let Ok(seg) = registry.segment(Some(segment)) else {
        return String::new();
    };
    let place = registry.get(&seg, name);
    let session = place.session_name();
    let window = window
        .map(|w| w.to_string())
        .unwrap_or_else(|| default_window(&session));
    let key = window_key(&session, &window);

    match state.panes.ensure_current(&key, &session, &window).await {
        Ok(s) => tracing::debug!("Mirroring window '{}' of rmux session {}", window, s),
        Err(e) => tracing::debug!("No live '{}' pane for {}/{}: {}", window, segment, name, e),
    }

    key
}

/// The agents this daemon can start, so the browser offers one action per agent ("New Claude
/// agent") rather than a single button that hides which one it means.
async fn agents_list(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "agents": state.rhai_plugins.list_agents(),
        "default": state.config.ancillaries.agent,
    }))
}

async fn ancillaries_list(State(state): State<AppState>) -> impl IntoResponse {
    let ancillaries = state.ancillaries.list();
    Json(json!({
        "ancillaries": ancillaries,
        "count": ancillaries.len()
    }))
}

async fn segments_list(State(state): State<AppState>) -> impl IntoResponse {
    let segments = state.segments.read().unwrap();
    let roots = segments.roots();
    let all_segments = segments.list_all();

    Json(json!({
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
        Ok(segment) => Ok(Json(json!({
            "success": true,
            "segment": segment
        }))),
        Err(e) => {
            eprintln!("Failed to create segment: {}", e);
            Err(StatusCode::BAD_REQUEST)
        }
    }
}

// ==================== Workspace API ====================

/// One workspace, rendered exactly as `breq get <ws> --json` emits it
/// (`breq/src/render.rs::detail_json`): the place's identity plus its full [`Sets`].
fn workspace_view(
    place: &Place,
    sets: &Sets,
    plugins: &toren_lib::PluginManager,
) -> serde_json::Value {
    json!({
        "name": place.name,
        "segment": place.segment,
        "uid": place.uid(),
        "path": place.path,
        "title": sets.title(place, plugins),
        "base": place.base(),
        "parent": place.parent(),
        "decorated": place.is_decorated(),
        "vcs_tracked": place.vcs_tracked,
        "state": place.state,
        "sets": sets,
    })
}

/// Collect a `WorkspaceView` for each place. Remote-derived sets are read from the cache and
/// never refreshed here, so listing never blocks on the network — the single-workspace view is
/// the write-through point that keeps those entries current.
fn collect_views(
    registry: &PlaceRegistry,
    config: &Config,
    plugins: &toren_lib::PluginManager,
    places: Vec<Place>,
) -> Vec<serde_json::Value> {
    places
        .into_iter()
        .map(|place| {
            let sets = Sets::collect(
                &place,
                &registry.workspaces,
                plugins,
                config,
                CollectOptions::cached(),
            );
            workspace_view(&place, &sets, plugins)
        })
        .collect()
}

async fn workspaces_list_all(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let registry = PlaceRegistry::new(&state.config).map_err(|e| {
        tracing::error!("Failed to build place registry: {:#}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let views = collect_views(
        &registry,
        &state.config,
        &state.rhai_plugins,
        registry.list_all(),
    );

    Ok(Json(json!({
        "workspaces": views,
        "count": views.len(),
    })))
}

async fn workspaces_list_segment(
    State(state): State<AppState>,
    Path(segment): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let registry = PlaceRegistry::new(&state.config).map_err(|e| {
        tracing::error!("Failed to build place registry: {:#}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let seg = registry
        .segment(Some(&segment))
        .map_err(|_| StatusCode::NOT_FOUND)?;

    let views = collect_views(
        &registry,
        &state.config,
        &state.rhai_plugins,
        registry.list(&seg),
    );

    Ok(Json(json!({
        "segment": segment,
        "workspaces": views,
        "count": views.len(),
    })))
}

async fn workspace_get(
    State(state): State<AppState>,
    Path((segment, name)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let registry = PlaceRegistry::new(&state.config).map_err(|e| {
        tracing::error!("Failed to build place registry: {:#}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let seg = registry
        .segment(Some(&segment))
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let mut place = registry
        .require(&seg, &name)
        .map_err(|_| StatusCode::NOT_FOUND)?;

    // Looking at one workspace is also when a finished agent session gets its ending written
    // down, since nothing watches the pane for it.
    toren_lib::sessions::settle_saved(&mut place, &state.rhai_plugins);

    // One workspace at a time: this is the daemon's write-through point, so the list endpoints
    // (and `breq list`) render metadata this refreshed rather than paying for it themselves.
    let sets = Sets::collect(
        &place,
        &registry.workspaces,
        &state.rhai_plugins,
        &state.config,
        CollectOptions::live(),
    );

    Ok(Json(json!({
        "workspace": workspace_view(&place, &sets, &state.rhai_plugins),
    })))
}

#[derive(Debug, Deserialize)]
struct StartRequest {
    /// Agent override, e.g. "claude" or "codex:o3". Falls back to the workspace's own agent,
    /// then the configured default.
    #[serde(default)]
    agent: Option<String>,
    /// Prompt to seed the agent with.
    #[serde(default)]
    prompt: Option<String>,
    /// Model override for the resolved agent.
    #[serde(default)]
    model: Option<String>,
    /// Resume the workspace's previous session instead of starting fresh.
    #[serde(default)]
    resume: bool,
    /// Resume one *named* session, out of the workspace's recorded list. Implies `resume`.
    #[serde(default)]
    session: Option<String>,
}

async fn workspace_start(
    State(state): State<AppState>,
    Path((segment, name)): Path<(String, String)>,
    Json(request): Json<StartRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let registry = PlaceRegistry::new(&state.config).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("{:#}", e)})),
        )
    })?;

    let seg = registry.segment(Some(&segment)).map_err(|_| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("Segment not found: {}", segment)})),
        )
    })?;
    let mut place = registry.require(&seg, &name).map_err(|_| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("Workspace not found: {}/{}", segment, name)})),
        )
    })?;

    let session = place.session_name();
    let agent_key = window_key(&session, toren_lib::rmux::AGENT_WINDOW);

    // The pane is shared with any attached terminal — a second spawn would fight the first.
    if state
        .panes
        .status(&session, toren_lib::rmux::AGENT_WINDOW)
        .await
        == toren_mirror::PaneLiveness::Running
    {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({"error": "Workspace already has an agent running"})),
        ));
    }

    // Resolve agent: per-request override → the workspace's own → configured default.
    let stored = place.agent();
    let resolved = AgentSpec::resolve(
        &state.rhai_plugins,
        request.agent.as_deref(),
        stored.as_ref(),
        state.config.ancillaries.agent.as_deref(),
    )
    .map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": e.to_string()})),
        )
    })?;
    let agent = AgentSpec {
        name: resolved.name,
        model: request.model.clone().or(resolved.model),
    };

    // Remember the choice so a later attach or resume uses the same agent.
    place.state.set_agent(&agent.name, agent.model.as_deref());
    place.save().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to save workspace: {:#}", e)})),
        )
    })?;

    let resuming = request.resume || request.session.is_some();
    let session_id = resuming
        .then(|| {
            toren_lib::sessions::resume_target(
                &place,
                &state.rhai_plugins,
                &agent.name,
                request.session.as_deref(),
            )
        })
        .flatten();

    // Nothing is watching this terminal to answer permission prompts.
    let argv = if resuming {
        agent.resume_argv_for(
            &state.rhai_plugins,
            session_id.as_deref(),
            request.prompt.as_deref(),
            true,
        )
    } else {
        agent.argv(&state.rhai_plugins, request.prompt.as_deref(), true)
    }
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to build agent argv: {:#}", e)})),
        )
    })?;

    // Same recorder `breq do` uses, so a session started from the browser is equally resumable.
    if let Err(e) = toren_lib::sessions::record_start(
        &mut place,
        &state.rhai_plugins,
        &agent.name,
        session_id.as_deref(),
    ) {
        tracing::warn!("Failed to record the agent session: {:#}", e);
    }

    let session = state
        .panes
        .start_agent(&agent_key, &session, &place.path, &place.env(), &argv)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("{:#}", e)})),
            )
        })?;

    Ok(Json(json!({
        "success": true,
        "session": session,
        "window": toren_lib::rmux::AGENT_WINDOW,
        "agent_session": session_id,
    })))
}

/// Open a fresh shell window in a workspace's session and return its name, so the browser can
/// attach a terminal to it. A workspace can hold several shells at once.
async fn workspace_open_shell(
    State(state): State<AppState>,
    Path((segment, name)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let registry = PlaceRegistry::new(&state.config).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("{:#}", e)})),
        )
    })?;

    let seg = registry.segment(Some(&segment)).map_err(|_| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("Segment not found: {}", segment)})),
        )
    })?;
    let place = registry.require(&seg, &name).map_err(|_| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("Workspace not found: {}/{}", segment, name)})),
        )
    })?;

    let session = place.session_name();
    let window = state
        .panes
        .open_shell(&session, &place.path, &place.env())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("{:#}", e)})),
            )
        })?;

    Ok(Json(json!({
        "success": true,
        "session": session,
        "window": window,
    })))
}

/// The agent sessions recorded for a workspace, oldest first, straight out of `state.json`.
///
/// Separate from the workspace view because picking a session to resume should not pay for the
/// task and PR round trips that view makes: this is a file read.
async fn workspace_sessions(
    State(state): State<AppState>,
    Path((segment, name)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let registry = PlaceRegistry::new(&state.config).map_err(|e| {
        tracing::error!("Failed to build place registry: {:#}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let seg = registry
        .segment(Some(&segment))
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let place = registry
        .require(&seg, &name)
        .map_err(|_| StatusCode::NOT_FOUND)?;

    Ok(Json(json!({
        "sessions": place.state.sessions(),
        "agent": place.state.agent.as_ref().map(|a| a.name.clone()),
    })))
}

/// Dismiss one window of a workspace's session — the browser's `<Ctrl-c>` on a held pane.
///
/// Every resume is a new pane and a held one outlives its process on purpose, so a workspace
/// accumulates them; getting rid of one has to cost a single click.
async fn workspace_close_window(
    State(state): State<AppState>,
    Path((segment, name, window)): Path<(String, String, String)>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let registry = PlaceRegistry::new(&state.config).map_err(|e| {
        tracing::error!("Failed to build place registry: {:#}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let seg = registry
        .segment(Some(&segment))
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let mut place = registry
        .require(&seg, &name)
        .map_err(|_| StatusCode::NOT_FOUND)?;

    let session = place.session_name();
    let key = window_key(&session, &window);
    let was_live = state
        .panes
        .close_window(&key, &session, &window)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Dismissing the agent's pane is the last chance to read what its session ended as.
    if window == toren_lib::rmux::AGENT_WINDOW {
        toren_lib::sessions::settle_saved(&mut place, &state.rhai_plugins);
    }

    Ok(Json(json!({ "success": true, "was_live": was_live })))
}

async fn workspace_stop(
    State(state): State<AppState>,
    Path((segment, name)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let registry = PlaceRegistry::new(&state.config).map_err(|e| {
        tracing::error!("Failed to build place registry: {:#}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let seg = registry
        .segment(Some(&segment))
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let mut place = registry
        .require(&seg, &name)
        .map_err(|_| StatusCode::NOT_FOUND)?;

    let session = place.session_name();
    let agent_key = window_key(&session, toren_lib::rmux::AGENT_WINDOW);

    // Works off the place's session rather than what this process tracks, so a `breq do` agent
    // is equally stoppable. Idempotent: stopping an already-stopped agent still reports success.
    state
        .panes
        .stop_agent(&agent_key, &session)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // The one moment a session's end is known exactly, so snapshot it here rather than waiting
    // for something to notice later.
    toren_lib::sessions::settle_saved(&mut place, &state.rhai_plugins);

    Ok(Json(json!({ "success": true })))
}
