//! Shared assignment lifecycle operations used by both breq CLI and toren daemon.
//!
//! These functions implement the complete/abort/resume/clean logic so both interfaces
//! behave identically.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;
use tracing::info;

use crate::assignment::{AssignmentManager, CompletionReason};
use crate::workspace::{CleanupMode, CommitInfo, WorkspaceManager};
use crate::workspace_setup::{SetupResult, WorkspaceContext, WorkspaceInfo, RepoInfo, TaskInfo};
use crate::Assignment;

/// Options for completing an assignment
pub struct CompleteOptions<'a> {
    /// Whether to push changes
    pub push: bool,
    /// Whether to keep the task open (default: close it)
    pub keep_task_open: bool,
    /// Segment path for running workspace hooks and task commands
    pub segment_path: &'a Path,
    /// Whether to kill processes running in the workspace
    pub kill: bool,
    /// Auto-commit message (rendered template). If Some, auto-commit before capture.
    pub auto_commit_message: Option<String>,
    /// Plugin manager for resolver-based task operations
    pub plugin_mgr: &'a crate::plugins::PluginManager,
}

/// Result from completing an assignment
pub struct CompleteResult {
    /// The revision hash if captured before cleanup
    pub revision: Option<String>,
    /// Whether changes were pushed
    pub pushed: bool,
    /// Commits exclusive to this workspace (captured before cleanup)
    pub workspace_info: Vec<CommitInfo>,
}

/// Options for aborting an assignment
pub struct AbortOptions<'a> {
    /// Whether to close the task (default: reopen it)
    pub close_task: bool,
    /// Segment path for running workspace hooks and task commands
    pub segment_path: &'a Path,
    /// Whether to kill processes running in the workspace
    pub kill: bool,
    /// Plugin manager for resolver-based task operations
    pub plugin_mgr: &'a crate::plugins::PluginManager,
}

/// Options for preparing a resume
pub struct ResumeOptions<'a> {
    /// Custom instruction/prompt for the resumed work
    pub instruction: Option<&'a str>,
    /// Segment path for running workspace hooks and task commands
    pub segment_path: &'a Path,
    /// Segment name
    pub segment_name: &'a str,
    /// Plugin manager for resolver-based task operations
    pub plugin_mgr: &'a crate::plugins::PluginManager,
}

/// Result from preparing a resume
pub struct ResumeResult {
    /// The prompt to use for the resumed session
    pub prompt: String,
    /// Whether the workspace was recreated
    pub workspace_recreated: bool,
    /// Setup result (if workspace was recreated)
    pub setup_result: SetupResult,
}

/// Options for cleaning an assignment (bead-free workspace teardown)
pub struct CleanOptions<'a> {
    /// Whether to push changes before cleanup
    pub push: bool,
    /// Segment path for running workspace hooks
    pub segment_path: &'a Path,
    /// Whether to kill processes running in the workspace
    pub kill: bool,
    /// Auto-commit message (rendered template). If Some, auto-commit before capture.
    pub auto_commit_message: Option<String>,
}

/// JSON-serializable result from cleaning an assignment
#[derive(Debug, Serialize)]
pub struct CleanResult {
    /// Workspace name (e.g., "three")
    pub workspace: String,
    /// External ID if present (e.g., bead ID)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// VCS revision hash captured before cleanup
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// Segment name
    pub segment: String,
}

/// Default auto-commit message template.
pub const DEFAULT_AUTO_COMMIT_MESSAGE: &str = "{{ task.id }}: {{ task.title }}";

/// Render the auto-commit message template for an assignment.
///
/// Uses the provided template string, rendered with task context (task.id, task.title).
/// Workspace and repo fields are populated from the assignment when available.
pub fn render_auto_commit_message(
    template: &str,
    assignment: &Assignment,
    segment_name: &str,
    segment_path: &std::path::Path,
) -> Option<String> {
    let task_id = assignment.task_id.clone().unwrap_or_default();
    let task_title = assignment
        .task_title
        .clone()
        .unwrap_or_else(|| task_id.clone());
    let ws_name = assignment
        .workspace_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    let ancillary_num = crate::ancillary_number(&assignment.ancillary_id).unwrap_or(0);
    let ctx = WorkspaceContext {
        ws: WorkspaceInfo {
            name: ws_name,
            num: ancillary_num,
            path: assignment.workspace_path.display().to_string(),
        },
        repo: RepoInfo {
            root: segment_path.display().to_string(),
            name: segment_name.to_string(),
        },
        task: Some(TaskInfo {
            id: task_id,
            title: task_title,
            description: None,
            url: assignment.task_url.clone(),
            source: assignment.task_source.clone(),
        }),
        vars: std::collections::HashMap::new(),
    };
    crate::workspace_setup::render_template(template, &ctx).ok()
}

/// Complete an assignment: auto-commit, capture revision, optionally push,
/// capture workspace info, cleanup workspace, close bead, and remove from storage.
///
/// This mirrors `breq complete` behavior.
pub fn complete_assignment(
    assignment: &Assignment,
    assignment_mgr: &mut AssignmentManager,
    ws_mgr: &WorkspaceManager,
    opts: &CompleteOptions,
) -> Result<CompleteResult> {
    let mut result = CompleteResult {
        revision: None,
        pushed: false,
        workspace_info: Vec::new(),
    };

    if assignment.workspace_path.exists() {
        // Auto-commit if message provided
        if let Some(ref message) = opts.auto_commit_message {
            match ws_mgr.auto_commit(opts.segment_path, &assignment.workspace_path, message) {
                Ok(committed) => {
                    if committed {
                        info!("Auto-committed changes for assignment {}", assignment.id);
                    }
                }
                Err(e) => {
                    tracing::warn!("Auto-commit failed: {}", e);
                    // Continue — don't fail the complete over an auto-commit failure
                }
            }
        }

        // Capture revision
        result.revision =
            ws_mgr.capture_revision(opts.segment_path, &assignment.workspace_path);

        // Capture workspace info (commit list) before cleanup
        result.workspace_info = ws_mgr
            .workspace_info(
                opts.segment_path,
                &assignment.workspace_path,
                assignment.base_branch.as_deref(),
            )
            .unwrap_or_default();

        // Push if requested
        if opts.push && result.revision.is_some() {
            info!("Pushing changes for assignment {}", assignment.id);
            ws_mgr.push(opts.segment_path, &assignment.workspace_path)?;
            result.pushed = true;
        }
    }

    // Cleanup workspace if it exists
    let cleanup_mode = CleanupMode::Complete {
        pushed: result.pushed,
    };
    cleanup_workspace(
        assignment,
        ws_mgr,
        opts.segment_path,
        opts.kill,
        cleanup_mode,
    )?;

    // Record completion history and remove assignment from active storage
    assignment_mgr.record_completion(
        assignment,
        CompletionReason::Completed,
        result.revision.clone(),
    )?;
    assignment_mgr.remove(&assignment.id)?;

    // Close task unless keep_task_open (only if task_id is present)
    if !opts.keep_task_open {
        if let Some(ref task_id) = assignment.task_id {
            let source = assignment.task_source.as_deref().unwrap_or("beads");
            let ctx = crate::PluginContext::new(
                Some(opts.segment_path.to_path_buf()),
                None,
            );
            opts.plugin_mgr.resolve_complete(source, task_id, ctx)?;
            info!("Task {} closed", task_id);
        }
    }

    Ok(result)
}

/// Abort an assignment: cleanup workspace, remove assignment, and handle bead status.
///
/// This mirrors `breq abort` behavior.
pub fn abort_assignment(
    assignment: &Assignment,
    assignment_mgr: &mut AssignmentManager,
    ws_mgr: &WorkspaceManager,
    opts: &AbortOptions,
) -> Result<()> {
    // Cleanup workspace if it exists
    cleanup_workspace(
        assignment,
        ws_mgr,
        opts.segment_path,
        opts.kill,
        CleanupMode::Abort,
    )?;

    // Record abort history and remove assignment from active storage
    assignment_mgr.record_completion(assignment, CompletionReason::Aborted, None)?;
    assignment_mgr.remove(&assignment.id)?;

    // Handle task status (only if task_id is present)
    if let Some(ref task_id) = assignment.task_id {
        let source = assignment.task_source.as_deref().unwrap_or("beads");
        let ctx = crate::PluginContext::new(
            Some(opts.segment_path.to_path_buf()),
            None,
        );
        if opts.close_task {
            opts.plugin_mgr.resolve_complete(source, task_id, ctx)?;
            info!("Task {} closed", task_id);
        } else {
            opts.plugin_mgr.resolve_abort(source, task_id, ctx)?;
            info!("Task {} unassigned and returned to open", task_id);
        }
    }

    Ok(())
}

/// Prepare an assignment for resuming: recreate workspace if missing,
/// update status to Active, ensure bead is claimed.
///
/// This mirrors `breq resume` behavior, but doesn't exec into claude
/// (the caller decides how to start work).
pub fn prepare_resume(
    assignment: &Assignment,
    assignment_mgr: &mut AssignmentManager,
    ws_mgr: &WorkspaceManager,
    opts: &ResumeOptions,
) -> Result<ResumeResult> {
    let mut workspace_recreated = false;
    let mut setup_result = SetupResult;

    // Recreate workspace if missing
    if !assignment.workspace_path.exists() {
        info!(
            "Workspace missing for assignment {}, recreating...",
            assignment.id
        );
        let ws_name = assignment
            .workspace_path
            .file_name()
            .and_then(|n| n.to_str())
            .context("Invalid workspace path")?;
        let ancillary_num = crate::ancillary_number(&assignment.ancillary_id).unwrap_or(0);

        let (_ws_path, result) = ws_mgr.create_workspace_with_setup(
            opts.segment_path,
            opts.segment_name,
            ws_name,
            ancillary_num,
        )?;
        setup_result = result;
        workspace_recreated = true;
        info!("Workspace recreated: {}", assignment.workspace_path.display());
    }

    // Touch updated_at timestamp (assignment is always Active)
    assignment_mgr.touch(&assignment.id)?;

    // Ensure task is in_progress and assigned to claude (if task_id present)
    let task_title = if let Some(ref task_id) = assignment.task_id {
        let source = assignment.task_source.as_deref().unwrap_or("beads");
        let ctx = crate::PluginContext::new(
            Some(opts.segment_path.to_path_buf()),
            Some(opts.segment_name.to_string()),
        );
        match opts.plugin_mgr.resolve_fetch(source, task_id, ctx) {
            Ok(task) => task.title,
            Err(_) => {
                // Task might be closed or not found, try to reclaim
                let ctx = crate::PluginContext::new(
                    Some(opts.segment_path.to_path_buf()),
                    Some(opts.segment_name.to_string()),
                );
                opts.plugin_mgr.resolve_claim(source, task_id, "claude", ctx)?;
                assignment
                    .task_title
                    .clone()
                    .unwrap_or_else(|| task_id.clone())
            }
        }
    } else {
        assignment.task_title.clone().unwrap_or_default()
    };

    let prompt = opts
        .instruction
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            if let Some(ref task_id) = assignment.task_id {
                format!(
                    "Continue working on {}: {}. Review progress and complete remaining work.",
                    task_id, task_title
                )
            } else {
                format!(
                    "Continue working on: {}. Review progress and complete remaining work.",
                    task_title
                )
            }
        });

    Ok(ResumeResult {
        prompt,
        workspace_recreated,
        setup_result,
    })
}

/// Clean an assignment: auto-commit, capture revision, push (if requested),
/// cleanup workspace, record completion, remove assignment.
///
/// This is the bead-free teardown — no bead status changes. Returns a
/// JSON-serializable result for structured output.
pub fn clean_assignment(
    assignment: &Assignment,
    assignment_mgr: &mut AssignmentManager,
    ws_mgr: &WorkspaceManager,
    opts: &CleanOptions,
) -> Result<CleanResult> {
    let ws_name = assignment
        .workspace_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    let mut revision = None;

    if assignment.workspace_path.exists() {
        // Auto-commit if message provided
        if let Some(ref message) = opts.auto_commit_message {
            match ws_mgr.auto_commit(opts.segment_path, &assignment.workspace_path, message) {
                Ok(committed) => {
                    if committed {
                        info!("Auto-committed changes for assignment {}", assignment.id);
                    }
                }
                Err(e) => {
                    tracing::warn!("Auto-commit failed: {}", e);
                }
            }
        }

        // Capture revision
        revision = ws_mgr.capture_revision(opts.segment_path, &assignment.workspace_path);

        // Push if requested
        if opts.push && revision.is_some() {
            info!("Pushing changes for assignment {}", assignment.id);
            ws_mgr.push(opts.segment_path, &assignment.workspace_path)?;
        }
    }

    // Cleanup workspace
    let cleanup_mode = CleanupMode::Complete {
        pushed: opts.push && revision.is_some(),
    };
    cleanup_workspace(
        assignment,
        ws_mgr,
        opts.segment_path,
        opts.kill,
        cleanup_mode,
    )?;

    // Record completion and remove assignment
    assignment_mgr.record_completion(
        assignment,
        CompletionReason::Completed,
        revision.clone(),
    )?;
    assignment_mgr.remove(&assignment.id)?;

    Ok(CleanResult {
        workspace: ws_name,
        id: assignment.task_id.clone(),
        revision,
        segment: assignment.segment.clone(),
    })
}

/// Cleanup workspace for an assignment (process check + destroy hooks + VCS tracking removal + delete)
fn cleanup_workspace(
    assignment: &Assignment,
    ws_mgr: &WorkspaceManager,
    segment_path: &Path,
    kill: bool,
    mode: CleanupMode,
) -> Result<SetupResult> {
    if assignment.workspace_path.exists() {
        // Check for running processes before cleanup
        let processes = crate::process::find_workspace_processes(&assignment.workspace_path);
        if !processes.is_empty() {
            if kill {
                info!(
                    "Terminating {} process(es) in workspace",
                    processes.len()
                );
                crate::process::terminate_processes(
                    &processes,
                    std::time::Duration::from_secs(5),
                )?;
            } else {
                return Err(
                    crate::process::WorkspaceProcessesRunning { processes }.into()
                );
            }
        }

        let ws_name = assignment
            .workspace_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");

        let segment_name = crate::ancillary_segment(&assignment.ancillary_id)
            .unwrap_or_else(|| assignment.segment.clone());

        let result =
            ws_mgr.cleanup_workspace(segment_path, &segment_name, ws_name, mode)?;
        info!("Workspace cleaned up for assignment {}", assignment.id);
        Ok(result)
    } else {
        info!(
            "Workspace already gone for assignment {}",
            assignment.id
        );
        Ok(SetupResult)
    }
}
