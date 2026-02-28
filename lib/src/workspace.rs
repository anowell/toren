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

use crate::workspace_setup::{BreqConfig, SetupResult, WorkspaceSetup};

/// Version control system type for a repository
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoType {
    Jj,
    Git,
}

/// Detect the repository type at a given path
pub fn detect_repo_type(path: &Path) -> Option<RepoType> {
    if path.join(".jj").exists() {
        Some(RepoType::Jj)
    } else if path.join(".git").exists() {
        Some(RepoType::Git)
    } else {
        None
    }
}

/// A commit in a workspace
#[derive(Debug, Clone)]
pub struct CommitInfo {
    /// Commit/change identifier (commit_id for git, change_id for jj)
    pub id: String,
    /// First line of commit message
    pub summary: String,
}

/// How a workspace cleanup was triggered
#[derive(Debug, Clone, Copy)]
pub enum CleanupMode {
    Complete { pushed: bool },
    Abort,
}

/// VCS-specific workspace operations.
///
/// Implemented by JjBackend and GitWorktreeBackend.
/// WorkspaceManager delegates to the appropriate backend based on segment repo type.
pub trait VcsBackend: Send + Sync {
    /// Repository type this backend handles
    fn repo_type(&self) -> RepoType;

    /// Create a VCS workspace at the given path
    fn create_workspace(
        &self,
        segment_path: &Path,
        workspace_path: &Path,
        workspace_name: &str,
    ) -> Result<()>;

    /// Remove VCS tracking for a workspace.
    /// For jj: workspace forget (commits persist in DAG).
    /// For git: worktree remove + conditionally delete branch based on mode.
    fn remove_vcs_tracking(
        &self,
        segment_path: &Path,
        workspace_path: &Path,
        workspace_name: &str,
        mode: CleanupMode,
    ) -> Result<()>;

    /// List VCS-tracked workspace names in a segment
    fn list_workspaces(&self, segment_path: &Path) -> Result<Vec<String>>;

    /// Check if a workspace directory is a valid VCS workspace
    fn is_valid_workspace(&self, workspace_path: &Path) -> bool;

    /// Check if a workspace name is tracked by VCS in this segment
    fn is_tracked(&self, segment_path: &Path, workspace_name: &str) -> bool;

    /// Get commits exclusive to this workspace
    fn workspace_info(
        &self,
        workspace_path: &Path,
        base_ref: Option<&str>,
    ) -> Result<Vec<CommitInfo>>;

    /// Push workspace changes to remote
    fn push(&self, workspace_path: &Path) -> Result<()>;

    /// Auto-commit changes if workspace has uncommitted work.
    /// Returns true if a commit was made.
    fn auto_commit(&self, workspace_path: &Path, message: &str) -> Result<bool>;

    /// Check if workspace has changes (committed or uncommitted) vs base
    fn has_changes(&self, workspace_path: &Path, base_ref: Option<&str>) -> bool;

    /// Capture the current revision/commit hash
    fn capture_revision(&self, workspace_path: &Path) -> Option<String>;

    /// Detect the active branch in a segment repo (for base_branch recording at assign time)
    fn active_branch(&self, segment_path: &Path) -> Option<String>;
}

// ==================== Jj Backend ====================

pub struct JjBackend;

impl VcsBackend for JjBackend {
    fn repo_type(&self) -> RepoType {
        RepoType::Jj
    }

    fn create_workspace(
        &self,
        segment_path: &Path,
        workspace_path: &Path,
        workspace_name: &str,
    ) -> Result<()> {
        info!(
            "Creating jj workspace '{}' at {} (from {})",
            workspace_name,
            workspace_path.display(),
            segment_path.display()
        );

        let output = Command::new("jj")
            .args(["workspace", "add", "--name", workspace_name])
            .arg(workspace_path)
            .current_dir(segment_path)
            .output()
            .with_context(|| "Failed to execute jj workspace add")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("jj workspace add failed: {}", stderr);
        }

        Ok(())
    }

    fn remove_vcs_tracking(
        &self,
        segment_path: &Path,
        _workspace_path: &Path,
        workspace_name: &str,
        _mode: CleanupMode,
    ) -> Result<()> {
        // jj: always forget (commits persist in DAG regardless of mode)
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

    fn list_workspaces(&self, segment_path: &Path) -> Result<Vec<String>> {
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

    fn is_valid_workspace(&self, workspace_path: &Path) -> bool {
        workspace_path.exists() && workspace_path.join(".jj").exists()
    }

    fn is_tracked(&self, segment_path: &Path, workspace_name: &str) -> bool {
        self.list_workspaces(segment_path)
            .unwrap_or_default()
            .iter()
            .any(|w| w == workspace_name)
    }

    fn workspace_info(
        &self,
        workspace_path: &Path,
        _base_ref: Option<&str>,
    ) -> Result<Vec<CommitInfo>> {
        let output = Command::new("jj")
            .args([
                "log",
                "-r",
                "::@ ~ ::default@ ~ empty()",
                "--no-graph",
                "-T",
                r#"change_id ++ " " ++ description.first_line() ++ "\n""#,
            ])
            .current_dir(workspace_path)
            .output()
            .with_context(|| "Failed to get jj workspace info")?;

        if !output.status.success() {
            return Ok(Vec::new());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let commits = stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let (id, summary) = line.split_once(' ').unwrap_or((line, ""));
                CommitInfo {
                    id: id.to_string(),
                    summary: summary.to_string(),
                }
            })
            .collect();

        Ok(commits)
    }

    fn push(&self, workspace_path: &Path) -> Result<()> {
        let rev = self
            .capture_revision(workspace_path)
            .context("No revision to push")?;

        info!(
            "Pushing jj changes (revision {})",
            &rev[..12.min(rev.len())]
        );
        let status = Command::new("jj")
            .args(["git", "push", "-c", &rev])
            .current_dir(workspace_path)
            .status()?;

        if !status.success() {
            anyhow::bail!("jj git push failed");
        }

        Ok(())
    }

    fn auto_commit(&self, workspace_path: &Path, message: &str) -> Result<bool> {
        // Check if jj working commit is empty
        let diff_output = Command::new("jj")
            .args(["diff", "--stat"])
            .current_dir(workspace_path)
            .output()
            .ok();

        let is_empty = diff_output
            .as_ref()
            .map(|o| {
                o.status.success() && String::from_utf8_lossy(&o.stdout).trim().is_empty()
            })
            .unwrap_or(true);

        if is_empty {
            debug!("jj working commit is empty, skipping auto-commit");
            return Ok(false);
        }

        info!("Auto-committing jj changes");
        let status = Command::new("jj")
            .args(["commit", "-m", message])
            .current_dir(workspace_path)
            .status()?;

        if !status.success() {
            anyhow::bail!("jj commit failed");
        }

        Ok(true)
    }

    fn has_changes(&self, workspace_path: &Path, _base_ref: Option<&str>) -> bool {
        if !workspace_path.exists() {
            return false;
        }

        // Check 1: non-empty commits exclusive to this workspace
        let log_output = Command::new("jj")
            .args([
                "log",
                "-r",
                "::@ ~ ::default@ ~ empty()",
                "--no-graph",
                "-T",
                r#"change_id ++ "\n""#,
            ])
            .current_dir(workspace_path)
            .output()
            .ok();

        if let Some(output) = log_output {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if !stdout.trim().is_empty() {
                    return true;
                }
            }
        }

        // Check 2: uncommitted working-copy changes
        let diff_output = Command::new("jj")
            .args(["diff", "--stat"])
            .current_dir(workspace_path)
            .output()
            .ok();

        if let Some(output) = diff_output {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if !stdout.trim().is_empty() {
                    return true;
                }
            }
        }

        false
    }

    fn capture_revision(&self, workspace_path: &Path) -> Option<String> {
        let output = Command::new("jj")
            .args(["log", "-r", "@", "--no-graph", "-T", "commit_id"])
            .current_dir(workspace_path)
            .output()
            .ok()?;

        if output.status.success() {
            let rev = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !rev.is_empty() {
                return Some(rev);
            }
        }
        None
    }

    fn active_branch(&self, _segment_path: &Path) -> Option<String> {
        // jj doesn't have a "current branch" — the default workspace is the reference
        None
    }
}

// ==================== Git Worktree Backend ====================

pub struct GitWorktreeBackend;

impl GitWorktreeBackend {
    /// Get the current branch name in a workspace
    fn current_branch(&self, workspace_path: &Path) -> Option<String> {
        let output = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(workspace_path)
            .output()
            .ok()?;

        if output.status.success() {
            let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !branch.is_empty() && branch != "HEAD" {
                return Some(branch);
            }
        }
        None
    }

    /// Check if a branch has commits beyond its fork point from the main worktree
    fn branch_has_commits(&self, segment_path: &Path, branch_name: &str) -> bool {
        // Compare branch tip to where it diverged from the current HEAD of the main worktree
        let output = Command::new("git")
            .args(["log", "--oneline", &format!("HEAD..{}", branch_name)])
            .current_dir(segment_path)
            .output()
            .ok();

        if let Some(o) = output {
            if o.status.success() {
                return !String::from_utf8_lossy(&o.stdout).trim().is_empty();
            }
        }

        // If we can't tell, assume it has commits (safe default: don't delete)
        true
    }

    /// Check if a branch exists
    fn branch_exists(&self, segment_path: &Path, branch_name: &str) -> bool {
        Command::new("git")
            .args([
                "rev-parse",
                "--verify",
                &format!("refs/heads/{}", branch_name),
            ])
            .current_dir(segment_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

impl VcsBackend for GitWorktreeBackend {
    fn repo_type(&self) -> RepoType {
        RepoType::Git
    }

    fn create_workspace(
        &self,
        segment_path: &Path,
        workspace_path: &Path,
        workspace_name: &str,
    ) -> Result<()> {
        info!(
            "Creating git worktree '{}' at {} (from {})",
            workspace_name,
            workspace_path.display(),
            segment_path.display()
        );

        if self.branch_exists(segment_path, workspace_name) {
            // Attach to existing branch
            let output = Command::new("git")
                .args(["worktree", "add"])
                .arg(workspace_path)
                .arg(workspace_name)
                .current_dir(segment_path)
                .output()
                .with_context(|| "Failed to execute git worktree add")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("git worktree add (existing branch) failed: {}", stderr);
            }
        } else {
            // Create new branch from current HEAD
            let output = Command::new("git")
                .args(["worktree", "add", "-b", workspace_name])
                .arg(workspace_path)
                .current_dir(segment_path)
                .output()
                .with_context(|| "Failed to execute git worktree add")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("git worktree add failed: {}", stderr);
            }
        }

        Ok(())
    }

    fn remove_vcs_tracking(
        &self,
        segment_path: &Path,
        workspace_path: &Path,
        workspace_name: &str,
        mode: CleanupMode,
    ) -> Result<()> {
        // Remove the worktree from git tracking
        if workspace_path.exists() {
            let output = Command::new("git")
                .args(["worktree", "remove", "--force"])
                .arg(workspace_path)
                .current_dir(segment_path)
                .output();

            match output {
                Ok(o) if !o.status.success() => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    warn!("git worktree remove failed: {}", stderr);
                    // Directory will be cleaned up by delete_workspace fallback
                }
                Err(e) => warn!("Failed to run git worktree remove: {}", e),
                _ => {}
            }
        } else {
            // Worktree directory already gone, prune stale entries
            let _ = Command::new("git")
                .args(["worktree", "prune"])
                .current_dir(segment_path)
                .output();
        }

        // Decide whether to delete the branch
        let should_delete_branch = match mode {
            CleanupMode::Complete { pushed } => pushed,
            CleanupMode::Abort => {
                // Delete branch only if it has no commits beyond base
                !self.branch_has_commits(segment_path, workspace_name)
            }
        };

        if should_delete_branch {
            info!("Deleting git branch '{}'", workspace_name);
            let output = Command::new("git")
                .args(["branch", "-D", workspace_name])
                .current_dir(segment_path)
                .output();

            match output {
                Ok(o) if !o.status.success() => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    warn!("git branch -D '{}' failed: {}", workspace_name, stderr);
                }
                Err(e) => warn!("Failed to run git branch -D: {}", e),
                _ => {}
            }
        } else {
            debug!("Leaving git branch '{}' intact", workspace_name);
        }

        Ok(())
    }

    fn list_workspaces(&self, segment_path: &Path) -> Result<Vec<String>> {
        let output = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(segment_path)
            .output()
            .with_context(|| "Failed to execute git worktree list")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git worktree list failed: {}", stderr);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut workspaces = Vec::new();

        // Parse porcelain format — skip the main worktree (same path as segment_path)
        let segment_canonical = segment_path
            .canonicalize()
            .unwrap_or_else(|_| segment_path.to_path_buf());

        for block in stdout.split("\n\n") {
            let mut worktree_path = None;
            let mut branch = None;

            for line in block.lines() {
                if let Some(path) = line.strip_prefix("worktree ") {
                    worktree_path = Some(PathBuf::from(path));
                }
                if let Some(ref_name) = line.strip_prefix("branch ") {
                    // "refs/heads/one" -> "one"
                    branch = ref_name
                        .strip_prefix("refs/heads/")
                        .map(|s| s.to_string());
                }
            }

            // Skip the main worktree
            if let Some(ref wt_path) = worktree_path {
                let wt_canonical = wt_path
                    .canonicalize()
                    .unwrap_or_else(|_| wt_path.clone());
                if wt_canonical == segment_canonical {
                    continue;
                }
            }

            if let Some(branch_name) = branch {
                workspaces.push(branch_name);
            }
        }

        Ok(workspaces)
    }

    fn is_valid_workspace(&self, workspace_path: &Path) -> bool {
        // Git worktrees have a .git file (not directory) pointing to main repo
        workspace_path.exists() && workspace_path.join(".git").exists()
    }

    fn is_tracked(&self, segment_path: &Path, workspace_name: &str) -> bool {
        self.list_workspaces(segment_path)
            .unwrap_or_default()
            .iter()
            .any(|w| w == workspace_name)
    }

    fn workspace_info(
        &self,
        workspace_path: &Path,
        base_ref: Option<&str>,
    ) -> Result<Vec<CommitInfo>> {
        let base = base_ref.unwrap_or("main");
        let range = format!("{}..HEAD", base);

        let output = Command::new("git")
            .args(["log", &range, "--format=%H %s"])
            .current_dir(workspace_path)
            .output()
            .with_context(|| "Failed to get git workspace info")?;

        if !output.status.success() {
            return Ok(Vec::new());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let commits = stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let (id, summary) = line.split_once(' ').unwrap_or((line, ""));
                CommitInfo {
                    id: id.to_string(),
                    summary: summary.to_string(),
                }
            })
            .collect();

        Ok(commits)
    }

    fn push(&self, workspace_path: &Path) -> Result<()> {
        let branch = self
            .current_branch(workspace_path)
            .context("Could not determine current branch")?;

        info!("Pushing git branch '{}'", branch);
        let status = Command::new("git")
            .args(["push", "origin", &branch])
            .current_dir(workspace_path)
            .status()?;

        if !status.success() {
            anyhow::bail!("git push origin {} failed", branch);
        }

        Ok(())
    }

    fn auto_commit(&self, workspace_path: &Path, message: &str) -> Result<bool> {
        // Check if git working copy has changes
        let status_output = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(workspace_path)
            .output()
            .ok();

        let is_clean = status_output
            .as_ref()
            .map(|o| {
                o.status.success() && String::from_utf8_lossy(&o.stdout).trim().is_empty()
            })
            .unwrap_or(true);

        if is_clean {
            debug!("git working copy is clean, skipping auto-commit");
            return Ok(false);
        }

        info!("Auto-committing git changes");

        // Stage all changes
        let status = Command::new("git")
            .args(["add", "-A"])
            .current_dir(workspace_path)
            .status()?;

        if !status.success() {
            anyhow::bail!("git add failed");
        }

        // Commit
        let status = Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(workspace_path)
            .status()?;

        if !status.success() {
            anyhow::bail!("git commit failed");
        }

        Ok(true)
    }

    fn has_changes(&self, workspace_path: &Path, base_ref: Option<&str>) -> bool {
        if !workspace_path.exists() {
            return false;
        }

        let base = base_ref.unwrap_or("main");

        // Check 1: commits beyond base
        let log_range = format!("{}..HEAD", base);
        let log_output = Command::new("git")
            .args(["log", &log_range, "--oneline"])
            .current_dir(workspace_path)
            .output()
            .ok();

        if let Some(output) = log_output {
            if output.status.success()
                && !String::from_utf8_lossy(&output.stdout).trim().is_empty()
            {
                return true;
            }
        }

        // Check 2: uncommitted working-copy changes
        let diff_output = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(workspace_path)
            .output()
            .ok();

        if let Some(output) = diff_output {
            if output.status.success()
                && !String::from_utf8_lossy(&output.stdout).trim().is_empty()
            {
                return true;
            }
        }

        false
    }

    fn capture_revision(&self, workspace_path: &Path) -> Option<String> {
        let output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(workspace_path)
            .output()
            .ok()?;

        if output.status.success() {
            let rev = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !rev.is_empty() {
                return Some(rev);
            }
        }
        None
    }

    fn active_branch(&self, segment_path: &Path) -> Option<String> {
        let output = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(segment_path)
            .output()
            .ok()?;

        if output.status.success() {
            let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !branch.is_empty() && branch != "HEAD" {
                return Some(branch);
            }
        }
        None
    }
}

// ==================== Workspace Manager ====================

/// Manages workspaces for ancillaries, delegating VCS-specific operations
/// to the appropriate backend (jj or git) based on segment repo type.
pub struct WorkspaceManager {
    workspace_root: PathBuf,
    local_domain: Option<String>,
}

impl WorkspaceManager {
    pub fn new(workspace_root: PathBuf, local_domain: Option<String>) -> Self {
        // Make workspace_root absolute if it's relative
        let workspace_root = if workspace_root.is_absolute() {
            workspace_root
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(&workspace_root)
        };
        Self { workspace_root, local_domain }
    }

    /// Get the VCS backend for a segment based on repo type detection
    fn backend_for(&self, segment_path: &Path) -> Box<dyn VcsBackend> {
        match detect_repo_type(segment_path) {
            Some(RepoType::Git) => Box::new(GitWorktreeBackend),
            Some(RepoType::Jj) | None => Box::new(JjBackend),
        }
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

    /// Detect the repo type for a segment
    pub fn repo_type(&self, segment_path: &Path) -> Option<RepoType> {
        detect_repo_type(segment_path)
    }

    /// Detect the active branch in a segment repo (for recording base_branch at assign time)
    pub fn active_branch(&self, segment_path: &Path) -> Option<String> {
        self.backend_for(segment_path).active_branch(segment_path)
    }

    /// Get workspace info (commits exclusive to this workspace)
    pub fn workspace_info(
        &self,
        segment_path: &Path,
        workspace_path: &Path,
        base_ref: Option<&str>,
    ) -> Result<Vec<CommitInfo>> {
        self.backend_for(segment_path)
            .workspace_info(workspace_path, base_ref)
    }

    /// Check if workspace has changes (committed or uncommitted) vs base
    pub fn has_changes(
        &self,
        segment_path: &Path,
        workspace_path: &Path,
        base_ref: Option<&str>,
    ) -> bool {
        self.backend_for(segment_path)
            .has_changes(workspace_path, base_ref)
    }

    /// Push workspace changes to remote
    pub fn push(&self, segment_path: &Path, workspace_path: &Path) -> Result<()> {
        self.backend_for(segment_path).push(workspace_path)
    }

    /// Auto-commit changes if workspace has uncommitted work.
    /// Returns true if a commit was made.
    pub fn auto_commit(
        &self,
        segment_path: &Path,
        workspace_path: &Path,
        message: &str,
    ) -> Result<bool> {
        self.backend_for(segment_path)
            .auto_commit(workspace_path, message)
    }

    /// Capture the current revision/commit hash
    pub fn capture_revision(
        &self,
        segment_path: &Path,
        workspace_path: &Path,
    ) -> Option<String> {
        self.backend_for(segment_path)
            .capture_revision(workspace_path)
    }

    /// Create a new workspace for a segment.
    /// Returns the path to the workspace directory.
    ///
    /// If the directory exists but is not tracked by VCS (orphaned from a previous
    /// cleanup), it is removed before creating the new workspace.
    pub fn create_workspace(
        &self,
        segment_path: &Path,
        segment_name: &str,
        workspace_name: &str,
    ) -> Result<PathBuf> {
        let ws_path = self.workspace_path(segment_name, workspace_name);
        let backend = self.backend_for(segment_path);

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

            // Check if VCS still tracks this workspace
            let is_tracked = backend.is_tracked(segment_path, workspace_name);

            if is_tracked && backend.is_valid_workspace(&ws_path) {
                // Valid, tracked workspace - reuse it
                return Ok(ws_path);
            }

            // Orphaned directory: exists on disk but not tracked by VCS.
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

        // Create VCS workspace
        backend.create_workspace(segment_path, &ws_path, workspace_name)?;

        info!("Created workspace at {}", ws_path.display());
        Ok(ws_path)
    }

    /// Delete a workspace directory (after VCS tracking is removed).
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

    /// Cleanup a workspace completely (destroy hooks + VCS tracking removal + delete)
    pub fn cleanup_workspace(
        &self,
        segment_path: &Path,
        segment_name: &str,
        workspace_name: &str,
        mode: CleanupMode,
    ) -> Result<SetupResult> {
        let ws_path = self.workspace_path(segment_name, workspace_name);

        // Run destroy hooks if workspace exists
        if ws_path.exists() {
            if let Err(e) = self.run_destroy(segment_path, &ws_path, workspace_name) {
                warn!("Workspace destroy hooks failed: {}", e);
                // Continue with cleanup even if destroy fails
            }
        }

        // Remove VCS tracking (backend-specific behavior based on mode)
        let backend = self.backend_for(segment_path);
        backend.remove_vcs_tracking(segment_path, &ws_path, workspace_name, mode)?;

        // Delete workspace directory (if VCS removal didn't already do it)
        self.delete_workspace(segment_name, workspace_name)?;

        Ok(SetupResult)
    }

    /// List workspaces for a segment
    pub fn list_workspaces(&self, segment_path: &Path) -> Result<Vec<String>> {
        self.backend_for(segment_path).list_workspaces(segment_path)
    }

    /// Check if a workspace exists for a segment
    pub fn workspace_exists(&self, segment_name: &str, workspace_name: &str) -> bool {
        let ws_path = self.workspace_path(segment_name, workspace_name);
        ws_path.exists() && (ws_path.join(".jj").exists() || ws_path.join(".git").exists())
    }

    /// Run workspace setup hooks if .toren.kdl exists
    pub fn run_setup(
        &self,
        segment_path: &Path,
        workspace_path: &Path,
        workspace_name: &str,
        ancillary_num: u32,
    ) -> Result<SetupResult> {
        if !BreqConfig::exists(segment_path) {
            debug!("No .toren.kdl found, skipping setup");
            return Ok(SetupResult);
        }

        let setup = WorkspaceSetup::new(
            segment_path.to_path_buf(),
            workspace_path.to_path_buf(),
            workspace_name.to_string(),
            ancillary_num,
            self.local_domain.clone(),
        );

        setup.run_setup()
    }

    /// Run workspace destroy hooks if .toren.kdl exists
    pub fn run_destroy(
        &self,
        segment_path: &Path,
        workspace_path: &Path,
        workspace_name: &str,
    ) -> Result<SetupResult> {
        if !BreqConfig::exists(segment_path) {
            debug!("No .toren.kdl found, skipping destroy");
            return Ok(SetupResult);
        }

        let setup = WorkspaceSetup::new(
            segment_path.to_path_buf(),
            workspace_path.to_path_buf(),
            workspace_name.to_string(),
            0, // ancillary_num not available during destroy
            self.local_domain.clone(),
        );

        setup.run_destroy()
    }

    /// Create workspace and run setup hooks.
    /// If setup fails, the workspace is rolled back.
    pub fn create_workspace_with_setup(
        &self,
        segment_path: &Path,
        segment_name: &str,
        workspace_name: &str,
        ancillary_num: u32,
    ) -> Result<(PathBuf, SetupResult)> {
        let ws_path = self.create_workspace(segment_path, segment_name, workspace_name)?;

        // Run setup hooks if .toren.kdl exists - fail if setup fails
        match self.run_setup(
            segment_path,
            &ws_path,
            workspace_name,
            ancillary_num,
        ) {
            Ok(setup_result) => Ok((ws_path, setup_result)),
            Err(e) => {
                // Rollback: remove VCS tracking + delete the partially-created workspace
                warn!(
                    "Setup failed for '{}', rolling back workspace: {}",
                    workspace_name, e
                );
                if let Err(rollback_err) = self.cleanup_workspace(
                    segment_path,
                    segment_name,
                    workspace_name,
                    CleanupMode::Abort,
                ) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_repo_type_nonexistent() {
        assert_eq!(detect_repo_type(std::path::Path::new("/nonexistent")), None);
    }

    #[test]
    fn test_jj_backend_repo_type() {
        let backend = JjBackend;
        assert_eq!(backend.repo_type(), RepoType::Jj);
    }

    #[test]
    fn test_git_backend_repo_type() {
        let backend = GitWorktreeBackend;
        assert_eq!(backend.repo_type(), RepoType::Git);
    }

    #[test]
    fn test_jj_backend_nonexistent_workspace() {
        let backend = JjBackend;
        assert!(!backend.is_valid_workspace(std::path::Path::new("/nonexistent")));
        assert!(!backend.has_changes(std::path::Path::new("/nonexistent"), None));
    }

    #[test]
    fn test_git_backend_nonexistent_workspace() {
        let backend = GitWorktreeBackend;
        assert!(!backend.is_valid_workspace(std::path::Path::new("/nonexistent")));
        assert!(!backend.has_changes(std::path::Path::new("/nonexistent"), None));
    }

    #[test]
    fn test_workspace_manager_path() {
        let mgr = WorkspaceManager::new(PathBuf::from("/tmp/workspaces"), None);
        assert_eq!(
            mgr.workspace_path("toren", "one"),
            PathBuf::from("/tmp/workspaces/toren/one")
        );
    }

    #[test]
    fn test_workspace_exists_nonexistent() {
        let mgr = WorkspaceManager::new(PathBuf::from("/tmp/nonexistent-ws-root"), None);
        assert!(!mgr.workspace_exists("toren", "one"));
    }

    #[test]
    fn test_git_worktree_list_parse() {
        // Test that we can create the backend and it handles nonexistent dirs gracefully
        let backend = GitWorktreeBackend;
        assert!(backend.list_workspaces(std::path::Path::new("/nonexistent")).is_err());
    }

    #[test]
    fn test_cleanup_mode_variants() {
        // Ensure CleanupMode variants can be constructed
        let _complete = CleanupMode::Complete { pushed: true };
        let _complete_no_push = CleanupMode::Complete { pushed: false };
        let _abort = CleanupMode::Abort;
    }

    #[test]
    fn test_git_create_workspace_integration() {
        // Integration test: create a real git repo, create a worktree, verify it works
        let tmp = tempfile::tempdir().unwrap();
        let repo_path = tmp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();

        // Init a git repo with an initial commit
        Command::new("git")
            .args(["init"])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        std::fs::write(repo_path.join("README.md"), "# Test").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&repo_path)
            .output()
            .unwrap();

        let backend = GitWorktreeBackend;

        // Verify active branch detection
        let branch = backend.active_branch(&repo_path);
        assert!(branch.is_some(), "Should detect active branch");

        // Create a worktree
        let ws_path = tmp.path().join("ws-one");
        backend
            .create_workspace(&repo_path, &ws_path, "one")
            .expect("Should create worktree");

        // Verify worktree exists and is valid
        assert!(backend.is_valid_workspace(&ws_path));
        assert!(backend.is_tracked(&repo_path, "one"));

        // List workspaces should include "one"
        let workspaces = backend.list_workspaces(&repo_path).unwrap();
        assert!(workspaces.contains(&"one".to_string()));

        // Initially no changes
        let active_branch = backend.active_branch(&repo_path).unwrap();
        assert!(!backend.has_changes(&ws_path, Some(&active_branch)));

        // Make a change
        std::fs::write(ws_path.join("test.txt"), "hello").unwrap();
        assert!(backend.has_changes(&ws_path, Some(&active_branch)));

        // Auto-commit
        let committed = backend
            .auto_commit(&ws_path, "test commit")
            .expect("Should auto-commit");
        assert!(committed);

        // Should still have changes (committed but not in base)
        assert!(backend.has_changes(&ws_path, Some(&active_branch)));

        // Capture revision
        let rev = backend.capture_revision(&ws_path);
        assert!(rev.is_some());

        // Workspace info should show our commit
        let info = backend
            .workspace_info(&ws_path, Some(&active_branch))
            .unwrap();
        assert!(!info.is_empty());
        assert_eq!(info[0].summary, "test commit");

        // Cleanup: remove worktree with abort mode (has commits, so branch preserved)
        backend
            .remove_vcs_tracking(&repo_path, &ws_path, "one", CleanupMode::Abort)
            .expect("Should remove worktree");

        // Branch should still exist (has commits)
        assert!(backend.branch_exists(&repo_path, "one"));

        // Create again and cleanup with complete+pushed (branch deleted)
        let ws_path2 = tmp.path().join("ws-two");
        backend
            .create_workspace(&repo_path, &ws_path2, "two")
            .expect("Should create worktree");
        backend
            .remove_vcs_tracking(
                &repo_path,
                &ws_path2,
                "two",
                CleanupMode::Complete { pushed: true },
            )
            .expect("Should remove worktree");
        assert!(!backend.branch_exists(&repo_path, "two"));
    }

    #[test]
    fn test_workspace_manager_git_integration() {
        // Integration test for WorkspaceManager with a git repo
        let tmp = tempfile::tempdir().unwrap();
        let repo_path = tmp.path().join("repo");
        let ws_root = tmp.path().join("workspaces");
        std::fs::create_dir_all(&repo_path).unwrap();
        std::fs::create_dir_all(&ws_root).unwrap();

        // Init a git repo
        Command::new("git")
            .args(["init"])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        std::fs::write(repo_path.join("README.md"), "# Test").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&repo_path)
            .output()
            .unwrap();

        let mgr = WorkspaceManager::new(ws_root, None);

        // Detect repo type
        assert_eq!(mgr.repo_type(&repo_path), Some(RepoType::Git));

        // Create workspace
        let ws_path = mgr
            .create_workspace(&repo_path, "repo", "one")
            .expect("Should create workspace");
        assert!(ws_path.exists());

        // List workspaces
        let workspaces = mgr.list_workspaces(&repo_path).unwrap();
        assert!(workspaces.contains(&"one".to_string()));

        // Cleanup
        mgr.cleanup_workspace(&repo_path, "repo", "one", CleanupMode::Abort)
            .expect("Should cleanup");
        assert!(!ws_path.exists());
    }
}
