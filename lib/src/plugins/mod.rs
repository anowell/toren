//! Rhai plugin system for breq.
//!
//! Plugins are Rhai scripts that call native Rust operations via a host API.
//! User plugins are discovered from `~/.toren/plugins/`.
//! Community/example plugins live in `contrib/plugins/` and can be copied
//! into the user plugin directory.
//!
//! Plugins are either **commands** (in `commands/` subdir, invoked as `breq <name>`)
//! or **task resolvers** (in `tasks/` subdir, provide task data for a source).
//!
//! For backwards compatibility, if neither `commands/` nor `tasks/` subdirectory
//! exists, flat directory scanning with `/// @resolver` tag detection is used.
//!
//! Compilation is lazy: plugins are scanned for metadata on init but only
//! compiled to AST when actually executed.

pub mod builtin;
pub mod runtime;

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

use crate::config::PluginsConfig;
use crate::tasks::ResolvedTask;

/// Lightweight metadata extracted from a plugin file without compilation.
#[derive(Debug, Clone)]
pub struct PluginMeta {
    pub name: String,
    pub path: PathBuf,
    /// First paragraph of `///` doc comments (short description).
    pub description: Option<String>,
    /// Full collected `///` doc-comment text (shown by `--help`).
    pub usage: Option<String>,
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
    /// Ordered list of task sources for multi-source resolution.
    pub task_sources: Vec<String>,
}

impl PluginContext {
    pub fn new(segment_path: Option<PathBuf>, segment_name: Option<String>) -> Self {
        Self {
            segment_path,
            segment_name,
            resolvers: HashMap::new(),
            task_sources: Vec::new(),
        }
    }
}

impl Default for PluginContext {
    fn default() -> Self {
        Self::new(None, None)
    }
}

/// Manages plugin discovery, loading, and execution.
///
/// Plugins are scanned for metadata (description, usage) on init without
/// compilation. ASTs are compiled lazily on first use and cached.
pub struct PluginManager {
    command_metas: HashMap<String, PluginMeta>,
    resolver_metas: HashMap<String, PluginMeta>,
    /// Lazily compiled ASTs, keyed by plugin path.
    compiled: Mutex<HashMap<PathBuf, rhai::AST>>,
}

impl PluginManager {
    /// Create a new PluginManager: scan plugins from the configured directory
    /// without compiling them.
    pub fn new(config: &PluginsConfig) -> Result<Self> {
        let mut mgr = Self {
            command_metas: HashMap::new(),
            resolver_metas: HashMap::new(),
            compiled: Mutex::new(HashMap::new()),
        };

        let dir = crate::config::expand_path_str(&config.dir);
        if dir.exists() {
            let commands_dir = dir.join("commands");
            let tasks_dir = dir.join("tasks");

            if commands_dir.exists() || tasks_dir.exists() {
                // Semantic directory structure
                if commands_dir.exists() {
                    mgr.scan_dir(&commands_dir, &mut Vec::new(), false)?;
                }
                if tasks_dir.exists() {
                    mgr.scan_dir(&tasks_dir, &mut Vec::new(), true)?;
                }
            } else {
                // Legacy flat directory with @resolver tag detection
                mgr.scan_dir_legacy(&dir)?;
            }
        }

        // Remove disabled command plugins
        for name in &config.disable {
            if mgr.command_metas.remove(name).is_some() {
                info!("Plugin '{}' disabled by config", name);
            }
        }

        Ok(mgr)
    }

    /// Check if a command plugin exists by name.
    pub fn has(&self, name: &str) -> bool {
        self.command_metas.contains_key(name)
    }

    /// List all command plugin names.
    pub fn list(&self) -> Vec<&str> {
        self.command_metas.keys().map(|s| s.as_str()).collect()
    }

    /// List command plugin names with their short descriptions, sorted by name.
    pub fn list_with_descriptions(&self) -> Vec<(&str, Option<&str>)> {
        let mut items: Vec<_> = self
            .command_metas
            .iter()
            .map(|(name, m)| (name.as_str(), m.description.as_deref()))
            .collect();
        items.sort_by_key(|(name, _)| *name);
        items
    }

    /// Get the full usage text for a command plugin (from `///` doc comments).
    pub fn usage(&self, name: &str) -> Option<&str> {
        self.command_metas
            .get(name)
            .and_then(|m| m.usage.as_deref())
    }

    /// Run a command plugin by name with the given arguments and context.
    pub fn run(&self, name: &str, args: &[String], mut ctx: PluginContext) -> Result<PluginResult> {
        let meta = self
            .command_metas
            .get(name)
            .with_context(|| format!("Plugin '{}' not found", name))?;

        let ast = self.compile(meta)?;

        // Inject resolver ASTs so command plugins calling toren::task() get resolver-backed data
        ctx.resolvers = self.resolver_asts();
        let ctx = Arc::new(ctx);
        let engine = runtime::create_engine(ctx);

        runtime::run_ast(&engine, &ast, args)
    }

    // ── Resolver methods ─────────────────────────────────────────────

    /// Check if a resolver exists for the given source name.
    pub fn has_resolver(&self, source: &str) -> bool {
        self.resolver_metas.contains_key(source)
    }

    /// List all resolver source names.
    pub fn list_resolvers(&self) -> Vec<&str> {
        self.resolver_metas.keys().map(|s| s.as_str()).collect()
    }

    /// Check if a resolver has a specific function defined.
    pub fn resolver_has_fn(&self, source: &str, fn_name: &str) -> bool {
        self.resolver_metas
            .get(source)
            .and_then(|m| self.compile(m).ok())
            .map(|ast| ast.iter_functions().any(|f| f.name == fn_name))
            .unwrap_or(false)
    }

    /// Resolve task info via a resolver plugin's `info(id)` function.
    ///
    /// Returns a unified `ResolvedTask` with all available fields.
    pub fn resolve_info(
        &self,
        source: &str,
        id: &str,
        ctx: PluginContext,
    ) -> Result<ResolvedTask> {
        let map = self.call_resolver_map(source, "info", (id.to_string(),), ctx)?;

        Ok(ResolvedTask {
            id: get_map_string(&map, "id").unwrap_or_else(|| id.to_string()),
            source: source.to_string(),
            kind: get_map_string(&map, "kind"),
            title: get_map_string(&map, "title").unwrap_or_default(),
            status: get_map_string(&map, "status"),
            assignee: get_map_string(&map, "assignee"),
            description: get_map_string(&map, "description"),
            created_at: get_map_string(&map, "created_at"),
            updated_at: get_map_string(&map, "updated_at"),
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

    /// Determine effective task sources: use config sources if non-empty,
    /// otherwise all installed task plugins.
    pub fn effective_sources(&self, config_sources: &[String]) -> Vec<String> {
        if config_sources.is_empty() {
            self.list_resolvers().iter().map(|s| s.to_string()).collect()
        } else {
            config_sources.to_vec()
        }
    }

    /// Try resolvers in source order until one succeeds.
    pub fn resolve_info_multi(
        &self,
        sources: &[String],
        id: &str,
        ctx: PluginContext,
    ) -> Result<ResolvedTask> {
        let available: Vec<_> = sources
            .iter()
            .filter(|s| self.has_resolver(s))
            .collect();
        if available.is_empty() {
            anyhow::bail!("No task resolvers available (tried: {:?})", sources);
        }
        let mut last_err = None;
        for source in &available {
            let ctx = PluginContext::new(ctx.segment_path.clone(), ctx.segment_name.clone());
            match self.resolve_info(source, id, ctx) {
                Ok(info) => return Ok(info),
                Err(e) => {
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap())
    }

    /// Call a resolver function, returning the raw Dynamic result.
    fn call_resolver_raw<A: rhai::FuncArgs>(
        &self,
        source: &str,
        fn_name: &str,
        args: A,
        ctx: PluginContext,
    ) -> Result<rhai::Dynamic> {
        let meta = self
            .resolver_metas
            .get(source)
            .with_context(|| format!("No resolver found for source '{}'", source))?;

        let ast = self.compile(meta)?;

        let ctx = Arc::new(ctx);
        let engine = runtime::create_resolver_engine(ctx);
        let mut scope = rhai::Scope::new();

        engine
            .call_fn::<rhai::Dynamic>(&mut scope, &ast, fn_name, args)
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

    /// Compile a plugin on demand, caching the result.
    fn compile(&self, meta: &PluginMeta) -> Result<rhai::AST> {
        {
            let cache = self.compiled.lock().unwrap();
            if let Some(ast) = cache.get(&meta.path) {
                return Ok(ast.clone());
            }
        }
        let source = std::fs::read_to_string(&meta.path)
            .with_context(|| format!("Failed to read plugin '{}' ({})", meta.name, meta.path.display()))?;
        let engine = rhai::Engine::new();
        let ast = engine.compile(&source)
            .map_err(|e| anyhow::anyhow!("Failed to compile plugin '{}' ({}): {}", meta.name, meta.path.display(), e))?;
        let mut cache = self.compiled.lock().unwrap();
        cache.insert(meta.path.clone(), ast.clone());
        Ok(ast)
    }

    /// Get a clone of all resolver ASTs (for injection into PluginContext).
    /// Compiles any resolvers that haven't been compiled yet.
    fn resolver_asts(&self) -> HashMap<String, rhai::AST> {
        self.resolver_metas
            .iter()
            .filter_map(|(name, meta)| {
                self.compile(meta).ok().map(|ast| (name.clone(), ast))
            })
            .collect()
    }

    /// Scan a directory for `.rhai` plugin files, extracting metadata only.
    ///
    /// If `is_resolver` is true, metas are added to `resolver_metas`;
    /// otherwise to `command_metas`.
    fn scan_dir(
        &mut self,
        dir: &Path,
        _errors: &mut Vec<String>,
        is_resolver: bool,
    ) -> Result<()> {
        let entries = std::fs::read_dir(dir)
            .with_context(|| format!("Failed to read plugin directory: {}", dir.display()))?;

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
                Ok(source) => {
                    let (description, usage, _) = parse_doc_comments(&source);
                    let kind_label = if is_resolver { "resolver" } else { "command" };
                    info!("Scanned {} plugin '{}' from {}", kind_label, name, path.display());
                    let meta = PluginMeta {
                        name: name.clone(),
                        path,
                        description,
                        usage,
                    };
                    if is_resolver {
                        self.resolver_metas.insert(name, meta);
                    } else {
                        self.command_metas.insert(name, meta);
                    }
                }
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

    /// Legacy flat directory scan: route to commands or resolvers based on
    /// `/// @resolver` doc comment tag.
    fn scan_dir_legacy(&mut self, dir: &Path) -> Result<()> {
        let entries = std::fs::read_dir(dir)
            .with_context(|| format!("Failed to read plugin directory: {}", dir.display()))?;

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
                Ok(source) => {
                    let (description, usage, is_resolver) = parse_doc_comments(&source);
                    let kind_label = if is_resolver { "resolver" } else { "command" };
                    info!("Scanned {} plugin '{}' from {} (legacy)", kind_label, name, path.display());
                    let meta = PluginMeta {
                        name: name.clone(),
                        path,
                        description,
                        usage,
                    };
                    if is_resolver {
                        self.resolver_metas.insert(name, meta);
                    } else {
                        self.command_metas.insert(name, meta);
                    }
                }
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
        assert!(mgr.command_metas.is_empty());
        assert!(mgr.resolver_metas.is_empty());
    }

    #[test]
    fn test_disabled_plugins_removed() {
        let dir = tempfile::tempdir().unwrap();
        let cmd_dir = dir.path().join("commands");
        std::fs::create_dir_all(&cmd_dir).unwrap();
        std::fs::write(cmd_dir.join("foo.rhai"), r#"let x = 1;"#).unwrap();
        std::fs::write(cmd_dir.join("bar.rhai"), r#"let x = 2;"#).unwrap();

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
        let cmd_dir = dir.path().join("commands");
        std::fs::create_dir_all(&cmd_dir).unwrap();
        std::fs::write(cmd_dir.join("custom.rhai"), r#"let x = "hello";"#).unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();
        assert!(mgr.has("custom"));
    }

    #[test]
    fn test_invalid_plugin_detected_on_compile() {
        let dir = tempfile::tempdir().unwrap();
        let cmd_dir = dir.path().join("commands");
        std::fs::create_dir_all(&cmd_dir).unwrap();
        std::fs::write(cmd_dir.join("bad.rhai"), r#"this is not valid rhai {{{"#).unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();
        // Meta is scanned (plugin is "known") but compilation will fail
        assert!(mgr.has("bad"));
        // Compilation should fail
        let meta = mgr.command_metas.get("bad").unwrap();
        assert!(mgr.compile(meta).is_err());
    }

    #[test]
    fn test_non_rhai_files_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let cmd_dir = dir.path().join("commands");
        std::fs::create_dir_all(&cmd_dir).unwrap();
        std::fs::write(cmd_dir.join("readme.md"), "# Plugins").unwrap();
        std::fs::write(cmd_dir.join("notes.txt"), "notes").unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();
        assert!(mgr.command_metas.is_empty());
        assert!(mgr.resolver_metas.is_empty());
    }

    #[test]
    fn test_contrib_plugins_compile() {
        // Verify the contrib plugin scripts are valid Rhai
        let engine = rhai::Engine::new();
        let contrib_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("contrib/plugins");

        // Commands are in commands/ subdir
        for name in &["assign", "complete", "abort"] {
            let path = contrib_dir.join(format!("commands/{}.rhai", name));
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|_| panic!("Failed to read {}", path.display()));
            engine
                .compile(&source)
                .unwrap_or_else(|e| panic!("Failed to compile {}: {}", name, e));
        }

        // Resolvers are in tasks/ subdir
        for name in &["beads"] {
            let path = contrib_dir.join(format!("tasks/{}.rhai", name));
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
        let cmd_dir = dir.path().join("commands");
        std::fs::create_dir_all(&cmd_dir).unwrap();
        std::fs::write(
            cmd_dir.join("beta.rhai"),
            "/// Beta plugin.\nlet x = 1;",
        )
        .unwrap();
        std::fs::write(
            cmd_dir.join("alpha.rhai"),
            "/// Alpha plugin.\nlet x = 1;",
        )
        .unwrap();
        std::fs::write(cmd_dir.join("gamma.rhai"), "let x = 1;").unwrap();

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
    fn test_semantic_dir_routing() {
        let dir = tempfile::tempdir().unwrap();
        let cmd_dir = dir.path().join("commands");
        let tasks_dir = dir.path().join("tasks");
        std::fs::create_dir_all(&cmd_dir).unwrap();
        std::fs::create_dir_all(&tasks_dir).unwrap();

        std::fs::write(
            tasks_dir.join("myresolver.rhai"),
            "/// Test resolver.\nfn fetch(id) { #{} }",
        )
        .unwrap();
        std::fs::write(
            cmd_dir.join("mycmd.rhai"),
            "/// Test command.\nlet x = 1;",
        )
        .unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();

        assert!(mgr.has("mycmd"));
        assert!(!mgr.has_resolver("mycmd"));

        assert!(!mgr.has("myresolver"));
        assert!(mgr.has_resolver("myresolver"));
    }

    #[test]
    fn test_legacy_flat_dir_fallback() {
        let dir = tempfile::tempdir().unwrap();
        // No commands/ or tasks/ subdirs — should use legacy mode
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

        assert!(mgr.has("mycmd"));
        assert!(!mgr.has_resolver("mycmd"));

        assert!(!mgr.has("myresolver"));
        assert!(mgr.has_resolver("myresolver"));
    }

    #[test]
    fn test_resolver_has_fn() {
        let dir = tempfile::tempdir().unwrap();
        let tasks_dir = dir.path().join("tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();
        std::fs::write(
            tasks_dir.join("test_src.rhai"),
            "fn fetch(id) { #{} }\nfn info(id) { #{} }",
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
        let tasks_dir = dir.path().join("tasks");
        let cmd_dir = dir.path().join("commands");
        std::fs::create_dir_all(&tasks_dir).unwrap();
        std::fs::create_dir_all(&cmd_dir).unwrap();
        std::fs::write(
            tasks_dir.join("alpha.rhai"),
            "fn fetch(id) { #{} }",
        )
        .unwrap();
        std::fs::write(
            tasks_dir.join("beta.rhai"),
            "fn fetch(id) { #{} }",
        )
        .unwrap();
        std::fs::write(cmd_dir.join("cmd.rhai"), "let x = 1;").unwrap();

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
    fn test_resolve_info() {
        let dir = tempfile::tempdir().unwrap();
        let tasks_dir = dir.path().join("tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();
        std::fs::write(
            tasks_dir.join("mock.rhai"),
            r#"fn info(id) {
    #{ id: id, title: "Task " + id, description: "Desc for " + id, status: "in_progress", assignee: "claude" }
}"#,
        )
        .unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();
        let ctx = PluginContext::default();
        let info = mgr.resolve_info("mock", "abc-123", ctx).unwrap();

        assert_eq!(info.id, "abc-123");
        assert_eq!(info.title, "Task abc-123");
        assert_eq!(info.description.as_deref(), Some("Desc for abc-123"));
        assert_eq!(info.source, "mock");
        assert_eq!(info.status.as_deref(), Some("in_progress"));
        assert_eq!(info.assignee.as_deref(), Some("claude"));
    }

    #[test]
    fn test_resolve_create() {
        let dir = tempfile::tempdir().unwrap();
        let tasks_dir = dir.path().join("tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();
        std::fs::write(
            tasks_dir.join("mock.rhai"),
            r#"fn create(title, desc) {
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
    fn test_resolve_claim() {
        let dir = tempfile::tempdir().unwrap();
        let tasks_dir = dir.path().join("tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();
        std::fs::write(
            tasks_dir.join("mock.rhai"),
            r#"fn claim(id, assignee) {
    // claim succeeds silently
}"#,
        )
        .unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();
        let ctx = PluginContext::default();
        mgr.resolve_claim("mock", "abc-123", "claude", ctx).unwrap();
    }

    #[test]
    fn test_resolve_complete_and_abort() {
        let dir = tempfile::tempdir().unwrap();
        let tasks_dir = dir.path().join("tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();
        std::fs::write(
            tasks_dir.join("mock.rhai"),
            r#"fn complete(id) { }
fn abort(id) { }"#,
        )
        .unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();

        let ctx = PluginContext::default();
        mgr.resolve_complete("mock", "abc-123", ctx).unwrap();

        let ctx = PluginContext::default();
        mgr.resolve_abort("mock", "abc-123", ctx).unwrap();
    }

    #[test]
    fn test_resolve_missing_resolver_errors() {
        let config = PluginsConfig {
            dir: "/nonexistent".to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();
        let ctx = PluginContext::default();

        assert!(mgr.resolve_info("nonexistent", "id", ctx).is_err());
    }

    #[test]
    fn test_resolver_asts_injected_in_run() {
        let dir = tempfile::tempdir().unwrap();
        let cmd_dir = dir.path().join("commands");
        let tasks_dir = dir.path().join("tasks");
        std::fs::create_dir_all(&cmd_dir).unwrap();
        std::fs::create_dir_all(&tasks_dir).unwrap();
        // A resolver plugin
        std::fs::write(
            tasks_dir.join("mock_src.rhai"),
            "fn info(id) { #{ id: id, title: \"resolved\" } }",
        )
        .unwrap();
        // A command plugin that just returns ok
        std::fs::write(cmd_dir.join("cmd.rhai"), "let x = 1;").unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();

        // Verify resolver ASTs are generated correctly
        let asts = mgr.resolver_asts();
        assert!(asts.contains_key("mock_src"));
    }

    #[test]
    fn test_scan_no_compilation() {
        let dir = tempfile::tempdir().unwrap();
        let cmd_dir = dir.path().join("commands");
        std::fs::create_dir_all(&cmd_dir).unwrap();
        std::fs::write(cmd_dir.join("foo.rhai"), "let x = 1;").unwrap();
        std::fs::write(cmd_dir.join("bar.rhai"), "let x = 2;").unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();

        // Plugins are known but no ASTs compiled yet
        assert!(mgr.has("foo"));
        assert!(mgr.has("bar"));
        let cache = mgr.compiled.lock().unwrap();
        assert!(cache.is_empty(), "No ASTs should be compiled on init");
    }

    #[test]
    fn test_lazy_compile_on_run() {
        let dir = tempfile::tempdir().unwrap();
        let cmd_dir = dir.path().join("commands");
        std::fs::create_dir_all(&cmd_dir).unwrap();
        std::fs::write(cmd_dir.join("hello.rhai"), r#"let x = "hi";"#).unwrap();
        std::fs::write(cmd_dir.join("world.rhai"), r#"let y = "wo";"#).unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();

        // Nothing compiled yet
        assert!(mgr.compiled.lock().unwrap().is_empty());

        // Run one plugin
        let ctx = PluginContext::default();
        let _ = mgr.run("hello", &[], ctx);

        // Only the executed plugin should be compiled
        let cache = mgr.compiled.lock().unwrap();
        assert_eq!(cache.len(), 1);
        assert!(cache.contains_key(&cmd_dir.join("hello.rhai")));
    }

    #[test]
    fn test_multi_source_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let tasks_dir = dir.path().join("tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();

        // First resolver fails (no info function)
        std::fs::write(
            tasks_dir.join("failing.rhai"),
            r#"fn fetch(id) { #{} }"#,
        )
        .unwrap();

        // Second resolver succeeds
        std::fs::write(
            tasks_dir.join("working.rhai"),
            r#"fn info(id) { #{ id: id, title: "Found: " + id } }"#,
        )
        .unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();

        let sources = vec!["failing".to_string(), "working".to_string()];
        let ctx = PluginContext::default();
        let task = mgr.resolve_info_multi(&sources, "test-1", ctx).unwrap();

        assert_eq!(task.id, "test-1");
        assert_eq!(task.title, "Found: test-1");
        assert_eq!(task.source, "working");
    }

    #[test]
    fn test_effective_sources_uses_config_when_set() {
        let dir = tempfile::tempdir().unwrap();
        let tasks_dir = dir.path().join("tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();
        std::fs::write(tasks_dir.join("alpha.rhai"), "fn fetch(id) { #{} }").unwrap();
        std::fs::write(tasks_dir.join("beta.rhai"), "fn fetch(id) { #{} }").unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();

        // With explicit config sources, uses those in order
        let configured = vec!["beta".to_string()];
        assert_eq!(mgr.effective_sources(&configured), vec!["beta"]);
    }

    #[test]
    fn test_effective_sources_autodetects_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let tasks_dir = dir.path().join("tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();
        std::fs::write(tasks_dir.join("alpha.rhai"), "fn fetch(id) { #{} }").unwrap();
        std::fs::write(tasks_dir.join("beta.rhai"), "fn fetch(id) { #{} }").unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();

        // With empty config sources, discovers all installed resolvers
        let empty: Vec<String> = vec![];
        let mut sources = mgr.effective_sources(&empty);
        sources.sort();
        assert_eq!(sources, vec!["alpha", "beta"]);
    }

    #[test]
    fn test_resolve_info_multi_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let tasks_dir = dir.path().join("tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();

        // First resolver has no info function
        std::fs::write(
            tasks_dir.join("no_info.rhai"),
            r#"fn fetch(id) { #{} }"#,
        ).unwrap();

        // Second resolver has info
        std::fs::write(
            tasks_dir.join("has_info.rhai"),
            r#"fn info(id) { #{ id: id, title: "T", status: "open", assignee: "bob" } }"#,
        ).unwrap();

        let config = PluginsConfig {
            dir: dir.path().display().to_string(),
            disable: Vec::new(),
        };
        let mgr = PluginManager::new(&config).unwrap();

        let sources = vec!["no_info".to_string(), "has_info".to_string()];
        let ctx = PluginContext::default();
        let info = mgr.resolve_info_multi(&sources, "x", ctx).unwrap();
        assert_eq!(info.status.as_deref(), Some("open"));
        assert_eq!(info.assignee.as_deref(), Some("bob"));
    }
}
