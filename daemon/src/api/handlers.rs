use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::AppState;

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
