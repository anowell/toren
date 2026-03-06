//! Rhai plugin system for breq.
//!
//! Plugins are Rhai scripts that call native Rust operations via a host API.
//! User plugins are discovered from `~/.toren/plugins/*.rhai`.
//! Community/example plugins live in `contrib/plugins/` and can be copied
//! into the user plugin directory.
//!
//! Plugins are either **commands** (invoked as `breq <name>`) or **resolvers**
//! (provide task data for a source). Resolvers are identified by the
//! `/// @resolver` doc comment tag.

pub mod builtin;
pub mod runtime;

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, warn};

use crate::config::PluginsConfig;
use crate::tasks::TaskInfo;

/// Whether a plugin is a command or a resolver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginKind {
    Command,
    Resolver,
}

/// A loaded plugin (compiled AST with optional doc-comment metadata).
pub struct Plugin {
    pub name: String,
    pub path: PathBuf,
    pub ast: rhai::AST,
    /// First paragraph of `///` doc comments (short description).
    pub description: Option<String>,
    /// Full collected `///` doc-comment text (shown by `--help`).
    pub usage: Option<String>,
    /// Whether this is a command or resolver plugin.
    pub kind: PluginKind,
}

/// Parse leading `///` doc comments from a Rhai source string.
///
/// Returns `(description, usage, is_resolver)` where:
/// - `description` is the first paragraph (lines before a blank `///` line)
/// - `usage` is the full collected text
/// - `is_resolver` is true if `/// @resolver` tag was found
fn parse_doc_comments(source: &str) -> (Option<String>, Option<String>, bool) {
    let mut lines: Vec<String> = Vec::new();
    let mut is_resolver = false;
    for raw in source.lines() {
        let trimmed = raw.trim_start();
        if let Some(rest) = trimmed.strip_prefix("///") {
            // Strip one optional leading space after ///
            let content = rest.strip_prefix(' ').unwrap_or(rest);
            if content.trim() == "@resolver" {
                is_resolver = true;
                continue; // Don't include @resolver tag in description/usage
            }
            lines.push(content.to_string());
        } else {
            break;
        }
    }

    if lines.is_empty() {
        return (None, None, is_resolver);
    }

    let usage = lines.join("\n").trim().to_string();

    // First paragraph: lines before the first blank line
    let description = lines
        .iter()
        .take_while(|l| !l.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();

    let desc = if description.is_empty() {
        None
    } else {
        Some(description)
    };
    let usg = if usage.is_empty() { None } else { Some(usage) };

    (desc, usg, is_resolver)
}

/// The result of running a plugin script.
pub enum PluginResult {
    /// Script completed normally (no deferred action).
    Ok,
    /// Script returned a deferred action for the host to execute.
    Action(DeferredAction),
}

/// A deferred action returned by a plugin script.
///
/// Plugins that need the host to exec into another process (e.g., `claude`)
/// return a map like `#{ action: "do", task_id: "x" }`. The host interprets
/// these after the script completes.
#[derive(Debug, Clone)]
pub enum DeferredAction {
    /// Start a coding agent session via `breq do`.
    Do {
        task_id: Option<String>,
        task_title: Option<String>,
        task_url: Option<String>,
        prompt: Option<String>,
        intent: Option<String>,
    },
}

/// Context passed to the Rhai engine for host function closures.
pub struct PluginContext {
    pub segment_path: Option<PathBuf>,
    pub segment_name: Option<String>,
    /// Resolver ASTs keyed by source name, for use by `toren::task()`.
    pub resolvers: HashMap<String, rhai::AST>,
}

impl PluginContext {
    pub fn new(segment_path: Option<PathBuf>, segment_name: Option<String>) -> Self {
        Self {
            segment_path,
            segment_name,
            resolvers: HashMap::new(),
        }
    }
}

impl Default for PluginContext {
    fn default() -> Self {
        Self::new(None, None)
    }
}

/// Manages plugin discovery, loading, and execution.
pub struct PluginManager {
    commands: HashMap<String, Plugin>,
    resolvers: HashMap<String, Plugin>,
}

impl PluginManager {
    /// Create a new PluginManager: discover user plugins from the configured directory.
    pub fn new(config: &PluginsConfig) -> Result<Self> {
        let mut mgr = Self {
            commands: HashMap::new(),
            resolvers: HashMap::new(),
        };

        // Discover user plugins
        let dir = crate::config::expand_path_str(&config.dir);
        if dir.exists() {
            mgr.load_from_dir(&dir)?;
        }

        // Remove disabled command plugins
        for name in &config.disable {
            if mgr.commands.remove(name).is_some() {
                info!("Plugin '{}' disabled by config", name);
            }
        }

        Ok(mgr)
    }

    /// Check if a command plugin exists by name.
    pub fn has(&self, name: &str) -> bool {
        self.commands.contains_key(name)
    }

    /// List all loaded command plugin names.
    pub fn list(&self) -> Vec<&str> {
        self.commands.keys().map(|s| s.as_str()).collect()
    }

    /// List command plugin names with their short descriptions, sorted by name.
    pub fn list_with_descriptions(&self) -> Vec<(&str, Option<&str>)> {
        let mut items: Vec<_> = self
            .commands
            .iter()
            .map(|(name, p)| (name.as_str(), p.description.as_deref()))
            .collect();
        items.sort_by_key(|(name, _)| *name);
        items
    }

    /// Get the full usage text for a command plugin (from `///` doc comments).
    pub fn usage(&self, name: &str) -> Option<&str> {
        self.commands.get(name).and_then(|p| p.usage.as_deref())
    }

    /// Run a command plugin by name with the given arguments and context.
    pub fn run(&self, name: &str, args: &[String], mut ctx: PluginContext) -> Result<PluginResult> {
        let plugin = self
            .commands
            .get(name)
            .with_context(|| format!("Plugin '{}' not found", name))?;

        // Inject resolver ASTs so command plugins calling toren::task() get resolver-backed data
        ctx.resolvers = self.resolver_asts();
        let ctx = Arc::new(ctx);
        let engine = runtime::create_engine(ctx);

        runtime::run_ast(&engine, &plugin.ast, args)
    }

    // ── Resolver methods ─────────────────────────────────────────────

    /// Check if a resolver exists for the given source name.
    pub fn has_resolver(&self, source: &str) -> bool {
        self.resolvers.contains_key(source)
    }

    /// List all loaded resolver source names.
    pub fn list_resolvers(&self) -> Vec<&str> {
        self.resolvers.keys().map(|s| s.as_str()).collect()
    }

    /// Check if a resolver has a specific function defined.
    pub fn resolver_has_fn(&self, source: &str, fn_name: &str) -> bool {
        self.resolvers
            .get(source)
            .map(|p| p.ast.iter_functions().any(|f| f.name == fn_name))
            .unwrap_or(false)
    }

    /// Fetch a task via a resolver plugin.
    ///
    /// Calls the resolver's `fetch(id)` function and converts the result to a Task.
    pub fn resolve_fetch(
        &self,
        source: &str,
        id: &str,
        ctx: PluginContext,
    ) -> Result<crate::tasks::Task> {
        let map = self.call_resolver_map(source, "fetch", (id.to_string(),), ctx)?;

        Ok(crate::tasks::Task {
            id: get_map_string(&map, "id").unwrap_or_else(|| id.to_string()),
            title: get_map_string(&map, "title").unwrap_or_default(),
            description: get_map_string(&map, "description"),
            source: source.to_string(),
        })
    }

    /// Fetch task info (status/assignee) via a resolver plugin.
    ///
    /// Calls the resolver's `info(id)` function.
    pub fn resolve_info(
        &self,
        source: &str,
        id: &str,
        ctx: PluginContext,
    ) -> Result<TaskInfo> {
        let map = self.call_resolver_map(source, "info", (id.to_string(),), ctx)?;

        Ok(TaskInfo {
            id: get_map_string(&map, "id").unwrap_or_else(|| id.to_string()),
            title: get_map_string(&map, "title").unwrap_or_default(),
            status: get_map_string(&map, "status").unwrap_or_else(|| "open".to_string()),
            assignee: get_map_string(&map, "assignee").unwrap_or_default(),
        })
    }

    /// Claim a task via a resolver plugin.
    pub fn resolve_claim(
        &self,
        source: &str,
        id: &str,
        assignee: &str,
        ctx: PluginContext,
    ) -> Result<()> {
        let _ = self.call_resolver_raw(
            source,
            "claim",
            (id.to_string(), assignee.to_string()),
            ctx,
        )?;
        Ok(())
    }

    /// Complete a task via a resolver plugin.
    pub fn resolve_complete(&self, source: &str, id: &str, ctx: PluginContext) -> Result<()> {
        let _ = self.call_resolver_raw(source, "complete", (id.to_string(),), ctx)?;
        Ok(())
    }

    /// Abort a task via a resolver plugin.
    pub fn resolve_abort(&self, source: &str, id: &str, ctx: PluginContext) -> Result<()> {
        let _ = self.call_resolver_raw(source, "abort", (id.to_string(),), ctx)?;
        Ok(())
    }

    /// Create a task via a resolver plugin. Returns the created task ID.
    pub fn resolve_create(
        &self,
        source: &str,
        title: &str,
        desc: Option<&str>,
        ctx: PluginContext,
    ) -> Result<String> {
        let desc_arg = match desc {
            Some(d) => rhai::Dynamic::from(d.to_string()),
            None => rhai::Dynamic::UNIT,
        };
        let result = self.call_resolver_raw(
            source,
            "create",
            (title.to_string(), desc_arg),
            ctx,
        )?;
        Ok(result.into_string().unwrap_or_default())
    }

    /// Call a resolver function, returning the raw Dynamic result.
    fn call_resolver_raw<A: rhai::FuncArgs>(
        &self,
        source: &str,
        fn_name: &str,
        args: A,
        ctx: PluginContext,
    ) -> Result<rhai::Dynamic> {
        let resolver = self
            .resolvers
            .get(source)
            .with_context(|| format!("No resolver found for source '{}'", source))?;

        let ctx = Arc::new(ctx);
        let engine = runtime::create_resolver_engine(ctx);
        let mut scope = rhai::Scope::new();

        engine
            .call_fn::<rhai::Dynamic>(&mut scope, &resolver.ast, fn_name, args)
            .map_err(|e| anyhow::anyhow!("Resolver '{}' {} error: {}", source, fn_name, e))
    }

    /// Call a resolver function that is expected to return a Map.
    fn call_resolver_map<A: rhai::FuncArgs>(
        &self,
        source: &str,
        fn_name: &str,
        args: A,
        ctx: PluginContext,
    ) -> Result<rhai::Map> {
        let result = self.call_resolver_raw(source, fn_name, args, ctx)?;

        result
            .try_cast::<rhai::Map>()
            .ok_or_else(|| anyhow::anyhow!("Resolver '{}' {} did not return a map", source, fn_name))
    }

    /// Get a clone of all resolver ASTs (for injection into PluginContext).
    fn resolver_asts(&self) -> HashMap<String, rhai::AST> {
        self.resolvers
            .iter()
            .map(|(name, p)| (name.clone(), p.ast.clone()))
            .collect()
    }

    /// Discover and load `.rhai` scripts from a directory.
    fn load_from_dir(&mut self, dir: &Path) -> Result<()> {
        let entries = std::fs::read_dir(dir)
            .with_context(|| format!("Failed to read plugin directory: {}", dir.display()))?;

        let engine = rhai::Engine::new();

        for entry in entries {
            let entry = entry?;
            let path = entry.path();

            if path.extension().and_then(|s| s.to_str()) != Some("rhai") {
                continue;
            }

            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();

            if name.is_empty() {
                continue;
            }

            match std::fs::read_to_string(&path) {
                Ok(source) => match engine.compile(&source) {
                    Ok(ast) => {
                        let (description, usage, is_resolver) = parse_doc_comments(&source);
                        let kind = if is_resolver {
                            PluginKind::Resolver
                        } else {
                            PluginKind::Command
                        };
                        info!("Loaded {:?} plugin '{}' from {}", kind, name, path.display());
                        let plugin = Plugin {
                            name: name.clone(),
                            path: path.clone(),
                            ast,
                            description,
                            usage,
                            kind,
                        };
                        match kind {
                            PluginKind::Command => {
                                self.commands.insert(name, plugin);
                            }
                            PluginKind::Resolver => {
                                self.resolvers.insert(name, plugin);
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            "Failed to compile plugin '{}' ({}): {}",
                            name,
                            path.display(),
                            e
                        );
                    }
                },
                Err(e) => {
                    warn!(
                        "Failed to read plugin '{}' ({}): {}",
                        name,
                        path.display(),
                        e
                    );
                }
            }
        }

        Ok(())
    }
}

/// Extract a string value from a Rhai Map, returning None for unit values.
fn get_map_string(map: &rhai::Map, key: &str) -> Option<String> {
    map.get(key).and_then(|v| {
        if v.is::<()>() {
            None
        } else {
            v.clone().into_string().ok()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_dir_no_plugins() {
        let config = PluginsConfig {
            dir: "/nonexistent".to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();
        assert!(mgr.commands.is_empty());
        assert!(mgr.resolvers.is_empty());
    }

    #[test]
    fn test_disabled_plugins_removed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("foo.rhai"), r#"let x = 1;"#).unwrap();
        std::fs::write(dir.path().join("bar.rhai"), r#"let x = 2;"#).unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: vec!["foo".to_string()],
        };
        let mgr = PluginManager::new(&config).unwrap();
        assert!(!mgr.has("foo"));
        assert!(mgr.has("bar"));
    }

    #[test]
    fn test_user_plugin_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_path = dir.path().join("custom.rhai");
        std::fs::write(&plugin_path, r#"let x = "hello";"#).unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();
        assert!(mgr.has("custom"));
    }

    #[test]
    fn test_invalid_plugin_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_path = dir.path().join("bad.rhai");
        std::fs::write(&plugin_path, r#"this is not valid rhai {{{"#).unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();
        assert!(!mgr.has("bad"));
    }

    #[test]
    fn test_non_rhai_files_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("readme.md"), "# Plugins").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "notes").unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();
        assert!(mgr.commands.is_empty());
        assert!(mgr.resolvers.is_empty());
    }

    #[test]
    fn test_contrib_plugins_compile() {
        // Verify the contrib plugin scripts are valid Rhai
        let engine = rhai::Engine::new();
        let contrib_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("contrib/plugins");

        for name in &["assign", "complete", "abort", "beads"] {
            let path = contrib_dir.join(format!("{}.rhai", name));
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|_| panic!("Failed to read {}", path.display()));
            engine
                .compile(&source)
                .unwrap_or_else(|e| panic!("Failed to compile {}: {}", name, e));
        }
    }

    #[test]
    fn test_doc_comments_basic() {
        let source =
            "/// Short description.\n///\n/// Detailed usage text\n/// spanning lines.\nlet x = 1;";
        let (desc, usage, is_resolver) = parse_doc_comments(source);
        assert_eq!(desc.as_deref(), Some("Short description."));
        assert_eq!(
            usage.as_deref(),
            Some("Short description.\n\nDetailed usage text\nspanning lines.")
        );
        assert!(!is_resolver);
    }

    #[test]
    fn test_doc_comments_missing() {
        let source = "// regular comment\nlet x = 1;";
        let (desc, usage, _) = parse_doc_comments(source);
        assert!(desc.is_none());
        assert!(usage.is_none());
    }

    #[test]
    fn test_doc_comments_single_paragraph() {
        let source = "/// Just a one-liner.\nlet x = 1;";
        let (desc, usage, _) = parse_doc_comments(source);
        assert_eq!(desc.as_deref(), Some("Just a one-liner."));
        assert_eq!(usage.as_deref(), Some("Just a one-liner."));
    }

    #[test]
    fn test_doc_comments_no_space_after_slashes() {
        let source = "///No space here.\nlet x = 1;";
        let (desc, _, _) = parse_doc_comments(source);
        assert_eq!(desc.as_deref(), Some("No space here."));
    }

    #[test]
    fn test_list_with_descriptions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("beta.rhai"),
            "/// Beta plugin.\nlet x = 1;",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("alpha.rhai"),
            "/// Alpha plugin.\nlet x = 1;",
        )
        .unwrap();
        std::fs::write(dir.path().join("gamma.rhai"), "let x = 1;").unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();
        let list = mgr.list_with_descriptions();
        // Should be sorted by name
        assert_eq!(list[0].0, "alpha");
        assert_eq!(list[0].1, Some("Alpha plugin."));
        assert_eq!(list[1].0, "beta");
        assert_eq!(list[1].1, Some("Beta plugin."));
        assert_eq!(list[2].0, "gamma");
        assert_eq!(list[2].1, None);
    }

    #[test]
    fn test_deferred_action_from_script() {
        let engine = rhai::Engine::new();
        let ast = engine
            .compile(r#"#{ action: "do", task_id: "test-123", task_title: "Test task" }"#)
            .unwrap();

        let result = runtime::run_ast(&engine, &ast, &[]).unwrap();
        match result {
            PluginResult::Action(DeferredAction::Do {
                task_id,
                task_title,
                ..
            }) => {
                assert_eq!(task_id.as_deref(), Some("test-123"));
                assert_eq!(task_title.as_deref(), Some("Test task"));
            }
            _ => panic!("Expected DeferredAction::Do"),
        }
    }

    // ── Resolver tests ──────────────────────────────────────────────

    #[test]
    fn test_resolver_detection_in_doc_comments() {
        let source = "/// @resolver\n/// My resolver plugin.\nfn fetch(id) { #{} }";
        let (desc, _, is_resolver) = parse_doc_comments(source);
        assert!(is_resolver);
        assert_eq!(desc.as_deref(), Some("My resolver plugin."));
    }

    #[test]
    fn test_resolver_detection_not_present() {
        let source = "/// Regular command plugin.\nlet x = 1;";
        let (_, _, is_resolver) = parse_doc_comments(source);
        assert!(!is_resolver);
    }

    #[test]
    fn test_resolver_routed_to_resolvers_map() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("myresolver.rhai"),
            "/// @resolver\n/// Test resolver.\nfn fetch(id) { #{} }",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("mycmd.rhai"),
            "/// Test command.\nlet x = 1;",
        )
        .unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();

        // Command should be in commands, not resolvers
        assert!(mgr.has("mycmd"));
        assert!(!mgr.has_resolver("mycmd"));

        // Resolver should be in resolvers, not commands
        assert!(!mgr.has("myresolver"));
        assert!(mgr.has_resolver("myresolver"));
    }

    #[test]
    fn test_resolver_has_fn() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("test_src.rhai"),
            "/// @resolver\nfn fetch(id) { #{} }\nfn info(id) { #{} }",
        )
        .unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();

        assert!(mgr.resolver_has_fn("test_src", "fetch"));
        assert!(mgr.resolver_has_fn("test_src", "info"));
        assert!(!mgr.resolver_has_fn("test_src", "create"));
        assert!(!mgr.resolver_has_fn("nonexistent", "fetch"));
    }

    #[test]
    fn test_list_resolvers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("alpha.rhai"),
            "/// @resolver\nfn fetch(id) { #{} }",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("beta.rhai"),
            "/// @resolver\nfn fetch(id) { #{} }",
        )
        .unwrap();
        std::fs::write(dir.path().join("cmd.rhai"), "let x = 1;").unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();

        let mut resolvers = mgr.list_resolvers();
        resolvers.sort();
        assert_eq!(resolvers, vec!["alpha", "beta"]);
        assert_eq!(mgr.list(), vec!["cmd"]);
    }

    #[test]
    fn test_resolve_fetch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("mock.rhai"),
            r#"/// @resolver
fn fetch(id) {
    #{ id: id, title: "Task " + id, description: "Desc for " + id }
}"#,
        )
        .unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();
        let ctx = PluginContext::default();
        let task = mgr.resolve_fetch("mock", "abc-123", ctx).unwrap();

        assert_eq!(task.id, "abc-123");
        assert_eq!(task.title, "Task abc-123");
        assert_eq!(task.description.as_deref(), Some("Desc for abc-123"));
        assert_eq!(task.source, "mock");
    }

    #[test]
    fn test_resolve_info() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("mock.rhai"),
            r#"/// @resolver
fn info(id) {
    #{ id: id, title: "Task", status: "in_progress", assignee: "claude" }
}"#,
        )
        .unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();
        let ctx = PluginContext::default();
        let info = mgr.resolve_info("mock", "test-1", ctx).unwrap();

        assert_eq!(info.id, "test-1");
        assert_eq!(info.status, "in_progress");
        assert_eq!(info.assignee, "claude");
    }

    #[test]
    fn test_resolve_create() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("mock.rhai"),
            r#"/// @resolver
fn create(title, desc) {
    "new-id-42"
}"#,
        )
        .unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();
        let ctx = PluginContext::default();
        let id = mgr
            .resolve_create("mock", "My Task", Some("desc"), ctx)
            .unwrap();

        assert_eq!(id, "new-id-42");
    }

    #[test]
    fn test_resolve_missing_resolver_errors() {
        let config = PluginsConfig {
            dir: "/nonexistent".to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();
        let ctx = PluginContext::default();

        assert!(mgr.resolve_fetch("nonexistent", "id", ctx).is_err());
    }

    #[test]
    fn test_resolver_asts_injected_in_run() {
        let dir = tempfile::tempdir().unwrap();
        // A resolver plugin
        std::fs::write(
            dir.path().join("mock_src.rhai"),
            "/// @resolver\nfn fetch(id) { #{ id: id, title: \"resolved\" } }",
        )
        .unwrap();
        // A command plugin that just returns ok
        std::fs::write(dir.path().join("cmd.rhai"), "let x = 1;").unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();

        // Verify resolver ASTs are generated correctly
        let asts = mgr.resolver_asts();
        assert!(asts.contains_key("mock_src"));
    }
}
