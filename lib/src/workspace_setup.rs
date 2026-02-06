//! Workspace setup hooks for initializing and tearing down jj workspaces.
//!
//! This module implements a lightweight, procedural mechanism for workspace initialization
//! using `.toren.kdl` configuration files. It supports three primitive actions:
//! - `template`: Copy and render files with workspace context
//! - `copy`: Copy files verbatim
//! - `run`: Execute shell commands

use anyhow::{Context, Result};
use clonetree::Options as CloneOptions;
use kdl::{KdlDocument, KdlNode};
use minijinja::{context, Environment};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{debug, info, trace, warn};

const TOREN_CONFIG_FILE: &str = ".toren.kdl";

/// Derive default dest from src: if src is relative, use it as-is; if absolute, use basename.
fn default_dest(src: &str) -> String {
    let path = Path::new(src);
    if path.is_absolute() {
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(src)
            .to_string()
    } else {
        src.to_string()
    }
}

/// Workspace context available to templates
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceContext {
    pub ws: WorkspaceInfo,
    pub repo: RepoInfo,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceInfo {
    /// jj workspace name (e.g., "one", "two")
    pub name: String,
    /// Ancillary number (1 for "one", 2 for "two", etc.)
    pub num: Option<u32>,
    /// Workspace path
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepoInfo {
    /// Repository root path
    pub root: String,
    /// Repository name
    pub name: String,
}

/// An action to execute during setup or destroy
#[derive(Debug, Clone)]
pub enum Action {
    /// Copy and render a template with workspace context
    Template { src: String, dest: String },
    /// Copy a file or directory using CoW when available, with fallback to regular copy
    Copy { src: String, dest: String, from: Option<String> },
    /// Create a symlink for truly shared content
    Share { src: String, from: Option<String> },
    /// Execute a shell command
    Run { command: String, cwd: Option<String> },
}

/// Configuration parsed from .toren.kdl
#[derive(Debug, Default)]
pub struct BreqConfig {
    pub setup: Vec<Action>,
    pub destroy: Vec<Action>,
}

impl BreqConfig {
    /// Check if a .toren.kdl file exists in the given directory
    pub fn exists(repo_root: &Path) -> bool {
        repo_root.join(TOREN_CONFIG_FILE).exists()
    }

    /// Parse a .toren.kdl file from the repository root
    pub fn parse(repo_root: &Path) -> Result<Self> {
        let config_path = repo_root.join(TOREN_CONFIG_FILE);

        if !config_path.exists() {
            trace!("No {} found at {}", TOREN_CONFIG_FILE, config_path.display());
            return Ok(Self::default());
        }

        trace!("Found config file: {}", config_path.display());

        let content = fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read {}", config_path.display()))?;

        Self::parse_kdl(&content)
            .with_context(|| format!("Failed to parse {}", config_path.display()))
    }

    fn parse_kdl(content: &str) -> Result<Self> {
        let doc: KdlDocument = content.parse()?;
        let mut config = Self::default();

        for node in doc.nodes() {
            match node.name().value() {
                "setup" => {
                    config.setup = Self::parse_block(node)?;
                }
                "destroy" => {
                    config.destroy = Self::parse_block(node)?;
                }
                other => {
                    warn!("Unknown top-level block in .toren.kdl: {}", other);
                }
            }
        }

        Ok(config)
    }

    fn parse_block(node: &KdlNode) -> Result<Vec<Action>> {
        let mut actions = Vec::new();

        if let Some(children) = node.children() {
            for child in children.nodes() {
                let action = Self::parse_action(child)?;
                actions.push(action);
            }
        }

        Ok(actions)
    }

    fn parse_action(node: &KdlNode) -> Result<Action> {
        match node.name().value() {
            "template" => {
                let src = node
                    .get("src")
                    .and_then(|v| v.as_string())
                    .context("template requires src= attribute")?
                    .to_string();
                let dest = node
                    .get("dest")
                    .and_then(|v| v.as_string())
                    .context("template requires dest= attribute")?
                    .to_string();
                Ok(Action::Template { src, dest })
            }
            "copy" => {
                let src = node
                    .get("src")
                    .and_then(|v| v.as_string())
                    .context("copy requires src= attribute")?
                    .to_string();
                let dest = node
                    .get("dest")
                    .and_then(|v| v.as_string())
                    .map(|s| s.to_string());
                let from = node
                    .get("from")
                    .and_then(|v| v.as_string())
                    .map(|s| s.to_string());
                // dest defaults to src if relative, or basename of src if absolute
                let dest = dest.unwrap_or_else(|| default_dest(&src));
                Ok(Action::Copy { src, dest, from })
            }
            "share" => {
                let src = node
                    .get("src")
                    .and_then(|v| v.as_string())
                    .context("share requires src= attribute")?
                    .to_string();
                let from = node
                    .get("from")
                    .and_then(|v| v.as_string())
                    .map(|s| s.to_string());
                Ok(Action::Share { src, from })
            }
            "run" => {
                // run takes command as first argument: run "pnpm install"
                let command = node
                    .entries()
                    .first()
                    .and_then(|e| e.value().as_string())
                    .context("run requires a command argument")?
                    .to_string();
                let cwd = node
                    .get("cwd")
                    .and_then(|v| v.as_string())
                    .map(|s| s.to_string());
                Ok(Action::Run { command, cwd })
            }
            other => {
                anyhow::bail!("Unknown action type: {}", other);
            }
        }
    }
}

/// Manages workspace setup state and execution
pub struct WorkspaceSetup {
    /// Path to the repository root (where .toren.kdl lives)
    repo_root: PathBuf,
    /// Path to the workspace being set up
    workspace_path: PathBuf,
    /// Workspace name (jj workspace name)
    workspace_name: String,
    /// Ancillary number (if known)
    ancillary_num: Option<u32>,
}

impl WorkspaceSetup {
    pub fn new(
        repo_root: PathBuf,
        workspace_path: PathBuf,
        workspace_name: String,
        ancillary_num: Option<u32>,
    ) -> Self {
        Self {
            repo_root,
            workspace_path,
            workspace_name,
            ancillary_num,
        }
    }

    /// Build workspace context for template rendering
    fn build_context(&self) -> WorkspaceContext {
        let repo_name = self
            .repo_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        WorkspaceContext {
            ws: WorkspaceInfo {
                name: self.workspace_name.clone(),
                num: self.ancillary_num,
                path: self.workspace_path.display().to_string(),
            },
            repo: RepoInfo {
                root: self.repo_root.display().to_string(),
                name: repo_name,
            },
        }
    }

    /// Run the setup block
    pub fn run_setup(&self) -> Result<()> {
        let config = BreqConfig::parse(&self.repo_root)?;

        if config.setup.is_empty() {
            debug!("No setup actions defined");
            return Ok(());
        }

        info!(
            "Running workspace setup for '{}' in {}",
            self.workspace_name,
            self.workspace_path.display()
        );

        let ctx = self.build_context();
        self.execute_actions(&config.setup, &ctx)?;

        info!("Workspace setup complete");
        Ok(())
    }

    /// Run the destroy block
    pub fn run_destroy(&self) -> Result<()> {
        let config = BreqConfig::parse(&self.repo_root)?;

        if config.destroy.is_empty() {
            debug!("No destroy actions defined");
            return Ok(());
        }

        info!(
            "Running workspace destroy for '{}' in {}",
            self.workspace_name,
            self.workspace_path.display()
        );

        let ctx = self.build_context();
        self.execute_actions(&config.destroy, &ctx)?;

        info!("Workspace destroy complete");
        Ok(())
    }

    /// Execute a list of actions in order
    fn execute_actions(&self, actions: &[Action], ctx: &WorkspaceContext) -> Result<()> {
        for (i, action) in actions.iter().enumerate() {
            trace!("Executing action {}: {:?}", i + 1, action);
            self.execute_action(action, ctx)
                .with_context(|| format!("Action {} failed", i + 1))?;
        }
        Ok(())
    }

    fn execute_action(&self, action: &Action, ctx: &WorkspaceContext) -> Result<()> {
        match action {
            Action::Template { src, dest } => self.execute_template(src, dest, ctx),
            Action::Copy { src, dest, from } => self.execute_copy(src, dest, from.as_deref(), ctx),
            Action::Share { src, from } => self.execute_share(src, from.as_deref(), ctx),
            Action::Run { command, cwd } => self.execute_run(command, cwd.as_deref()),
        }
    }

    fn execute_template(&self, src: &str, dest: &str, ctx: &WorkspaceContext) -> Result<()> {
        // Source is relative to repo root (template files are versioned)
        let src_path = self.repo_root.join(src);
        // Dest is relative to workspace
        let dest_path = self.workspace_path.join(dest);

        let template_content = fs::read_to_string(&src_path)
            .with_context(|| format!("Failed to read template: {}", src_path.display()))?;

        let mut env = Environment::new();
        env.add_template("template", &template_content)?;

        let template = env.get_template("template")?;
        let rendered = template.render(context! {
            ws => ctx.ws,
            repo => ctx.repo,
        })?;

        // Ensure parent directory exists
        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent)?;
        }

        fs::write(&dest_path, rendered)
            .with_context(|| format!("Failed to write: {}", dest_path.display()))?;

        info!("  template: {} -> {}", src, dest);
        Ok(())
    }

    fn execute_copy(&self, src: &str, dest: &str, from: Option<&str>, ctx: &WorkspaceContext) -> Result<()> {
        // Resolve source: from attribute (with template rendering) or repo root
        let src_path = if let Some(from_template) = from {
            let rendered_from = self.render_string(from_template, ctx)?;
            PathBuf::from(rendered_from).join(src)
        } else {
            self.repo_root.join(src)
        };
        // Dest is relative to workspace
        let dest_path = self.workspace_path.join(dest);

        // Ensure parent directory exists
        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Use clonetree for CoW with automatic fallback
        clonetree::clone_tree(&src_path, &dest_path, &CloneOptions::new())
            .with_context(|| format!("Failed to copy {} to {}", src_path.display(), dest_path.display()))?;

        info!("  copy: {} -> {}", src_path.display(), dest);
        Ok(())
    }

    fn execute_share(&self, src: &str, from: Option<&str>, ctx: &WorkspaceContext) -> Result<()> {
        // Resolve source: from attribute (with template rendering) or repo root
        let src_path = if let Some(from_template) = from {
            let rendered_from = self.render_string(from_template, ctx)?;
            PathBuf::from(rendered_from).join(src)
        } else {
            PathBuf::from(&ctx.repo.root).join(src)
        };
        let dest_path = self.workspace_path.join(src);

        // Ensure parent directory exists
        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Create symlink
        #[cfg(unix)]
        std::os::unix::fs::symlink(&src_path, &dest_path)
            .with_context(|| format!("Failed to symlink {} -> {}", dest_path.display(), src_path.display()))?;

        #[cfg(windows)]
        {
            if src_path.is_dir() {
                std::os::windows::fs::symlink_dir(&src_path, &dest_path)
            } else {
                std::os::windows::fs::symlink_file(&src_path, &dest_path)
            }
            .with_context(|| format!("Failed to symlink {} -> {}", dest_path.display(), src_path.display()))?;
        }

        info!("  share: {} -> {}", dest_path.display(), src_path.display());
        Ok(())
    }

    /// Render a string template with workspace context
    fn render_string(&self, template: &str, ctx: &WorkspaceContext) -> Result<String> {
        let mut env = Environment::new();
        env.add_template("inline", template)?;
        let tmpl = env.get_template("inline")?;
        let rendered = tmpl.render(context! {
            ws => ctx.ws,
            repo => ctx.repo,
        })?;
        Ok(rendered)
    }

    fn execute_run(&self, command: &str, cwd: Option<&str>) -> Result<()> {
        // Resolve cwd: if provided, relative to workspace; otherwise workspace root
        let work_dir = match cwd {
            Some(dir) => self.workspace_path.join(dir),
            None => self.workspace_path.clone(),
        };

        if let Some(dir) = cwd {
            info!("  run: {} (in {})", command, dir);
        } else {
            info!("  run: {}", command);
        }

        let output = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&work_dir)
            .output()
            .with_context(|| format!("Failed to execute: {}", command))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            anyhow::bail!(
                "Command failed: {}\nstdout: {}\nstderr: {}",
                command,
                stdout,
                stderr
            );
        }

        // Print stdout if any
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !stdout.is_empty() {
            for line in stdout.lines() {
                debug!("    {}", line);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_kdl_basic() {
        let content = r#"
setup {
    template src=".env.breq" dest=".env"
    run "pnpm install"
}

destroy {
    run "rm -rf node_modules"
}
"#;

        let config = BreqConfig::parse_kdl(content).unwrap();

        assert_eq!(config.setup.len(), 2);
        assert_eq!(config.destroy.len(), 1);

        match &config.setup[0] {
            Action::Template { src, dest } => {
                assert_eq!(src, ".env.breq");
                assert_eq!(dest, ".env");
            }
            _ => panic!("Expected Template action"),
        }

        match &config.setup[1] {
            Action::Run { command, cwd } => {
                assert_eq!(command, "pnpm install");
                assert!(cwd.is_none());
            }
            _ => panic!("Expected Run action"),
        }
    }

    #[test]
    fn test_parse_kdl_copy() {
        let content = r#"
setup {
    copy src="config.example.json" dest="config.json"
}
"#;

        let config = BreqConfig::parse_kdl(content).unwrap();

        assert_eq!(config.setup.len(), 1);
        match &config.setup[0] {
            Action::Copy { src, dest, from } => {
                assert_eq!(src, "config.example.json");
                assert_eq!(dest, "config.json");
                assert!(from.is_none());
            }
            _ => panic!("Expected Copy action"),
        }
    }

    #[test]
    fn test_empty_config() {
        let content = "";
        let config = BreqConfig::parse_kdl(content).unwrap();

        assert!(config.setup.is_empty());
        assert!(config.destroy.is_empty());
    }

    #[test]
    fn test_parse_kdl_share() {
        let content = r#"
setup {
    share src="node_modules" from="{{ repo.root }}"
    share src=".pnpm-store"
}
"#;

        let config = BreqConfig::parse_kdl(content).unwrap();

        assert_eq!(config.setup.len(), 2);
        match &config.setup[0] {
            Action::Share { src, from } => {
                assert_eq!(src, "node_modules");
                assert_eq!(from.as_deref(), Some("{{ repo.root }}"));
            }
            _ => panic!("Expected Share action"),
        }
        match &config.setup[1] {
            Action::Share { src, from } => {
                assert_eq!(src, ".pnpm-store");
                assert!(from.is_none());
            }
            _ => panic!("Expected Share action"),
        }
    }

    #[test]
    fn test_parse_kdl_copy_with_from() {
        let content = r#"
setup {
    copy src="node_modules" from="{{ repo.root }}"
    copy src="config.json" dest="config.json"
}
"#;

        let config = BreqConfig::parse_kdl(content).unwrap();

        assert_eq!(config.setup.len(), 2);
        match &config.setup[0] {
            Action::Copy { src, dest, from } => {
                assert_eq!(src, "node_modules");
                assert_eq!(dest, "node_modules"); // dest defaults to src
                assert_eq!(from.as_deref(), Some("{{ repo.root }}"));
            }
            _ => panic!("Expected Copy action"),
        }
        match &config.setup[1] {
            Action::Copy { src, dest, from } => {
                assert_eq!(src, "config.json");
                assert_eq!(dest, "config.json");
                assert!(from.is_none());
            }
            _ => panic!("Expected Copy action"),
        }
    }

    #[test]
    fn test_parse_kdl_copy_absolute_src() {
        let content = r#"
setup {
    copy src="/some/path/to/node_modules"
    copy src="relative/path"
}
"#;

        let config = BreqConfig::parse_kdl(content).unwrap();

        assert_eq!(config.setup.len(), 2);
        match &config.setup[0] {
            Action::Copy { src, dest, from } => {
                assert_eq!(src, "/some/path/to/node_modules");
                assert_eq!(dest, "node_modules"); // basename for absolute paths
                assert!(from.is_none());
            }
            _ => panic!("Expected Copy action"),
        }
        match &config.setup[1] {
            Action::Copy { src, dest, from } => {
                assert_eq!(src, "relative/path");
                assert_eq!(dest, "relative/path"); // relative paths preserved
                assert!(from.is_none());
            }
            _ => panic!("Expected Copy action"),
        }
    }

    #[test]
    fn test_parse_kdl_run_with_cwd() {
        let content = r#"
setup {
    run "pnpm install" cwd="web"
    run "cargo build"
}
"#;

        let config = BreqConfig::parse_kdl(content).unwrap();

        assert_eq!(config.setup.len(), 2);
        match &config.setup[0] {
            Action::Run { command, cwd } => {
                assert_eq!(command, "pnpm install");
                assert_eq!(cwd.as_deref(), Some("web"));
            }
            _ => panic!("Expected Run action"),
        }
        match &config.setup[1] {
            Action::Run { command, cwd } => {
                assert_eq!(command, "cargo build");
                assert!(cwd.is_none());
            }
            _ => panic!("Expected Run action"),
        }
    }
}
