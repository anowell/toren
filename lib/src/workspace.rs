use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{debug, info, warn};

/// Recursively remove a directory without following symlinks.
/// Symlinks themselves are removed, but their targets are not traversed.
fn remove_dir_all_no_follow(path: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        // file_type() on DirEntry uses lstat (doesn't follow symlinks)
        let ft = entry.file_type()?;
        if ft.is_symlink() || ft.is_file() {
            std::fs::remove_file(entry.path())?;
        } else if ft.is_dir() {
            remove_dir_all_no_follow(&entry.path())?;
        }
    }
    std::fs::remove_dir(path)
}

/// Spawn a background thread to delete all `.cleanup-*` directories under `parent`.
fn spawn_background_cleanup(parent: PathBuf) {
    std::thread::spawn(move || {
        let entries = match std::fs::read_dir(&parent) {
            Ok(entries) => entries,
            Err(e) => {
                warn!("Failed to read directory for cleanup {}: {}", parent.display(), e);
                return;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    warn!("Failed to read dir entry during cleanup: {}", e);
                    continue;
                }
            };
            let name = entry.file_name();
            if name.to_string_lossy().starts_with(".cleanup-") {
                let p = entry.path();
                info!("Background cleanup: removing {}", p.display());
                if let Err(e) = remove_dir_all_no_follow(&p) {
                    warn!("Background cleanup failed for {}: {}", p.display(), e);
                }
            }
        }
    });
}

use crate::config::ProxyConfig;
use crate::workspace_setup::{BreqConfig, ProxyDirective, SetupResult, WorkspaceSetup};

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

            // Sweep any stale .cleanup-* dirs from previously interrupted deletions
            spawn_background_cleanup(parent.to_path_buf());
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
            remove_dir_all_no_follow(&ws_path).with_context(|| {
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

    /// Delete a workspace directory (after forgetting).
    ///
    /// Renames the directory to a `.cleanup-*` sibling for near-instant return,
    /// then spawns a background thread to delete it (plus any stale `.cleanup-*`
    /// dirs left behind by previous interrupted cleanups).
    pub fn delete_workspace(&self, segment_name: &str, workspace_name: &str) -> Result<()> {
        let ws_path = self.workspace_path(segment_name, workspace_name);

        if ws_path.exists() {
            let parent = ws_path.parent().with_context(|| {
                format!("Workspace path has no parent: {}", ws_path.display())
            })?;

            let cleanup_name = format!(
                ".cleanup-{}-{}",
                workspace_name,
                std::process::id()
            );
            let cleanup_path = parent.join(&cleanup_name);

            info!(
                "Renaming workspace for background deletion: {} -> {}",
                ws_path.display(),
                cleanup_path.display()
            );
            std::fs::rename(&ws_path, &cleanup_path).with_context(|| {
                format!(
                    "Failed to rename workspace directory for cleanup: {}",
                    ws_path.display()
                )
            })?;

            spawn_background_cleanup(parent.to_path_buf());
        }

        Ok(())
    }

    /// Cleanup a workspace completely (destroy hooks + forget + delete)
    pub fn cleanup_workspace(
        &self,
        segment_path: &Path,
        segment_name: &str,
        workspace_name: &str,
        proxy_config: Option<&ProxyConfig>,
    ) -> Result<SetupResult> {
        let ws_path = self.workspace_path(segment_name, workspace_name);
        let mut result = SetupResult::default();

        // Run destroy hooks if workspace exists
        if ws_path.exists() {
            match self.run_destroy(segment_path, &ws_path, workspace_name, proxy_config) {
                Ok(destroy_result) => {
                    result = destroy_result;
                }
                Err(e) => {
                    warn!("Workspace destroy hooks failed: {}", e);
                    // Continue with cleanup even if destroy fails
                }
            }
        }

        self.forget_workspace(segment_path, workspace_name)?;
        self.delete_workspace(segment_name, workspace_name)?;
        Ok(result)
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

    /// Run workspace setup hooks if .toren.kdl exists
    /// Called automatically after workspace creation, but can also be invoked manually
    pub fn run_setup(
        &self,
        segment_path: &Path,
        workspace_path: &Path,
        workspace_name: &str,
        ancillary_num: u32,
        proxy_config: Option<&ProxyConfig>,
    ) -> Result<SetupResult> {
        if !BreqConfig::exists(segment_path) {
            debug!("No .toren.kdl found, skipping setup");
            return Ok(SetupResult::default());
        }

        let setup = WorkspaceSetup::new(
            segment_path.to_path_buf(),
            workspace_path.to_path_buf(),
            workspace_name.to_string(),
            ancillary_num,
        );

        setup.run_setup(proxy_config)
    }

    /// Run workspace destroy hooks if .toren.kdl exists
    /// Should be called before cleanup to allow cleanup scripts to run
    pub fn run_destroy(
        &self,
        segment_path: &Path,
        workspace_path: &Path,
        workspace_name: &str,
        proxy_config: Option<&ProxyConfig>,
    ) -> Result<SetupResult> {
        if !BreqConfig::exists(segment_path) {
            debug!("No .toren.kdl found, skipping destroy");
            return Ok(SetupResult::default());
        }

        let setup = WorkspaceSetup::new(
            segment_path.to_path_buf(),
            workspace_path.to_path_buf(),
            workspace_name.to_string(),
            0, // ancillary_num not available during destroy
        );

        setup.run_destroy(proxy_config)
    }

    /// Evaluate proxy directives from .toren.kdl for an existing workspace
    /// without running other setup actions. Used to refresh Caddy routes.
    pub fn evaluate_proxy_directives(
        &self,
        segment_path: &Path,
        segment_name: &str,
        workspace_name: &str,
        proxy_config: Option<&ProxyConfig>,
    ) -> Result<Vec<ProxyDirective>> {
        let ws_path = self.workspace_path(segment_name, workspace_name);
        let ancillary_num = crate::word_to_number(workspace_name).unwrap_or(0);

        let setup = WorkspaceSetup::new(
            segment_path.to_path_buf(),
            ws_path,
            workspace_name.to_string(),
            ancillary_num,
        );

        setup.evaluate_proxy_directives(proxy_config)
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
        ancillary_num: u32,
        proxy_config: Option<&ProxyConfig>,
    ) -> Result<(PathBuf, SetupResult)> {
        let ws_path = self.create_workspace(segment_path, segment_name, workspace_name)?;

        // Run setup hooks if .toren.kdl exists - fail if setup fails
        match self.run_setup(segment_path, &ws_path, workspace_name, ancillary_num, proxy_config) {
            Ok(setup_result) => Ok((ws_path, setup_result)),
            Err(e) => {
                // Rollback: forget + delete the partially-created workspace
                warn!(
                    "Setup failed for '{}', rolling back workspace: {}",
                    workspace_name, e
                );
                if let Err(rollback_err) =
                    self.cleanup_workspace(segment_path, segment_name, workspace_name, None)
                {
                    warn!("Rollback cleanup also failed: {}", rollback_err);
                }
                Err(e).with_context(|| {
                    format!(
                        "Workspace setup failed for '{}' (workspace rolled back)",
                        workspace_name
                    )
                })
            }
        }
    }
}
