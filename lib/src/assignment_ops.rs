//! Shared assignment lifecycle operations used by both breq CLI and toren daemon.
//!
//! These functions implement the complete/abort/resume logic so both interfaces
//! behave identically.

use anyhow::{Context, Result};
use std::path::Path;
use tracing::info;

use crate::assignment::{AssignmentManager, CompletionReason};
use crate::config::{AncillaryConfig, ProxyConfig};
use crate::workspace::{CleanupMode, CommitInfo, WorkspaceManager};
use crate::workspace_setup::{ProxyDirective, SetupResult, WorkspaceContext, WorkspaceInfo, RepoInfo, TaskInfo};
use crate::Assignment;

/// Options for completing an assignment
pub struct CompleteOptions<'a> {
    /// Whether to push changes
    pub push: bool,
    /// Whether to keep the bead open (default: close it)
    pub keep_open: bool,
    /// Segment path for running workspace hooks and bead commands
    pub segment_path: &'a Path,
    /// Proxy configuration (for destroy hooks)
    pub proxy_config: Option<&'a ProxyConfig>,
    /// Whether to kill processes running in the workspace
    pub kill: bool,
    /// Auto-commit message (rendered template). If Some, auto-commit before capture.
    pub auto_commit_message: Option<String>,
}

/// Result from completing an assignment
pub struct CompleteResult {
    /// The revision hash if captured before cleanup
    pub revision: Option<String>,
    /// Whether changes were pushed
    pub pushed: bool,
    /// Proxy directives from destroy hooks (to remove routes)
    pub destroy_directives: Vec<ProxyDirective>,
    /// Commits exclusive to this workspace (captured before cleanup)
    pub workspace_info: Vec<CommitInfo>,
}

/// Options for aborting an assignment
pub struct AbortOptions<'a> {
    /// Whether to close the bead (default: reopen it)
    pub close_bead: bool,
    /// Segment path for running workspace hooks and bead commands
    pub segment_path: &'a Path,
    /// Proxy configuration (for destroy hooks)
    pub proxy_config: Option<&'a ProxyConfig>,
    /// Whether to kill processes running in the workspace
    pub kill: bool,
}

/// Result from aborting an assignment
pub struct AbortResult {
    /// Proxy directives from destroy hooks (to remove routes)
    pub destroy_directives: Vec<ProxyDirective>,
}

/// Options for preparing a resume
pub struct ResumeOptions<'a> {
    /// Custom instruction/prompt for the resumed work
    pub instruction: Option<&'a str>,
    /// Segment path for running workspace hooks and bead commands
    pub segment_path: &'a Path,
    /// Segment name
    pub segment_name: &'a str,
    /// Proxy configuration (for setup hooks if workspace is recreated)
    pub proxy_config: Option<&'a ProxyConfig>,
}

/// Result from preparing a resume
pub struct ResumeResult {
    /// The prompt to use for the resumed session
    pub prompt: String,
    /// Whether the workspace was recreated
    pub workspace_recreated: bool,
    /// Proxy directives from setup hooks (if workspace was recreated)
    pub setup_result: SetupResult,
}

/// Render the auto-commit message template for an assignment.
///
/// Uses the `auto_commit_message` template from AncillaryConfig, rendered with
/// task context (task.id, task.title). Workspace and repo fields are populated
/// from the assignment when available.
pub fn render_auto_commit_message(
    ancillary_config: &AncillaryConfig,
    assignment: &Assignment,
    segment_name: &str,
    segment_path: &std::path::Path,
) -> Option<String> {
    let task_title = assignment
        .bead_title
        .clone()
        .unwrap_or_else(|| assignment.bead_id.clone());
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
            id: assignment.bead_id.clone(),
            title: task_title,
        }),
        vars: std::collections::HashMap::new(),
        config: None,
    };
    crate::workspace_setup::render_template(&ancillary_config.auto_commit_message, &ctx).ok()
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
        destroy_directives: Vec::new(),
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
        if opts.push {
            if result.revision.is_some() {
                info!("Pushing changes for assignment {}", assignment.id);
                ws_mgr.push(opts.segment_path, &assignment.workspace_path)?;
                result.pushed = true;
            }
        }
    }

    // Cleanup workspace if it exists
    let cleanup_mode = CleanupMode::Complete {
        pushed: result.pushed,
    };
    let destroy_result = cleanup_workspace(
        assignment,
        ws_mgr,
        opts.segment_path,
        opts.proxy_config,
        opts.kill,
        cleanup_mode,
    )?;
    result.destroy_directives = destroy_result.proxy_directives;

    // Record completion history and remove assignment from active storage
    assignment_mgr.record_completion(
        assignment,
        CompletionReason::Completed,
        result.revision.clone(),
    )?;
    assignment_mgr.remove(&assignment.id)?;

    // Close bead unless keep_open
    if !opts.keep_open {
        crate::tasks::beads::update_bead_status(&assignment.bead_id, "closed", opts.segment_path)?;
        info!("Bead {} closed", assignment.bead_id);
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
) -> Result<AbortResult> {
    // Cleanup workspace if it exists
    let destroy_result = cleanup_workspace(
        assignment,
        ws_mgr,
        opts.segment_path,
        opts.proxy_config,
        opts.kill,
        CleanupMode::Abort,
    )?;

    // Record abort history and remove assignment from active storage
    assignment_mgr.record_completion(assignment, CompletionReason::Aborted, None)?;
    assignment_mgr.remove(&assignment.id)?;

    // Handle bead status
    if opts.close_bead {
        crate::tasks::beads::update_bead_status(&assignment.bead_id, "closed", opts.segment_path)?;
        info!("Bead {} closed", assignment.bead_id);
    } else {
        // Unassign and reopen
        let _ =
            crate::tasks::beads::update_bead_assignee(&assignment.bead_id, "", opts.segment_path);
        crate::tasks::beads::update_bead_status(&assignment.bead_id, "open", opts.segment_path)?;
        info!(
            "Bead {} unassigned and returned to open",
            assignment.bead_id
        );
    }

    Ok(AbortResult {
        destroy_directives: destroy_result.proxy_directives,
    })
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
    let mut setup_result = SetupResult::default();

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
            opts.proxy_config,
        )?;
        setup_result = result;
        workspace_recreated = true;
        info!("Workspace recreated: {}", assignment.workspace_path.display());
    }

    // Touch updated_at timestamp (assignment is always Active)
    assignment_mgr.touch(&assignment.id)?;

    // Ensure bead is in_progress and assigned to claude
    let task_title = match crate::tasks::fetch_task(&assignment.bead_id, opts.segment_path) {
        Ok(task) => task.title,
        Err(_) => {
            // Bead might be closed or not found, try to reopen/reclaim
            crate::tasks::beads::claim_bead(&assignment.bead_id, "claude", opts.segment_path)?;
            assignment
                .bead_title
                .clone()
                .unwrap_or_else(|| assignment.bead_id.clone())
        }
    };

    let prompt = opts
        .instruction
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            format!(
                "Continue working on bead {}: {}. Review progress and complete remaining work.",
                assignment.bead_id, task_title
            )
        });

    Ok(ResumeResult {
        prompt,
        workspace_recreated,
        setup_result,
    })
}

/// Cleanup workspace for an assignment (process check + destroy hooks + VCS tracking removal + delete)
fn cleanup_workspace(
    assignment: &Assignment,
    ws_mgr: &WorkspaceManager,
    segment_path: &Path,
    proxy_config: Option<&ProxyConfig>,
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
            ws_mgr.cleanup_workspace(segment_path, &segment_name, ws_name, proxy_config, mode)?;
        info!("Workspace cleaned up for assignment {}", assignment.id);
        Ok(result)
    } else {
        info!(
            "Workspace already gone for assignment {}",
            assignment.id
        );
        Ok(SetupResult::default())
    }
}
