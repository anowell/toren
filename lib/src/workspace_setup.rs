//! Workspace setup hooks for initializing and tearing down jj workspaces.
//!
//! This module implements a lightweight, procedural mechanism for workspace initialization
//! using `toren.kdl` configuration files. It supports these primitive actions:
//! - `template`: Copy and render files with workspace context
//! - `copy`: Copy files verbatim
//! - `run`: Execute shell commands (auto-gets `STATION_DOMAIN` env var)
//! - `proxy`: Manage station reverse-proxy routes (auto-cleanup on destroy)

use anyhow::{Context, Result};
use clonetree::Options as CloneOptions;
use kdl::{KdlDocument, KdlNode};
use minijinja::{context, Environment};
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{debug, info, trace, warn};

const TOREN_CONFIG_FILE: &str = "toren.kdl";
const TOREN_CONFIG_FILE_HIDDEN: &str = ".toren.kdl";

/// Extract an i64 from a KdlValue (kdl 6.x uses i128 internally)
fn kdl_value_as_i64(val: &kdl::KdlValue) -> Option<i64> {
    val.as_integer().and_then(|n| i64::try_from(n).ok())
}

/// Render a template string with workspace context using minijinja.
/// Available variables: ws.name, ws.num, ws.path, repo.root, repo.name, task.id, task.title, vars.*
pub fn render_template(template: &str, ctx: &WorkspaceContext) -> Result<String> {
    let mut env = Environment::new();
    env.add_template("inline", template)?;
    let tmpl = env.get_template("inline")?;
    let rendered = tmpl.render(context! {
        ws => ctx.ws,
        repo => ctx.repo,
        task => ctx.task,
        vars => ctx.vars,
    })?;
    Ok(rendered)
}

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

/// Task context available to templates
#[derive(Debug, Clone, Serialize)]
pub struct TaskInfo {
    /// Task/bead ID (e.g., "breq-a1b2")
    pub id: String,
    /// Task title
    pub title: String,
    /// Task description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Task URL (e.g., link to issue tracker)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Task source (e.g., "beads", "github")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Workspace context available to templates
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceContext {
    pub ws: WorkspaceInfo,
    pub repo: RepoInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskInfo>,
    #[serde(default)]
    pub vars: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceInfo {
    /// jj workspace name (e.g., "one", "two")
    pub name: String,
    /// Ancillary number (1 for "one", 2 for "two", etc.; 0 if unknown)
    pub num: u32,
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

// ==================== Variable Definitions ====================

/// A variable definition from a `vars {}` block
#[derive(Debug, Clone)]
pub enum VarDef {
    /// Literal string value: `port "8080"`
    Literal { name: String, value: String },
    /// Expression evaluated via minijinja: `port expr="8000 + ws.num"`
    Expr { name: String, expr: String },
}

/// Evaluate a list of variable definitions sequentially.
/// Each var can reference previously-defined vars via the context.
pub fn evaluate_vars(
    vars: &[VarDef],
    ctx: &WorkspaceContext,
) -> Result<HashMap<String, serde_json::Value>> {
    let mut result = HashMap::new();

    for var in vars {
        match var {
            VarDef::Literal { name, value } => {
                // Try to parse as integer, otherwise store as string
                let json_val = if let Ok(n) = value.parse::<i64>() {
                    serde_json::Value::Number(n.into())
                } else {
                    serde_json::Value::String(value.clone())
                };
                result.insert(name.clone(), json_val);
            }
            VarDef::Expr { name, expr } => {
                // Build a temporary context that includes previously evaluated vars
                let template_str = format!("{{{{ {} }}}}", expr);
                let mut env = Environment::new();
                env.add_template("expr", &template_str)?;
                let tmpl = env.get_template("expr")?;
                let rendered = tmpl.render(context! {
                    ws => ctx.ws,
                    repo => ctx.repo,
                    task => ctx.task,
                    vars => &result,
                })?;

                // Try to parse as integer, otherwise store as string
                let json_val = if let Ok(n) = rendered.trim().parse::<i64>() {
                    serde_json::Value::Number(n.into())
                } else {
                    serde_json::Value::String(rendered)
                };
                result.insert(name.clone(), json_val);
            }
        }
    }

    Ok(result)
}

// ==================== Port Specification ====================

/// A port specification that may be a literal u16 or a string (protocol name or template)
#[derive(Debug, Clone)]
pub enum PortSpec {
    /// A numeric port value
    Numeric(u16),
    /// A protocol name ("http"/"https") or template string ("{{vars.port}}")
    Named(String),
}

// ==================== Actions ====================

/// An action to execute during setup or destroy
#[derive(Debug, Clone)]
pub enum Action {
    /// Copy and render a template with workspace context
    Template { src: String, dest: String },
    /// Copy a file or directory using CoW when available, with fallback to regular copy
    Copy {
        src: String,
        dest: String,
        from: Option<String>,
    },
    /// Create a symlink for truly shared content
    Share { src: String, from: Option<String> },
    /// Execute a shell command
    Run {
        command: String,
        cwd: Option<String>,
    },
    /// Manage a station reverse-proxy route
    Proxy {
        port: PortSpec,
        upstream: String,
        tls: Option<bool>,
        name: Option<String>,
    },
}

/// Failure handling mode for setup/destroy actions
#[derive(Debug, Clone, Default, PartialEq)]
pub enum OnFail {
    /// Abort setup on failure (default, current behavior)
    #[default]
    Exit,
    /// Log a warning and continue
    Warn,
    /// Silently continue (debug-level log only)
    Ignore,
}

/// A parsed action with failure-handling metadata
#[derive(Debug, Clone)]
pub struct ParsedAction {
    pub action: Action,
    pub on_fail: OnFail,
}

/// Result from running setup or destroy actions
#[derive(Debug, Default)]
pub struct SetupResult;

// ==================== Config Parsing ====================

/// Configuration parsed from toren.kdl
#[derive(Debug, Default)]
pub struct BreqConfig {
    pub setup: Vec<ParsedAction>,
    pub destroy: Vec<ParsedAction>,
    pub vars: Vec<VarDef>,
}

impl BreqConfig {
    /// Find toren.kdl config file, preferring `toren.kdl` over `.toren.kdl`
    fn config_path(repo_root: &Path) -> Option<PathBuf> {
        let preferred = repo_root.join(TOREN_CONFIG_FILE);
        if preferred.exists() {
            return Some(preferred);
        }
        let hidden = repo_root.join(TOREN_CONFIG_FILE_HIDDEN);
        if hidden.exists() {
            return Some(hidden);
        }
        None
    }

    /// Check if a toren.kdl file exists in the given directory
    pub fn exists(repo_root: &Path) -> bool {
        Self::config_path(repo_root).is_some()
    }

    /// Parse a toren.kdl file from the repository root
    pub fn parse(repo_root: &Path) -> Result<Self> {
        let config_path = match Self::config_path(repo_root) {
            Some(p) => p,
            None => {
                trace!(
                    "No {} found at {}",
                    TOREN_CONFIG_FILE,
                    repo_root.display()
                );
                return Ok(Self::default());
            }
        };

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
                "vars" => {
                    config.vars = Self::parse_vars(node)?;
                }
                other => {
                    warn!("Unknown top-level block in toren.kdl: {}", other);
                }
            }
        }

        Ok(config)
    }

    fn parse_vars(node: &KdlNode) -> Result<Vec<VarDef>> {
        let mut vars = Vec::new();

        if let Some(children) = node.children() {
            for child in children.nodes() {
                let name = child.name().value().to_string();

                // Check for expr= attribute first
                if let Some(expr) = child.get("expr").and_then(|v| v.as_string()) {
                    vars.push(VarDef::Expr {
                        name,
                        expr: expr.to_string(),
                    });
                } else {
                    // Literal: first positional argument
                    let value = child
                        .entries()
                        .iter()
                        .find(|e| e.name().is_none())
                        .and_then(|e| {
                            // Support both string and integer literals
                            if let Some(s) = e.value().as_string() {
                                Some(s.to_string())
                            } else {
                                kdl_value_as_i64(e.value()).map(|n| n.to_string())
                            }
                        })
                        .with_context(|| format!("var '{}' requires a value or expr= attribute", name))?;
                    vars.push(VarDef::Literal { name, value });
                }
            }
        }

        Ok(vars)
    }

    fn parse_block(node: &KdlNode) -> Result<Vec<ParsedAction>> {
        let mut actions = Vec::new();

        if let Some(children) = node.children() {
            for child in children.nodes() {
                let action = Self::parse_action(child)?;
                actions.push(action);
            }
        }

        Ok(actions)
    }

    fn parse_on_fail(node: &KdlNode) -> Result<OnFail> {
        match node.get("on_fail").and_then(|v| v.as_string()) {
            None => Ok(OnFail::Exit),
            Some("exit") => Ok(OnFail::Exit),
            Some("warn") => Ok(OnFail::Warn),
            Some("ignore") => Ok(OnFail::Ignore),
            Some(other) => anyhow::bail!(
                "Invalid on_fail value '{}': expected 'exit', 'warn', or 'ignore'",
                other
            ),
        }
    }

    fn parse_action(node: &KdlNode) -> Result<ParsedAction> {
        let on_fail = Self::parse_on_fail(node)?;
        let action = Self::parse_action_inner(node)?;
        Ok(ParsedAction { action, on_fail })
    }

    fn parse_action_inner(node: &KdlNode) -> Result<Action> {
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
            "proxy" => {
                // First positional arg: port number or string (protocol/"{{template}}")
                let first = node
                    .entries()
                    .iter()
                    .find(|e| e.name().is_none())
                    .context("proxy requires a port or protocol argument")?;

                let port = if let Some(n) = kdl_value_as_i64(first.value()) {
                    PortSpec::Numeric(
                        u16::try_from(n).context("proxy port must be a valid u16")?,
                    )
                } else if let Some(s) = first.value().as_string() {
                    PortSpec::Named(s.to_string())
                } else {
                    anyhow::bail!("proxy first argument must be a port number or string");
                };

                // upstream=: string or integer (supports {{...}} templates)
                let upstream = if let Some(s) =
                    node.get("upstream").and_then(|v| v.as_string())
                {
                    s.to_string()
                } else if let Some(n) = node.get("upstream").and_then(kdl_value_as_i64) {
                    n.to_string()
                } else {
                    anyhow::bail!("proxy requires upstream= attribute");
                };

                // tls=: optional bool; None means infer from protocol at execution time
                let tls = node.get("tls").and_then(|v| v.as_bool());

                // name=: optional subdomain prefix
                let name = node
                    .get("name")
                    .and_then(|v| v.as_string())
                    .map(|s| s.to_string());

                Ok(Action::Proxy {
                    port,
                    upstream,
                    tls,
                    name,
                })
            }
            other => {
                anyhow::bail!("Unknown action type: {}", other);
            }
        }
    }
}

// ==================== Workspace Setup ====================

/// Manages workspace setup state and execution
pub struct WorkspaceSetup {
    /// Path to the repository root (where toren.kdl lives)
    repo_root: PathBuf,
    /// Path to the workspace being set up
    workspace_path: PathBuf,
    /// Workspace name (jj workspace name)
    workspace_name: String,
    /// Ancillary number (0 if unknown)
    ancillary_num: u32,
    /// Local domain for station proxy (e.g. "lvh.me")
    local_domain: Option<String>,
}

impl WorkspaceSetup {
    pub fn new(
        repo_root: PathBuf,
        workspace_path: PathBuf,
        workspace_name: String,
        ancillary_num: u32,
        local_domain: Option<String>,
    ) -> Self {
        Self {
            repo_root,
            workspace_path,
            workspace_name,
            ancillary_num,
            local_domain,
        }
    }

    /// Compute the STATION_DOMAIN value: `{repo_name}.{local_domain}`
    /// Returns None if local_domain is not configured.
    fn station_domain(&self) -> Option<String> {
        let repo_name = self
            .repo_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
        self.local_domain
            .as_ref()
            .map(|domain| format!("{}.{}", repo_name, domain))
    }

    /// Compute the station route name for a proxy action.
    /// If `name` is provided: `{name}.{ws_name}`, otherwise just `{ws_name}`.
    fn station_name(&self, name: Option<&str>) -> String {
        match name {
            Some(n) => format!("{}.{}", n, self.workspace_name),
            None => self.workspace_name.clone(),
        }
    }

    /// Collect unique station names from proxy actions in a setup block.
    /// Renders name templates against the workspace context.
    fn collect_proxy_station_names(
        actions: &[ParsedAction],
        ws_name: &str,
        ctx: &WorkspaceContext,
    ) -> Result<Vec<String>> {
        let mut names: Vec<String> = Vec::new();
        for pa in actions {
            if let Action::Proxy { name, .. } = &pa.action {
                let rendered_name = name
                    .as_deref()
                    .map(|n| render_template(n, ctx))
                    .transpose()?;
                let station_name = match rendered_name.as_deref() {
                    Some(n) => format!("{}.{}", n, ws_name),
                    None => ws_name.to_string(),
                };
                names.push(station_name);
            }
        }
        names.sort();
        names.dedup();
        Ok(names)
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
            task: None,
            vars: HashMap::new(),
        }
    }

    /// Run the setup block
    pub fn run_setup(&self) -> Result<SetupResult> {
        let config = BreqConfig::parse(&self.repo_root)?;

        if config.setup.is_empty() && config.vars.is_empty() {
            debug!("No setup actions defined");
            return Ok(SetupResult);
        }

        info!(
            "Running workspace setup for '{}' in {}",
            self.workspace_name,
            self.workspace_path.display()
        );

        let mut ctx = self.build_context();

        // Evaluate vars and inject into context
        if !config.vars.is_empty() {
            let vars = evaluate_vars(&config.vars, &ctx)?;
            ctx.vars = vars;
        }

        self.execute_actions(&config.setup, &ctx)?;

        info!("Workspace setup complete");
        Ok(SetupResult)
    }

    /// Run the destroy block, then auto-forget any proxy routes from the setup block.
    pub fn run_destroy(&self) -> Result<SetupResult> {
        let config = BreqConfig::parse(&self.repo_root)?;

        let has_destroy = !config.destroy.is_empty();
        let has_proxy_actions = config
            .setup
            .iter()
            .any(|pa| matches!(&pa.action, Action::Proxy { .. }));

        if !has_destroy && !has_proxy_actions {
            debug!("No destroy actions or proxy routes to clean up");
            return Ok(SetupResult);
        }

        info!(
            "Running workspace destroy for '{}' in {}",
            self.workspace_name,
            self.workspace_path.display()
        );

        let mut ctx = self.build_context();

        // Evaluate vars for destroy too (needed for template rendering in proxy names)
        if !config.vars.is_empty() {
            let vars = evaluate_vars(&config.vars, &ctx)?;
            ctx.vars = vars;
        }

        if has_destroy {
            self.execute_actions(&config.destroy, &ctx)?;
        }

        // Auto-forget proxy routes from setup block (best-effort)
        let proxy_names =
            Self::collect_proxy_station_names(&config.setup, &self.workspace_name, &ctx)?;
        for station_name in &proxy_names {
            if let Err(e) = self.execute_station_forget(station_name) {
                warn!("Failed to forget station route '{}': {}", station_name, e);
            }
        }

        info!("Workspace destroy complete");
        Ok(SetupResult)
    }

    /// Execute a list of actions in order.
    /// Respects on_fail metadata on each action.
    fn execute_actions(
        &self,
        actions: &[ParsedAction],
        ctx: &WorkspaceContext,
    ) -> Result<()> {
        for (i, parsed) in actions.iter().enumerate() {
            trace!("Executing action {}: {:?}", i + 1, parsed.action);
            let res = self
                .execute_action(&parsed.action, ctx)
                .with_context(|| format!("Action {} failed", i + 1));

            if let Err(e) = res {
                match parsed.on_fail {
                    OnFail::Exit => return Err(e),
                    OnFail::Warn => warn!("Action {} failed (continuing): {:#}", i + 1, e),
                    OnFail::Ignore => debug!("Action {} failed (ignored): {:#}", i + 1, e),
                }
            }
        }

        Ok(())
    }

    fn execute_action(&self, action: &Action, ctx: &WorkspaceContext) -> Result<()> {
        match action {
            Action::Template { src, dest } => self.execute_template(src, dest, ctx),
            Action::Copy { src, dest, from } => self.execute_copy(src, dest, from.as_deref(), ctx),
            Action::Share { src, from } => self.execute_share(src, from.as_deref(), ctx),
            Action::Run { command, cwd } => self.execute_run(command, cwd.as_deref(), ctx),
            Action::Proxy {
                port,
                upstream,
                tls,
                name,
            } => self.execute_proxy(port, upstream, *tls, name.as_deref(), ctx),
        }
    }

    fn execute_template(&self, src: &str, dest: &str, ctx: &WorkspaceContext) -> Result<()> {
        let src = self.render_string(src, ctx)?;
        let dest = self.render_string(dest, ctx)?;
        // Source is relative to repo root (template files are versioned)
        let src_path = self.repo_root.join(&src);
        // Dest is relative to workspace
        let dest_path = self.workspace_path.join(&dest);

        let template_content = fs::read_to_string(&src_path)
            .with_context(|| format!("Failed to read template: {}", src_path.display()))?;

        let mut env = Environment::new();
        env.add_template("template", &template_content)?;

        let template = env.get_template("template")?;
        let rendered = template.render(context! {
            ws => ctx.ws,
            repo => ctx.repo,
            task => ctx.task,
            vars => ctx.vars,
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

    fn execute_copy(
        &self,
        src: &str,
        dest: &str,
        from: Option<&str>,
        ctx: &WorkspaceContext,
    ) -> Result<()> {
        let src = self.render_string(src, ctx)?;
        let dest = self.render_string(dest, ctx)?;
        // Resolve source: from attribute (with template rendering) or repo root
        let src_path = if let Some(from_template) = from {
            let rendered_from = self.render_string(from_template, ctx)?;
            PathBuf::from(rendered_from).join(&src)
        } else {
            self.repo_root.join(&src)
        };
        // Dest is relative to workspace
        let dest_path = self.workspace_path.join(&dest);

        // Ensure parent directory exists
        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Use clonetree for CoW with automatic fallback
        clonetree::clone_tree(&src_path, &dest_path, &CloneOptions::new()).with_context(|| {
            format!(
                "Failed to copy {} to {}",
                src_path.display(),
                dest_path.display()
            )
        })?;

        info!("  copy: {} -> {}", src_path.display(), dest);
        Ok(())
    }

    fn execute_share(&self, src: &str, from: Option<&str>, ctx: &WorkspaceContext) -> Result<()> {
        let src = self.render_string(src, ctx)?;
        // Resolve source: from attribute (with template rendering) or repo root
        let src_path = if let Some(from_template) = from {
            let rendered_from = self.render_string(from_template, ctx)?;
            PathBuf::from(rendered_from).join(&src)
        } else {
            PathBuf::from(&ctx.repo.root).join(&src)
        };
        let dest_path = self.workspace_path.join(&src);

        // Ensure parent directory exists
        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Handle existing dest: skip if correct symlink, remove if stale/wrong
        if dest_path.symlink_metadata().is_ok() {
            if let Ok(target) = fs::read_link(&dest_path) {
                if target == src_path {
                    debug!(
                        "  share: {} already points to {} (skipping)",
                        dest_path.display(),
                        src_path.display()
                    );
                    return Ok(());
                }
            }
            // Stale or wrong symlink (or regular file/dir) - remove it
            info!(
                "  share: removing stale entry at {}",
                dest_path.display()
            );
            if dest_path.is_dir() && !dest_path.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
                fs::remove_dir_all(&dest_path)?;
            } else {
                fs::remove_file(&dest_path)?;
            }
        }

        // Create symlink
        #[cfg(unix)]
        std::os::unix::fs::symlink(&src_path, &dest_path).with_context(|| {
            format!(
                "Failed to symlink {} -> {}",
                dest_path.display(),
                src_path.display()
            )
        })?;

        #[cfg(windows)]
        {
            if src_path.is_dir() {
                std::os::windows::fs::symlink_dir(&src_path, &dest_path)
            } else {
                std::os::windows::fs::symlink_file(&src_path, &dest_path)
            }
            .with_context(|| {
                format!(
                    "Failed to symlink {} -> {}",
                    dest_path.display(),
                    src_path.display()
                )
            })?;
        }

        info!("  share: {} -> {}", dest_path.display(), src_path.display());
        Ok(())
    }

    /// Render a string template with workspace context
    fn render_string(&self, template: &str, ctx: &WorkspaceContext) -> Result<String> {
        render_template(template, ctx)
    }

    fn execute_run(&self, command: &str, cwd: Option<&str>, ctx: &WorkspaceContext) -> Result<()> {
        let command = self.render_string(command, ctx)?;
        let cwd_rendered = cwd
            .map(|c| self.render_string(c, ctx))
            .transpose()?;

        // Resolve cwd: if provided, relative to workspace; otherwise workspace root
        let work_dir = match &cwd_rendered {
            Some(dir) => self.workspace_path.join(dir),
            None => self.workspace_path.clone(),
        };

        if let Some(dir) = &cwd_rendered {
            info!("  run: {} (in {})", command, dir);
        } else {
            info!("  run: {}", command);
        }

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&command).current_dir(&work_dir);

        // Inject STATION_DOMAIN env var if available
        if let Some(domain) = self.station_domain() {
            cmd.env("STATION_DOMAIN", &domain);
        }

        let output = cmd
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

    fn execute_proxy(
        &self,
        port: &PortSpec,
        upstream: &str,
        tls: Option<bool>,
        name: Option<&str>,
        ctx: &WorkspaceContext,
    ) -> Result<()> {
        let upstream_val = self.render_string(upstream, ctx)?;
        let rendered_name = name
            .map(|n| self.render_string(n, ctx))
            .transpose()?;
        let station_name = self.station_name(rendered_name.as_deref());

        // Resolve port: Numeric uses directly, Named renders template then resolves
        let (resolved_port, implicit_tls) = match port {
            PortSpec::Numeric(n) => (*n, false),
            PortSpec::Named(s) => {
                let rendered = self.render_string(s, ctx)?;
                match rendered.as_str() {
                    "http" => (80, false),
                    "https" => (443, true),
                    other => {
                        let p: u16 = other
                            .parse()
                            .with_context(|| format!(
                                "proxy port must be \"http\", \"https\", or a u16, got \"{}\"",
                                other
                            ))?;
                        (p, false)
                    }
                }
            }
        };

        // Explicit tls= overrides implicit protocol tls
        let use_tls = tls.unwrap_or(implicit_tls);

        info!("  proxy: {} -> {} (port {}{})", station_name, upstream_val, resolved_port,
            if use_tls { ", tls" } else { "" });

        let mut cmd = Command::new("station");
        cmd.arg("proxy")
            .arg(&station_name)
            .arg("-u")
            .arg(&upstream_val)
            .arg("-p")
            .arg(resolved_port.to_string());

        if use_tls {
            cmd.arg("--tls");
        }

        if let Some(domain) = self.station_domain() {
            cmd.env("STATION_DOMAIN", &domain);
        }

        let output = cmd
            .output()
            .with_context(|| format!("Failed to run station proxy for '{}'", station_name))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("station proxy failed for '{}': {}", station_name, stderr.trim());
        }

        Ok(())
    }

    fn execute_station_forget(&self, station_name: &str) -> Result<()> {
        info!("  station forget: {}", station_name);

        let mut cmd = Command::new("station");
        cmd.arg("forget").arg(station_name);

        if let Some(domain) = self.station_domain() {
            cmd.env("STATION_DOMAIN", &domain);
        }

        let output = cmd
            .output()
            .with_context(|| format!("Failed to run station forget for '{}'", station_name))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("station forget failed for '{}': {}", station_name, stderr.trim());
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

        match &config.setup[0].action {
            Action::Template { src, dest } => {
                assert_eq!(src, ".env.breq");
                assert_eq!(dest, ".env");
            }
            _ => panic!("Expected Template action"),
        }

        match &config.setup[1].action {
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
        match &config.setup[0].action {
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
        match &config.setup[0].action {
            Action::Share { src, from } => {
                assert_eq!(src, "node_modules");
                assert_eq!(from.as_deref(), Some("{{ repo.root }}"));
            }
            _ => panic!("Expected Share action"),
        }
        match &config.setup[1].action {
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
        match &config.setup[0].action {
            Action::Copy { src, dest, from } => {
                assert_eq!(src, "node_modules");
                assert_eq!(dest, "node_modules"); // dest defaults to src
                assert_eq!(from.as_deref(), Some("{{ repo.root }}"));
            }
            _ => panic!("Expected Copy action"),
        }
        match &config.setup[1].action {
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
        match &config.setup[0].action {
            Action::Copy { src, dest, from } => {
                assert_eq!(src, "/some/path/to/node_modules");
                assert_eq!(dest, "node_modules"); // basename for absolute paths
                assert!(from.is_none());
            }
            _ => panic!("Expected Copy action"),
        }
        match &config.setup[1].action {
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
        match &config.setup[0].action {
            Action::Run { command, cwd } => {
                assert_eq!(command, "pnpm install");
                assert_eq!(cwd.as_deref(), Some("web"));
            }
            _ => panic!("Expected Run action"),
        }
        match &config.setup[1].action {
            Action::Run { command, cwd } => {
                assert_eq!(command, "cargo build");
                assert!(cwd.is_none());
            }
            _ => panic!("Expected Run action"),
        }
    }

    #[test]
    fn test_parse_vars_literal() {
        let content = r#"
vars {
    port 5173
    name "my-app"
}
setup { }
"#;

        let config = BreqConfig::parse_kdl(content).unwrap();
        assert_eq!(config.vars.len(), 2);

        match &config.vars[0] {
            VarDef::Literal { name, value } => {
                assert_eq!(name, "port");
                assert_eq!(value, "5173");
            }
            _ => panic!("Expected Literal"),
        }
        match &config.vars[1] {
            VarDef::Literal { name, value } => {
                assert_eq!(name, "name");
                assert_eq!(value, "my-app");
            }
            _ => panic!("Expected Literal"),
        }
    }

    #[test]
    fn test_parse_vars_expr() {
        let content = r#"
vars {
    base_port 5170
    port expr="vars.base_port + ws.num"
}
setup { }
"#;

        let config = BreqConfig::parse_kdl(content).unwrap();
        assert_eq!(config.vars.len(), 2);

        match &config.vars[1] {
            VarDef::Expr { name, expr } => {
                assert_eq!(name, "port");
                assert_eq!(expr, "vars.base_port + ws.num");
            }
            _ => panic!("Expected Expr"),
        }
    }

    #[test]
    fn test_evaluate_vars_sequential() {
        let vars = vec![
            VarDef::Literal {
                name: "base_port".to_string(),
                value: "5170".to_string(),
            },
            VarDef::Expr {
                name: "port".to_string(),
                expr: "vars.base_port + ws.num".to_string(),
            },
        ];

        let ctx = WorkspaceContext {
            ws: WorkspaceInfo {
                name: "three".to_string(),
                num: 3,
                path: "/tmp/ws".to_string(),
            },
            repo: RepoInfo {
                root: "/tmp/repo".to_string(),
                name: "myrepo".to_string(),
            },
            task: None,
            vars: HashMap::new(),
        };

        let result = evaluate_vars(&vars, &ctx).unwrap();
        assert_eq!(result.get("base_port"), Some(&serde_json::json!(5170)));
        assert_eq!(result.get("port"), Some(&serde_json::json!(5173)));
    }

    #[test]
    fn test_render_template_with_vars() {
        let ctx = WorkspaceContext {
            ws: WorkspaceInfo {
                name: "one".to_string(),
                num: 1,
                path: "/tmp/ws".to_string(),
            },
            repo: RepoInfo {
                root: "/tmp/repo".to_string(),
                name: "myrepo".to_string(),
            },
            task: None,
            vars: {
                let mut m = HashMap::new();
                m.insert("upstream_url".to_string(), serde_json::json!("http://localhost:5173"));
                m.insert("port".to_string(), serde_json::json!(5173));
                m
            },
        };

        assert_eq!(
            render_template("{{vars.upstream_url}}", &ctx).unwrap(),
            "http://localhost:5173"
        );
        assert_eq!(render_template("{{ws.name}}", &ctx).unwrap(), "one");
        assert_eq!(render_template("{{ws.num}}", &ctx).unwrap(), "1");
        assert_eq!(render_template("{{vars.port}}", &ctx).unwrap(), "5173");
    }

    #[test]
    fn test_parse_on_fail_default() {
        let content = r#"
setup {
    run "pnpm install"
}
"#;
        let config = BreqConfig::parse_kdl(content).unwrap();
        assert_eq!(config.setup[0].on_fail, OnFail::Exit);
    }

    #[test]
    fn test_parse_on_fail_exit() {
        let content = r#"
setup {
    run "pnpm install" on_fail="exit"
}
"#;
        let config = BreqConfig::parse_kdl(content).unwrap();
        assert_eq!(config.setup[0].on_fail, OnFail::Exit);
    }

    #[test]
    fn test_parse_on_fail_warn() {
        let content = r#"
setup {
    run "createdb mydb" on_fail="warn"
    template src=".env.tpl" dest=".env" on_fail="warn"
    copy src="optional.conf" on_fail="warn"
}
"#;
        let config = BreqConfig::parse_kdl(content).unwrap();
        assert_eq!(config.setup[0].on_fail, OnFail::Warn);
        assert_eq!(config.setup[1].on_fail, OnFail::Warn);
        assert_eq!(config.setup[2].on_fail, OnFail::Warn);
    }

    #[test]
    fn test_parse_on_fail_ignore() {
        let content = r#"
setup {
    run "optional-step" on_fail="ignore"
    share src="cache" on_fail="ignore"
}
"#;
        let config = BreqConfig::parse_kdl(content).unwrap();
        assert_eq!(config.setup[0].on_fail, OnFail::Ignore);
        assert_eq!(config.setup[1].on_fail, OnFail::Ignore);
    }

    #[test]
    fn test_parse_on_fail_invalid() {
        let content = r#"
setup {
    run "test" on_fail="retry"
}
"#;
        let err = BreqConfig::parse_kdl(content).unwrap_err();
        assert!(
            err.to_string().contains("Invalid on_fail value 'retry'"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn test_parse_on_fail_in_destroy() {
        let content = r#"
destroy {
    run "cleanup" on_fail="ignore"
}
"#;
        let config = BreqConfig::parse_kdl(content).unwrap();
        assert_eq!(config.destroy[0].on_fail, OnFail::Ignore);
    }

    fn test_setup() -> WorkspaceSetup {
        let dir = std::env::temp_dir().join("toren-test-ws");
        let _ = fs::create_dir_all(&dir);
        WorkspaceSetup::new(dir.clone(), dir, "test".to_string(), 1, None)
    }

    #[test]
    fn test_execute_actions_on_fail_warn_continues() {
        let setup = test_setup();
        let ctx = setup.build_context();
        let actions = vec![
            ParsedAction {
                action: Action::Run {
                    command: "exit 1".to_string(),
                    cwd: None,
                },
                on_fail: OnFail::Warn,
            },
            ParsedAction {
                action: Action::Run {
                    command: "echo ok".to_string(),
                    cwd: None,
                },
                on_fail: OnFail::Exit,
            },
        ];
        let result = setup.execute_actions(&actions, &ctx);
        assert!(result.is_ok(), "on_fail=warn should continue: {:?}", result);
    }

    #[test]
    fn test_execute_actions_on_fail_ignore_continues() {
        let setup = test_setup();
        let ctx = setup.build_context();
        let actions = vec![
            ParsedAction {
                action: Action::Run {
                    command: "exit 1".to_string(),
                    cwd: None,
                },
                on_fail: OnFail::Ignore,
            },
            ParsedAction {
                action: Action::Run {
                    command: "echo ok".to_string(),
                    cwd: None,
                },
                on_fail: OnFail::Exit,
            },
        ];
        let result = setup.execute_actions(&actions, &ctx);
        assert!(
            result.is_ok(),
            "on_fail=ignore should continue: {:?}",
            result
        );
    }

    #[test]
    fn test_execute_actions_on_fail_exit_aborts() {
        let setup = test_setup();
        let ctx = setup.build_context();
        let actions = vec![
            ParsedAction {
                action: Action::Run {
                    command: "exit 1".to_string(),
                    cwd: None,
                },
                on_fail: OnFail::Exit,
            },
            ParsedAction {
                action: Action::Run {
                    command: "echo should-not-run".to_string(),
                    cwd: None,
                },
                on_fail: OnFail::Exit,
            },
        ];
        let result = setup.execute_actions(&actions, &ctx);
        assert!(result.is_err(), "on_fail=exit should abort");
    }

    // ─── Proxy parsing tests ───────────────────────────────────────────

    #[test]
    fn test_parse_proxy_with_port() {
        let content = r#"
setup {
    proxy 80 upstream=3000
}
"#;
        let config = BreqConfig::parse_kdl(content).unwrap();
        assert_eq!(config.setup.len(), 1);
        match &config.setup[0].action {
            Action::Proxy { port, upstream, tls, name } => {
                assert!(matches!(port, PortSpec::Numeric(80)));
                assert_eq!(upstream, "3000");
                assert_eq!(*tls, None);
                assert!(name.is_none());
            }
            _ => panic!("Expected Proxy action"),
        }
    }

    #[test]
    fn test_parse_proxy_https_protocol() {
        let content = r#"
setup {
    proxy "https" upstream=4443
}
"#;
        let config = BreqConfig::parse_kdl(content).unwrap();
        match &config.setup[0].action {
            Action::Proxy { port, tls, .. } => {
                assert!(matches!(port, PortSpec::Named(s) if s == "https"));
                assert_eq!(*tls, None);
            }
            _ => panic!("Expected Proxy action"),
        }
    }

    #[test]
    fn test_parse_proxy_http_protocol() {
        let content = r#"
setup {
    proxy "http" upstream=8080
}
"#;
        let config = BreqConfig::parse_kdl(content).unwrap();
        match &config.setup[0].action {
            Action::Proxy { port, tls, .. } => {
                assert!(matches!(port, PortSpec::Named(s) if s == "http"));
                assert_eq!(*tls, None);
            }
            _ => panic!("Expected Proxy action"),
        }
    }

    #[test]
    fn test_parse_proxy_with_uri_upstream() {
        let content = r#"
setup {
    proxy 80 upstream="http://localhost:3000"
}
"#;
        let config = BreqConfig::parse_kdl(content).unwrap();
        match &config.setup[0].action {
            Action::Proxy { upstream, .. } => {
                assert_eq!(upstream, "http://localhost:3000");
            }
            _ => panic!("Expected Proxy action"),
        }
    }

    #[test]
    fn test_parse_proxy_with_upstream_template() {
        let content = r#"
setup {
    proxy "http" upstream="{{vars.web_port}}"
}
"#;
        let config = BreqConfig::parse_kdl(content).unwrap();
        match &config.setup[0].action {
            Action::Proxy { upstream, .. } => {
                assert_eq!(upstream, "{{vars.web_port}}");
            }
            _ => panic!("Expected Proxy action"),
        }
    }

    #[test]
    fn test_parse_proxy_with_tls_and_name() {
        let content = r#"
setup {
    proxy 443 upstream=8443 tls=#true name="api"
}
"#;
        let config = BreqConfig::parse_kdl(content).unwrap();
        match &config.setup[0].action {
            Action::Proxy { port, upstream, tls, name } => {
                assert!(matches!(port, PortSpec::Numeric(443)));
                assert_eq!(upstream, "8443");
                assert_eq!(*tls, Some(true));
                assert_eq!(name.as_deref(), Some("api"));
            }
            _ => panic!("Expected Proxy action"),
        }
    }

    #[test]
    fn test_parse_proxy_string_accepted_at_parse_time() {
        // Any string is accepted at parse time; validation happens at execution time
        let content = r#"
setup {
    proxy "ftp" upstream=21
}
"#;
        let config = BreqConfig::parse_kdl(content).unwrap();
        match &config.setup[0].action {
            Action::Proxy { port, .. } => {
                assert!(matches!(port, PortSpec::Named(s) if s == "ftp"));
            }
            _ => panic!("Expected Proxy action"),
        }
    }

    #[test]
    fn test_parse_proxy_missing_upstream() {
        let content = r#"
setup {
    proxy 80
}
"#;
        let err = BreqConfig::parse_kdl(content).unwrap_err();
        assert!(err.to_string().contains("upstream"), "unexpected error: {}", err);
    }

    #[test]
    fn test_station_domain() {
        let setup = WorkspaceSetup::new(
            PathBuf::from("/repos/myrepo"),
            PathBuf::from("/ws/one"),
            "one".to_string(),
            1,
            Some("lvh.me".to_string()),
        );
        assert_eq!(setup.station_domain(), Some("myrepo.lvh.me".to_string()));
    }

    #[test]
    fn test_station_domain_none() {
        let setup = WorkspaceSetup::new(
            PathBuf::from("/repos/myrepo"),
            PathBuf::from("/ws/one"),
            "one".to_string(),
            1,
            None,
        );
        assert_eq!(setup.station_domain(), None);
    }

    #[test]
    fn test_station_name_with_prefix() {
        let setup = WorkspaceSetup::new(
            PathBuf::from("/repos/myrepo"),
            PathBuf::from("/ws/one"),
            "one".to_string(),
            1,
            None,
        );
        assert_eq!(setup.station_name(Some("api")), "api.one");
        assert_eq!(setup.station_name(None), "one");
    }

    #[test]
    fn test_collect_proxy_station_names() {
        let ctx = WorkspaceContext {
            ws: WorkspaceInfo {
                name: "one".to_string(),
                num: 1,
                path: "/tmp/ws".to_string(),
            },
            repo: RepoInfo {
                root: "/tmp/repo".to_string(),
                name: "myrepo".to_string(),
            },
            task: None,
            vars: HashMap::new(),
        };
        let actions = vec![
            ParsedAction {
                action: Action::Proxy {
                    port: PortSpec::Numeric(80),
                    upstream: "3000".to_string(),
                    tls: None,
                    name: None,
                },
                on_fail: OnFail::Exit,
            },
            ParsedAction {
                action: Action::Proxy {
                    port: PortSpec::Numeric(443),
                    upstream: "4443".to_string(),
                    tls: Some(true),
                    name: Some("api".to_string()),
                },
                on_fail: OnFail::Exit,
            },
            ParsedAction {
                action: Action::Run {
                    command: "echo hi".to_string(),
                    cwd: None,
                },
                on_fail: OnFail::Exit,
            },
        ];
        let names = WorkspaceSetup::collect_proxy_station_names(&actions, "one", &ctx).unwrap();
        assert_eq!(names, vec!["api.one", "one"]);
    }
}
