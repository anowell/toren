//! Workspace setup hooks for initializing and tearing down jj workspaces.
//!
//! This module implements a lightweight, procedural mechanism for workspace initialization
//! using `.toren.kdl` configuration files. It supports these primitive actions:
//! - `template`: Copy and render files with workspace context
//! - `copy`: Copy files verbatim
//! - `run`: Execute shell commands
//! - `proxy`: Declare a reverse-proxy route (returned as a directive)

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

use crate::config::ProxyConfig;

const TOREN_CONFIG_FILE: &str = ".toren.kdl";

/// Extract an i64 from a KdlValue (kdl 6.x uses i128 internally)
fn kdl_value_as_i64(val: &kdl::KdlValue) -> Option<i64> {
    val.as_integer().and_then(|n| i64::try_from(n).ok())
}

/// Render a template string with workspace context using minijinja.
/// Available variables: ws.name, ws.num, ws.path, repo.root, repo.name, task.id, task.title, vars.*, config.*
pub fn render_template(template: &str, ctx: &WorkspaceContext) -> Result<String> {
    let mut env = Environment::new();
    env.add_template("inline", template)?;
    let tmpl = env.get_template("inline")?;
    let rendered = tmpl.render(context! {
        ws => ctx.ws,
        repo => ctx.repo,
        task => ctx.task,
        vars => ctx.vars,
        config => ctx.config,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigContext>,
}

/// Config context exposed to templates
#[derive(Debug, Clone, Serialize)]
pub struct ConfigContext {
    pub proxy: ProxyContext,
}

/// Proxy config exposed to templates
#[derive(Debug, Clone, Serialize)]
pub struct ProxyContext {
    pub domain: String,
    pub tls: bool,
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
                    config => ctx.config,
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

// ==================== Attribute Values (.var support) ====================

/// An attribute value that may be a literal string or a reference to a context variable
#[derive(Debug, Clone)]
pub enum AttrValue {
    /// A plain string literal
    Literal(String),
    /// A dotted-path reference into the workspace context (e.g., "vars.upstream_url")
    VarRef(String),
}

impl AttrValue {
    /// Resolve this attribute value to a concrete string.
    /// For VarRef, performs a dotted-path lookup on the serialized context.
    pub fn resolve(&self, ctx: &WorkspaceContext) -> Result<String> {
        match self {
            AttrValue::Literal(s) => Ok(s.clone()),
            AttrValue::VarRef(path) => {
                let ctx_value = serde_json::to_value(ctx)
                    .context("Failed to serialize workspace context")?;
                let mut current = &ctx_value;
                for segment in path.split('.') {
                    current = current.get(segment).with_context(|| {
                        format!("Variable path '{}' not found (failed at '{}')", path, segment)
                    })?;
                }
                // Convert to string
                match current {
                    serde_json::Value::String(s) => Ok(s.clone()),
                    serde_json::Value::Number(n) => Ok(n.to_string()),
                    serde_json::Value::Bool(b) => Ok(b.to_string()),
                    other => Ok(other.to_string()),
                }
            }
        }
    }
}

/// Get an attribute value from a KDL node, checking for `.var` suffix first.
/// If `name.var` exists, returns `AttrValue::VarRef`; otherwise checks `name` for a literal.
fn get_attr(node: &KdlNode, name: &str) -> Option<AttrValue> {
    let var_name = format!("{}.var", name);
    if let Some(val) = node.get(&*var_name).and_then(|v| v.as_string()) {
        return Some(AttrValue::VarRef(val.to_string()));
    }
    node.get(name)
        .and_then(|v| v.as_string())
        .map(|s| AttrValue::Literal(s.to_string()))
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
    /// Declare a reverse-proxy route
    Proxy {
        port: u16,
        upstream: AttrValue,
        tls: Option<bool>,
        host: Option<AttrValue>,
    },
}

/// A resolved proxy directive ready for Caddy configuration
#[derive(Debug, Clone, Serialize)]
pub struct ProxyDirective {
    /// The hostname to match (e.g., "one.toren.lvh.me")
    pub host: String,
    /// The upstream URL (e.g., "http://localhost:5173")
    pub upstream: String,
    /// Whether to use TLS
    pub tls: bool,
    /// The workspace port
    pub port: u16,
}

/// Result from running setup or destroy actions
#[derive(Debug, Default)]
pub struct SetupResult {
    /// Proxy directives declared by proxy actions
    pub proxy_directives: Vec<ProxyDirective>,
}

// ==================== Config Parsing ====================

/// Configuration parsed from .toren.kdl
#[derive(Debug, Default)]
pub struct BreqConfig {
    pub setup: Vec<Action>,
    pub destroy: Vec<Action>,
    pub vars: Vec<VarDef>,
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
            trace!(
                "No {} found at {}",
                TOREN_CONFIG_FILE,
                config_path.display()
            );
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
                "vars" => {
                    config.vars = Self::parse_vars(node)?;
                }
                other => {
                    warn!("Unknown top-level block in .toren.kdl: {}", other);
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
                            } else if let Some(n) = kdl_value_as_i64(e.value()) {
                                Some(n.to_string())
                            } else {
                                None
                            }
                        })
                        .with_context(|| format!("var '{}' requires a value or expr= attribute", name))?;
                    vars.push(VarDef::Literal { name, value });
                }
            }
        }

        Ok(vars)
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
            "proxy" => {
                // Support both positional (proxy 443 ...) and named (proxy port=443 ...)
                let port = node
                    .get("port")
                    .and_then(|v| kdl_value_as_i64(v))
                    .or_else(|| {
                        node.entries()
                            .iter()
                            .find(|e| e.name().is_none())
                            .and_then(|e| kdl_value_as_i64(e.value()))
                    })
                    .context("proxy requires port= attribute or port number as first argument")?
                    as u16;
                let upstream = get_attr(node, "upstream")
                    .context("proxy requires upstream= or upstream.var= attribute")?;
                let tls = node.get("tls").and_then(|v| v.as_bool());
                let host = get_attr(node, "host");
                Ok(Action::Proxy {
                    port,
                    upstream,
                    tls,
                    host,
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
    /// Path to the repository root (where .toren.kdl lives)
    repo_root: PathBuf,
    /// Path to the workspace being set up
    workspace_path: PathBuf,
    /// Workspace name (jj workspace name)
    workspace_name: String,
    /// Ancillary number (0 if unknown)
    ancillary_num: u32,
}

impl WorkspaceSetup {
    pub fn new(
        repo_root: PathBuf,
        workspace_path: PathBuf,
        workspace_name: String,
        ancillary_num: u32,
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
            task: None,
            vars: HashMap::new(),
            config: None,
        }
    }

    /// Run the setup block
    pub fn run_setup(&self, proxy_config: Option<&ProxyConfig>) -> Result<SetupResult> {
        let config = BreqConfig::parse(&self.repo_root)?;

        if config.setup.is_empty() && config.vars.is_empty() {
            debug!("No setup actions defined");
            return Ok(SetupResult::default());
        }

        info!(
            "Running workspace setup for '{}' in {}",
            self.workspace_name,
            self.workspace_path.display()
        );

        let mut ctx = self.build_context();

        // Set config context from proxy config
        if let Some(pc) = proxy_config {
            ctx.config = Some(ConfigContext {
                proxy: ProxyContext {
                    domain: pc.domain.clone(),
                    tls: pc.tls,
                },
            });
        }

        // Evaluate vars and inject into context
        if !config.vars.is_empty() {
            let vars = evaluate_vars(&config.vars, &ctx)?;
            ctx.vars = vars;
        }

        let result = self.execute_actions(&config.setup, &ctx, proxy_config)?;

        info!("Workspace setup complete");
        Ok(result)
    }

    /// Run the destroy block
    pub fn run_destroy(&self, proxy_config: Option<&ProxyConfig>) -> Result<SetupResult> {
        let config = BreqConfig::parse(&self.repo_root)?;

        if config.destroy.is_empty() {
            debug!("No destroy actions defined");
            return Ok(SetupResult::default());
        }

        info!(
            "Running workspace destroy for '{}' in {}",
            self.workspace_name,
            self.workspace_path.display()
        );

        let mut ctx = self.build_context();

        // Set config context from proxy config
        if let Some(pc) = proxy_config {
            ctx.config = Some(ConfigContext {
                proxy: ProxyContext {
                    domain: pc.domain.clone(),
                    tls: pc.tls,
                },
            });
        }

        // Evaluate vars for destroy too
        if !config.vars.is_empty() {
            let vars = evaluate_vars(&config.vars, &ctx)?;
            ctx.vars = vars;
        }

        let result = self.execute_actions(&config.destroy, &ctx, proxy_config)?;

        info!("Workspace destroy complete");
        Ok(result)
    }

    /// Execute a list of actions in order, collecting proxy directives
    fn execute_actions(
        &self,
        actions: &[Action],
        ctx: &WorkspaceContext,
        proxy_config: Option<&ProxyConfig>,
    ) -> Result<SetupResult> {
        let mut result = SetupResult::default();

        for (i, action) in actions.iter().enumerate() {
            trace!("Executing action {}: {:?}", i + 1, action);
            match action {
                Action::Proxy {
                    port,
                    upstream,
                    tls,
                    host,
                } => {
                    // Resolve proxy directive
                    let resolved_upstream = upstream
                        .resolve(ctx)
                        .with_context(|| format!("Action {} (proxy): failed to resolve upstream", i + 1))?;
                    let resolved_host = if let Some(h) = host {
                        h.resolve(ctx)
                            .with_context(|| format!("Action {} (proxy): failed to resolve host", i + 1))?
                    } else {
                        // Default host: ws.name.repo.name.config.proxy.domain
                        let domain = proxy_config
                            .map(|pc| pc.domain.as_str())
                            .unwrap_or("lvh.me");
                        format!("{}.{}.{}", ctx.ws.name, ctx.repo.name, domain)
                    };
                    let resolved_tls = tls.unwrap_or_else(|| {
                        proxy_config.map(|pc| pc.tls).unwrap_or(false)
                    });

                    info!("  proxy: {} -> {} (port {})", resolved_host, resolved_upstream, port);
                    result.proxy_directives.push(ProxyDirective {
                        host: resolved_host,
                        upstream: resolved_upstream,
                        tls: resolved_tls,
                        port: *port,
                    });
                }
                _ => {
                    self.execute_action(action, ctx)
                        .with_context(|| format!("Action {} failed", i + 1))?;
                }
            }
        }

        Ok(result)
    }

    fn execute_action(&self, action: &Action, ctx: &WorkspaceContext) -> Result<()> {
        match action {
            Action::Template { src, dest } => self.execute_template(src, dest, ctx),
            Action::Copy { src, dest, from } => self.execute_copy(src, dest, from.as_deref(), ctx),
            Action::Share { src, from } => self.execute_share(src, from.as_deref(), ctx),
            Action::Run { command, cwd } => self.execute_run(command, cwd.as_deref()),
            Action::Proxy { .. } => Ok(()), // handled in execute_actions
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
            task => ctx.task,
            vars => ctx.vars,
            config => ctx.config,
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
            config: None,
        };

        let result = evaluate_vars(&vars, &ctx).unwrap();
        assert_eq!(result.get("base_port"), Some(&serde_json::json!(5170)));
        assert_eq!(result.get("port"), Some(&serde_json::json!(5173)));
    }

    #[test]
    fn test_attr_value_var_ref_resolve() {
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
                m
            },
            config: None,
        };

        let attr = AttrValue::VarRef("vars.upstream_url".to_string());
        assert_eq!(attr.resolve(&ctx).unwrap(), "http://localhost:5173");

        let attr = AttrValue::VarRef("ws.name".to_string());
        assert_eq!(attr.resolve(&ctx).unwrap(), "one");

        let attr = AttrValue::VarRef("ws.num".to_string());
        assert_eq!(attr.resolve(&ctx).unwrap(), "1");
    }

    #[test]
    fn test_parse_proxy_action() {
        let content = r#"
setup {
    proxy 5173 upstream="http://localhost:5173"
}
"#;

        let config = BreqConfig::parse_kdl(content).unwrap();
        assert_eq!(config.setup.len(), 1);

        match &config.setup[0] {
            Action::Proxy {
                port,
                upstream,
                tls,
                host,
            } => {
                assert_eq!(*port, 5173);
                match upstream {
                    AttrValue::Literal(s) => assert_eq!(s, "http://localhost:5173"),
                    _ => panic!("Expected Literal upstream"),
                }
                assert!(tls.is_none());
                assert!(host.is_none());
            }
            _ => panic!("Expected Proxy action"),
        }
    }

    #[test]
    fn test_parse_proxy_action_with_var() {
        let content = r#"
vars {
    upstream_url "http://localhost:5173"
}
setup {
    proxy 5173 upstream.var="vars.upstream_url" host="custom.lvh.me"
}
"#;

        let config = BreqConfig::parse_kdl(content).unwrap();
        assert_eq!(config.setup.len(), 1);

        match &config.setup[0] {
            Action::Proxy {
                port,
                upstream,
                host,
                ..
            } => {
                assert_eq!(*port, 5173);
                match upstream {
                    AttrValue::VarRef(path) => assert_eq!(path, "vars.upstream_url"),
                    _ => panic!("Expected VarRef upstream"),
                }
                match host.as_ref().unwrap() {
                    AttrValue::Literal(s) => assert_eq!(s, "custom.lvh.me"),
                    _ => panic!("Expected Literal host"),
                }
            }
            _ => panic!("Expected Proxy action"),
        }
    }

    #[test]
    fn test_proxy_config_defaults() {
        let pc = ProxyConfig::default();
        assert!(!pc.enabled);
        assert!(!pc.tls);
        assert_eq!(pc.domain, "lvh.me");
        assert!(pc.dns_port.is_none());
    }

    #[test]
    fn test_default_host_pattern() {
        let ctx = WorkspaceContext {
            ws: WorkspaceInfo {
                name: "one".to_string(),
                num: 1,
                path: "/tmp/ws".to_string(),
            },
            repo: RepoInfo {
                root: "/tmp/repo".to_string(),
                name: "toren".to_string(),
            },
            task: None,
            vars: HashMap::new(),
            config: None,
        };

        let pc = ProxyConfig::default();
        let host = format!("{}.{}.{}", ctx.ws.name, ctx.repo.name, pc.domain);
        assert_eq!(host, "one.toren.lvh.me");
    }

    #[test]
    fn test_parse_proxy_named_port() {
        let content = r#"
setup {
    proxy port=443 upstream="http://localhost:5173" tls=#true
}
"#;

        let config = BreqConfig::parse_kdl(content).unwrap();
        assert_eq!(config.setup.len(), 1);

        match &config.setup[0] {
            Action::Proxy { port, upstream, tls, .. } => {
                assert_eq!(*port, 443);
                match upstream {
                    AttrValue::Literal(s) => assert_eq!(s, "http://localhost:5173"),
                    _ => panic!("Expected Literal upstream"),
                }
                assert_eq!(*tls, Some(true));
            }
            _ => panic!("Expected Proxy action"),
        }
    }

    #[test]
    fn test_proxy_directive_resolution_full_flow() {
        // Simulate the full flow: vars evaluation -> proxy action resolution -> directive
        let content = r#"
vars {
    web_port expr="8000 + ws.num"
    api_port expr="9000 + ws.num"
}
setup {
    proxy port=443 upstream.var="vars.web_port" tls=#true
    proxy port=8080 upstream.var="vars.api_port"
}
"#;

        let config = BreqConfig::parse_kdl(content).unwrap();
        assert_eq!(config.vars.len(), 2);
        assert_eq!(config.setup.len(), 2);

        // Build context for workspace "one" (num=1)
        let mut ctx = WorkspaceContext {
            ws: WorkspaceInfo {
                name: "one".to_string(),
                num: 1,
                path: "/tmp/ws/one".to_string(),
            },
            repo: RepoInfo {
                root: "/tmp/repo".to_string(),
                name: "toren".to_string(),
            },
            task: None,
            vars: HashMap::new(),
            config: Some(ConfigContext {
                proxy: ProxyContext {
                    domain: "lvh.me".to_string(),
                    tls: false,
                },
            }),
        };

        // Evaluate vars
        let vars = evaluate_vars(&config.vars, &ctx).unwrap();
        assert_eq!(vars.get("web_port"), Some(&serde_json::json!(8001)));
        assert_eq!(vars.get("api_port"), Some(&serde_json::json!(9001)));
        ctx.vars = vars;

        // Resolve proxy directives
        let proxy_config = ProxyConfig {
            enabled: true,
            tls: false,
            domain: "lvh.me".to_string(),
            dns_port: None,
        };

        let mut directives = Vec::new();
        for action in &config.setup {
            if let Action::Proxy { port, upstream, tls, host } = action {
                let resolved_upstream = upstream.resolve(&ctx).unwrap();
                let resolved_host = if let Some(h) = host {
                    h.resolve(&ctx).unwrap()
                } else {
                    format!("{}.{}.{}", ctx.ws.name, ctx.repo.name, proxy_config.domain)
                };
                let resolved_tls = tls.unwrap_or(proxy_config.tls);

                directives.push(ProxyDirective {
                    host: resolved_host,
                    upstream: resolved_upstream,
                    tls: resolved_tls,
                    port: *port,
                });
            }
        }

        assert_eq!(directives.len(), 2);

        // First directive: port 443, web_port (8001), tls=true, default host
        assert_eq!(directives[0].port, 443);
        assert_eq!(directives[0].upstream, "8001");
        assert_eq!(directives[0].host, "one.toren.lvh.me");
        assert!(directives[0].tls);

        // Second directive: port 8080, api_port (9001), tls=false (default), default host
        assert_eq!(directives[1].port, 8080);
        assert_eq!(directives[1].upstream, "9001");
        assert_eq!(directives[1].host, "one.toren.lvh.me");
        assert!(!directives[1].tls);
    }
}
