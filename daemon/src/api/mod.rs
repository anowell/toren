use anyhow::Result;
use axum::{
    extract::{
        ws::WebSocketUpgrade,
        State,
    },
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tower_http::cors::CorsLayer;

use crate::ancillary::AncillaryManager;
use crate::plugins::PluginManager;
use crate::security::SecurityContext;
use crate::services::Services;

mod handlers;
mod ws_handler;

#[derive(Clone)]
pub struct AppState {
    pub services: Services,
    pub security: Arc<SecurityContext>,
    pub plugins: Arc<PluginManager>,
    pub ancillaries: Arc<AncillaryManager>,
}

pub async fn serve(
    addr: &str,
    services: Services,
    security_ctx: SecurityContext,
    plugin_manager: PluginManager,
    ancillary_manager: AncillaryManager,
) -> Result<()> {
    let state = AppState {
        services,
        security: Arc::new(security_ctx),
        plugins: Arc::new(plugin_manager),
        ancillaries: Arc::new(ancillary_manager),
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
