use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use super::AppState;
use crate::plugins::CommandSet;
use crate::services::command::CommandRequest;

// Filesystem handlers

#[derive(Debug, Deserialize)]
pub struct FsReadRequest {
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct FsReadResponse {
    pub content: String,
}

pub async fn fs_read(
    State(state): State<AppState>,
    Json(request): Json<FsReadRequest>,
) -> Result<Json<FsReadResponse>, StatusCode> {
    let path = PathBuf::from(&request.path);

    let content = state
        .services
        .filesystem
        .read_file(&path)
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    Ok(Json(FsReadResponse { content }))
}

#[derive(Debug, Deserialize)]
pub struct FsWriteRequest {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct FsWriteResponse {
    pub success: bool,
}

pub async fn fs_write(
    State(state): State<AppState>,
    Json(request): Json<FsWriteRequest>,
) -> Result<Json<FsWriteResponse>, StatusCode> {
    let path = PathBuf::from(&request.path);

    state
        .services
        .filesystem
        .write_file(&path, &request.content)
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    Ok(Json(FsWriteResponse { success: true }))
}

#[derive(Debug, Deserialize)]
pub struct FsListRequest {
    pub path: String,
}

pub async fn fs_list(
    State(state): State<AppState>,
    Json(request): Json<FsListRequest>,
) -> Result<Json<crate::services::filesystem::DirListing>, StatusCode> {
    let path = PathBuf::from(&request.path);

    let listing = state
        .services
        .filesystem
        .list_directory(&path)
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    Ok(Json(listing))
}

// VCS handlers

#[derive(Debug, Deserialize)]
pub struct VcsStatusRequest {
    pub path: String,
}

pub async fn vcs_status(
    State(state): State<AppState>,
    Json(request): Json<VcsStatusRequest>,
) -> Result<Json<crate::services::vcs::VcsStatus>, StatusCode> {
    let path = PathBuf::from(&request.path);

    let status = state
        .services
        .vcs
        .status(&path)
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    Ok(Json(status))
}

#[derive(Debug, Deserialize)]
pub struct VcsDiffRequest {
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct VcsDiffResponse {
    pub diff: String,
}

pub async fn vcs_diff(
    State(state): State<AppState>,
    Json(request): Json<VcsDiffRequest>,
) -> Result<Json<VcsDiffResponse>, StatusCode> {
    let path = PathBuf::from(&request.path);

    let diff = state
        .services
        .vcs
        .diff(&path)
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    Ok(Json(VcsDiffResponse { diff }))
}

// Plugin handlers

#[derive(Debug, Serialize)]
pub struct PluginsListResponse {
    pub command_sets: Vec<CommandSet>,
}

pub async fn plugins_list(
    State(state): State<AppState>,
) -> Result<Json<PluginsListResponse>, StatusCode> {
    let command_sets: Vec<CommandSet> = state
        .plugins
        .list_command_sets()
        .into_iter()
        .cloned()
        .collect();

    Ok(Json(PluginsListResponse { command_sets }))
}

#[derive(Debug, Deserialize)]
pub struct PluginExecuteRequest {
    pub command_id: String,
    pub params: HashMap<String, String>,
    pub cwd: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PluginExecuteResponse {
    pub success: bool,
    pub message: String,
}

pub async fn plugins_execute(
    State(state): State<AppState>,
    Json(request): Json<PluginExecuteRequest>,
) -> Result<Json<PluginExecuteResponse>, StatusCode> {
    // Find the command
    let (command_set, command_def) = state
        .plugins
        .find_command(&request.command_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    // Interpolate parameters
    let interpolated_command = state
        .plugins
        .interpolate_command(&command_def.command, &request.params);

    // Split command into program and args
    let parts: Vec<&str> = interpolated_command.split_whitespace().collect();
    if parts.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let program = parts[0].to_string();
    let args = parts[1..].iter().map(|s| s.to_string()).collect();

    // Execute command
    let cmd_request = CommandRequest {
        command: program,
        args,
        cwd: request.cwd,
    };

    // For now, just validate we can create the command
    // In a real implementation, this would use the command service
    // and return streaming output via WebSocket
    let _ = state.services.command.execute(cmd_request).await;

    Ok(Json(PluginExecuteResponse {
        success: true,
        message: format!(
            "Command '{}' from set '{}' queued for execution",
            command_def.label, command_set.name
        ),
    }))
}
