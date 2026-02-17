use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{debug, info, warn};

use crate::workspace_setup::{BreqConfig, WorkspaceSetup};

/// Manages jujutsu workspaces for ancillaries
pub struct WorkspaceManager {
    workspace_root: PathBuf,
}

impl WorkspaceManager {
    pub fn new(workspace_root: PathBuf) -> Self {
        // Make workspace_root absolute if it's relative
        let workspace_root = if workspace_root.is_absolute() {
            workspace_root
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(&workspace_root)
        };
        Self { workspace_root }
    }

    /// Get the workspace root directory
    pub fn root(&self) -> &Path {
        &self.workspace_root
    }

    /// Get the workspace directory path for a given segment and workspace name
    /// Pattern: $workspace_root/$segment_name/$workspace_name
    pub fn workspace_path(&self, segment_name: &str, workspace_name: &str) -> PathBuf {
        self.workspace_root.join(segment_name).join(workspace_name)
    }

    /// Create a new jj workspace for a segment
    /// Returns the path to the workspace directory
    ///
    /// If the directory exists but is not tracked by jj (orphaned from a previous
    /// `jj workspace forget`), it is removed before creating the new workspace.
    pub fn create_workspace(
        &self,
        segment_path: &Path,
        segment_name: &str,
        workspace_name: &str,
    ) -> Result<PathBuf> {
        let ws_path = self.workspace_path(segment_name, workspace_name);

        // Ensure parent directory exists
        if let Some(parent) = ws_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create workspace parent directory: {}",
                    parent.display()
                )
            })?;
        }

        // Check if workspace already exists
        if ws_path.exists() {
            debug!("Workspace directory already exists: {}", ws_path.display());

            // Check if jj still tracks this workspace
            let jj_workspaces = self.list_workspaces(segment_path).unwrap_or_default();
            let is_tracked = jj_workspaces.iter().any(|w| w == workspace_name);

            if is_tracked && ws_path.join(".jj").exists() {
                // Valid, tracked workspace - reuse it
                return Ok(ws_path);
            }

            // Orphaned directory: exists on disk but not tracked by jj.
            // Remove it so we can create a fresh workspace.
            info!(
                "Removing orphaned workspace directory: {}",
                ws_path.display()
            );
            std::fs::remove_dir_all(&ws_path).with_context(|| {
                format!(
                    "Failed to remove orphaned workspace directory: {}",
                    ws_path.display()
                )
            })?;
        }

        // Create workspace using jj workspace add
        info!(
            "Creating jj workspace '{}' at {} (from {})",
            workspace_name,
            ws_path.display(),
            segment_path.display()
        );

        let output = Command::new("jj")
            .args(["workspace", "add", "--name", workspace_name])
            .arg(&ws_path)
            .current_dir(segment_path)
            .output()
            .with_context(|| "Failed to execute jj workspace add")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("jj workspace add failed: {}", stderr);
        }

        info!("Created workspace at {}", ws_path.display());
        Ok(ws_path)
    }

    /// Forget a workspace (removes from jj tracking but keeps files)
    pub fn forget_workspace(&self, segment_path: &Path, workspace_name: &str) -> Result<()> {
        info!(
            "Forgetting jj workspace '{}' in {}",
            workspace_name,
            segment_path.display()
        );

        let output = Command::new("jj")
            .args(["workspace", "forget", workspace_name])
            .current_dir(segment_path)
            .output()
            .with_context(|| "Failed to execute jj workspace forget")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("jj workspace forget failed: {}", stderr);
            // Don't fail - the workspace might already be forgotten
        }

        Ok(())
    }

    /// Delete a workspace directory (after forgetting)
    pub fn delete_workspace(&self, segment_name: &str, workspace_name: &str) -> Result<()> {
        let ws_path = self.workspace_path(segment_name, workspace_name);

        if ws_path.exists() {
            info!("Deleting workspace directory: {}", ws_path.display());
            std::fs::remove_dir_all(&ws_path).with_context(|| {
                format!(
                    "Failed to delete workspace directory: {}",
                    ws_path.display()
                )
            })?;
        }

        Ok(())
    }

    /// Cleanup a workspace completely (destroy hooks + forget + delete)
    pub fn cleanup_workspace(
        &self,
        segment_path: &Path,
        segment_name: &str,
        workspace_name: &str,
    ) -> Result<()> {
        let ws_path = self.workspace_path(segment_name, workspace_name);

        // Run destroy hooks if workspace exists
        if ws_path.exists() {
            if let Err(e) = self.run_destroy(segment_path, &ws_path, workspace_name) {
                warn!("Workspace destroy hooks failed: {}", e);
                // Continue with cleanup even if destroy fails
            }
        }

        self.forget_workspace(segment_path, workspace_name)?;
        self.delete_workspace(segment_name, workspace_name)?;
        Ok(())
    }

    /// List workspaces for a segment
    pub fn list_workspaces(&self, segment_path: &Path) -> Result<Vec<String>> {
        let output = Command::new("jj")
            .args(["workspace", "list"])
            .current_dir(segment_path)
            .output()
            .with_context(|| "Failed to execute jj workspace list")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("jj workspace list failed: {}", stderr);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let workspaces: Vec<String> = stdout
            .lines()
            .filter_map(|line| {
                // jj workspace list output format: "workspace_name: commit_id"
                line.split(':').next().map(|s| s.trim().to_string())
            })
            .collect();

        Ok(workspaces)
    }

    /// Check if a workspace exists for a segment
    pub fn workspace_exists(&self, segment_name: &str, workspace_name: &str) -> bool {
        let ws_path = self.workspace_path(segment_name, workspace_name);
        ws_path.exists() && ws_path.join(".jj").exists()
    }

    /// Run workspace setup hooks if .breq.kdl exists
    /// Called automatically after workspace creation, but can also be invoked manually
    pub fn run_setup(
        &self,
        segment_path: &Path,
        workspace_path: &Path,
        workspace_name: &str,
        ancillary_num: Option<u32>,
    ) -> Result<()> {
        if !BreqConfig::exists(segment_path) {
            debug!("No .breq.kdl found, skipping setup");
            return Ok(());
        }

        let setup = WorkspaceSetup::new(
            segment_path.to_path_buf(),
            workspace_path.to_path_buf(),
            workspace_name.to_string(),
            ancillary_num,
        );

        setup.run_setup()
    }

    /// Run workspace destroy hooks if .breq.kdl exists
    /// Should be called before cleanup to allow cleanup scripts to run
    pub fn run_destroy(
        &self,
        segment_path: &Path,
        workspace_path: &Path,
        workspace_name: &str,
    ) -> Result<()> {
        if !BreqConfig::exists(segment_path) {
            debug!("No .breq.kdl found, skipping destroy");
            return Ok(());
        }

        let setup = WorkspaceSetup::new(
            segment_path.to_path_buf(),
            workspace_path.to_path_buf(),
            workspace_name.to_string(),
            None, // ancillary_num not available during destroy
        );

        setup.run_destroy()
    }

    /// Create workspace and run setup hooks
    /// This is the recommended method for creating workspaces with full initialization.
    ///
    /// If setup fails, the workspace is rolled back (forgotten + deleted) so it
    /// doesn't become an orphaned directory that blocks future assignments.
    pub fn create_workspace_with_setup(
        &self,
        segment_path: &Path,
        segment_name: &str,
        workspace_name: &str,
        ancillary_num: Option<u32>,
    ) -> Result<PathBuf> {
        let ws_path = self.create_workspace(segment_path, segment_name, workspace_name)?;

        // Run setup hooks if .toren.kdl exists - fail if setup fails
        if let Err(e) = self.run_setup(segment_path, &ws_path, workspace_name, ancillary_num) {
            // Rollback: forget + delete the partially-created workspace
            warn!(
                "Setup failed for '{}', rolling back workspace: {}",
                workspace_name, e
            );
            if let Err(rollback_err) =
                self.cleanup_workspace(segment_path, segment_name, workspace_name)
            {
                warn!("Rollback cleanup also failed: {}", rollback_err);
            }
            return Err(e).with_context(|| {
                format!(
                    "Workspace setup failed for '{}' (workspace rolled back)",
                    workspace_name
                )
            });
        }

        Ok(ws_path)
    }
}
