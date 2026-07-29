//! Rhai resolver plugins for breq.
//!
//! Resolvers adapt external systems to breq. They come in three families, one per external
//! system breq needs to read or write:
//!
//! - `tasks/` — issue trackers: `info`, `claim`, `set_field`, `create`
//! - `agents/` — coding agents: `argv`, `resume_argv`, `activity`, `title`, `session_id`
//! - `delivery/` — forges: `prs`
//!
//! The contract is structured and in-process, which is what lets `breq list` join across every
//! workspace without a subprocess per row. Workflow *verbs* are not plugins — those are plain
//! `breq-<name>` scripts on PATH, called by the user rather than by breq.
//!
//! User plugins live in `~/.toren/plugins/<family>/<name>.rhai`. Breq vendors a few agent and
//! delivery resolvers, which a user file of the same name overrides — so the built-ins double
//! as reference implementations you can copy and edit.
//!
//! Compilation is lazy: plugins are scanned for metadata on init, compiled on first use.

pub mod builtin;
pub mod runtime;

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::{debug, warn};

use crate::tasks::ResolvedTask;

/// Which external system a resolver adapts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    Tasks,
    Agents,
    Delivery,
}

impl Family {
    /// Subdirectory under `~/.toren/plugins/`.
    pub fn dir(self) -> &'static str {
        match self {
            Family::Tasks => "tasks",
            Family::Agents => "agents",
            Family::Delivery => "delivery",
        }
    }

    pub fn all() -> &'static [Family] {
        &[Family::Tasks, Family::Agents, Family::Delivery]
    }

    pub fn parse(s: &str) -> Option<Family> {
        Family::all().iter().copied().find(|f| f.dir() == s)
    }
}

/// Where a plugin's source text comes from.
#[derive(Debug, Clone)]
pub enum PluginSource {
    /// A file under `~/.toren/plugins/`.
    File(PathBuf),
    /// Vendored into the binary. Overridable by a user file of the same name.
    Builtin(&'static str),
}

/// Lightweight metadata extracted from a plugin without compiling it.
#[derive(Debug, Clone)]
pub struct PluginMeta {
    pub name: String,
    pub family: Family,
    pub source: PluginSource,
    /// First paragraph of `///` doc comments.
    pub description: Option<String>,
    /// Full collected `///` doc-comment text.
    pub usage: Option<String>,
}

impl PluginMeta {
    fn text(&self) -> Result<String> {
        match &self.source {
            PluginSource::File(path) => std::fs::read_to_string(path)
                .with_context(|| format!("Failed to read plugin {}", path.display())),
            PluginSource::Builtin(text) => Ok(text.to_string()),
        }
    }

    fn cache_key(&self) -> String {
        format!("{}/{}", self.family.dir(), self.name)
    }

    /// Whether this is a vendored default rather than something the user installed.
    pub fn is_builtin(&self) -> bool {
        matches!(self.source, PluginSource::Builtin(_))
    }
}

/// Parse leading `///` doc comments from a Rhai source string.
///
/// Returns `(description, usage)` where `description` is the first paragraph and `usage` is
/// the full collected text.
fn parse_doc_comments(source: &str) -> (Option<String>, Option<String>) {
    let mut lines: Vec<String> = Vec::new();
    for raw in source.lines() {
        let trimmed = raw.trim_start();
        if let Some(rest) = trimmed.strip_prefix("///") {
            let content = rest.strip_prefix(' ').unwrap_or(rest);
            lines.push(content.to_string());
        } else {
            break;
        }
    }

    if lines.is_empty() {
        return (None, None);
    }

    let usage = lines.join("\n").trim().to_string();
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
    (desc, usg)
}

/// Context handed to resolver scripts.
pub struct PluginContext {
    pub segment_path: Option<PathBuf>,
    pub segment_name: Option<String>,
    /// Ordered list of task sources for multi-source resolution.
    pub task_sources: Vec<String>,
}

impl PluginContext {
    pub fn new(segment_path: Option<PathBuf>, segment_name: Option<String>) -> Self {
        Self {
            segment_path,
            segment_name,
            task_sources: Vec::new(),
        }
    }
}

impl Default for PluginContext {
    fn default() -> Self {
        Self::new(None, None)
    }
}

/// Discovers, compiles, and calls resolver plugins.
pub struct PluginManager {
    metas: HashMap<Family, HashMap<String, PluginMeta>>,
    /// Lazily compiled ASTs, keyed by `<family>/<name>`.
    compiled: Mutex<HashMap<String, rhai::AST>>,
}

impl PluginManager {
    /// Scan `dir` for plugins, layering user files over the vendored defaults.
    pub fn new(dir: &Path) -> Result<Self> {
        let mut metas: HashMap<Family, HashMap<String, PluginMeta>> = HashMap::new();

        for family in Family::all() {
            let mut family_metas: HashMap<String, PluginMeta> = HashMap::new();

            for (name, text) in builtin::for_family(*family) {
                let (description, usage) = parse_doc_comments(text);
                family_metas.insert(
                    name.to_string(),
                    PluginMeta {
                        name: name.to_string(),
                        family: *family,
                        source: PluginSource::Builtin(text),
                        description,
                        usage,
                    },
                );
            }

            let family_dir = dir.join(family.dir());
            if family_dir.exists() {
                scan_dir(&family_dir, *family, &mut family_metas)?;
            }

            metas.insert(*family, family_metas);
        }

        Ok(Self {
            metas,
            compiled: Mutex::new(HashMap::new()),
        })
    }

    /// Plugin names in a family, sorted.
    pub fn list(&self, family: Family) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .metas
            .get(&family)
            .map(|m| m.keys().map(|s| s.as_str()).collect())
            .unwrap_or_default();
        names.sort();
        names
    }

    pub fn get_meta(&self, family: Family, name: &str) -> Option<&PluginMeta> {
        self.metas.get(&family)?.get(name)
    }

    pub fn has(&self, family: Family, name: &str) -> bool {
        self.get_meta(family, name).is_some()
    }

    /// Whether a plugin defines a given function — how optional parts of a contract are
    /// probed, so a minimal resolver stays valid.
    pub fn has_fn(&self, family: Family, name: &str, fn_name: &str) -> bool {
        self.get_meta(family, name)
            .and_then(|m| self.compile(m).ok())
            .map(|ast| ast.iter_functions().any(|f| f.name == fn_name))
            .unwrap_or(false)
    }

    // ── Task resolvers ───────────────────────────────────────────────

    pub fn has_resolver(&self, source: &str) -> bool {
        self.has(Family::Tasks, source)
    }

    pub fn list_resolvers(&self) -> Vec<&str> {
        self.list(Family::Tasks)
    }

    /// Resolve task info via a task resolver's `info(id)`.
    pub fn resolve_info(&self, source: &str, id: &str, ctx: PluginContext) -> Result<ResolvedTask> {
        let map = self.call_map(Family::Tasks, source, "info", (id.to_string(),), ctx)?;

        Ok(ResolvedTask {
            id: get_map_string(&map, "id").unwrap_or_else(|| id.to_string()),
            source: source.to_string(),
            kind: get_map_string(&map, "kind"),
            title: get_map_string(&map, "title").unwrap_or_default(),
            status: get_map_string(&map, "status"),
            assignee: get_map_string(&map, "assignee"),
            description: get_map_string(&map, "description"),
            url: get_map_string(&map, "url"),
            created_at: get_map_string(&map, "created_at"),
            updated_at: get_map_string(&map, "updated_at"),
        })
    }

    /// Claim a task — the one tracker side effect anywhere in the place verbs.
    pub fn resolve_claim(
        &self,
        source: &str,
        id: &str,
        assignee: &str,
        ctx: PluginContext,
    ) -> Result<()> {
        let _ = self.call(
            Family::Tasks,
            source,
            "claim",
            (id.to_string(), assignee.to_string()),
            ctx,
        )?;
        Ok(())
    }

    /// Pass-through write of a task-source-owned field (`status`, `assignee`, `title`, …).
    ///
    /// Breq defines no status vocabulary: whatever the tracker accepts is what works, and
    /// whatever it reports is what `breq get` shows.
    pub fn resolve_set_field(
        &self,
        source: &str,
        id: &str,
        field: &str,
        value: &str,
        ctx: PluginContext,
    ) -> Result<()> {
        if !self.has_fn(Family::Tasks, source, "set_field") {
            anyhow::bail!(
                "Task source '{}' has no set_field(id, field, value) — add one to \
                 ~/.toren/plugins/tasks/{}.rhai to write task fields through breq",
                source,
                source
            );
        }
        let _ = self.call(
            Family::Tasks,
            source,
            "set_field",
            (id.to_string(), field.to_string(), value.to_string()),
            ctx,
        )?;
        Ok(())
    }

    /// Create a task. Returns the created task ID.
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
        let result = self.call(
            Family::Tasks,
            source,
            "create",
            (title.to_string(), desc_arg),
            ctx,
        )?;
        Ok(result.into_string().unwrap_or_default())
    }

    /// Effective task sources: config order if set, else every installed task resolver.
    pub fn effective_sources(&self, config_sources: &[String]) -> Vec<String> {
        if config_sources.is_empty() {
            self.list_resolvers()
                .iter()
                .map(|s| s.to_string())
                .collect()
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
        let available: Vec<_> = sources.iter().filter(|s| self.has_resolver(s)).collect();
        if available.is_empty() {
            anyhow::bail!(
                "No task resolvers available (tried: {:?}). Install one with \
                 `breq plugin install tasks/<name>`.",
                sources
            );
        }
        let mut last_err = None;
        for source in &available {
            let ctx = PluginContext::new(ctx.segment_path.clone(), ctx.segment_name.clone());
            match self.resolve_info(source, id, ctx) {
                Ok(info) => return Ok(info),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap())
    }

    // ── Agent resolvers ──────────────────────────────────────────────

    pub fn list_agents(&self) -> Vec<&str> {
        self.list(Family::Agents)
    }

    pub fn has_agent(&self, name: &str) -> bool {
        self.has(Family::Agents, name)
    }

    /// argv for a fresh agent run (program first).
    pub fn agent_argv(&self, name: &str, ctx_map: rhai::Map) -> Result<Vec<String>> {
        self.agent_argv_fn(name, "argv", ctx_map)
    }

    /// argv for resuming the workspace's previous session.
    pub fn agent_resume_argv(&self, name: &str, ctx_map: rhai::Map) -> Result<Vec<String>> {
        let f = if self.has_fn(Family::Agents, name, "resume_argv") {
            "resume_argv"
        } else {
            "argv"
        };
        self.agent_argv_fn(name, f, ctx_map)
    }

    fn agent_argv_fn(&self, name: &str, func: &str, ctx_map: rhai::Map) -> Result<Vec<String>> {
        let result = self.call(
            Family::Agents,
            name,
            func,
            (ctx_map,),
            PluginContext::default(),
        )?;
        let array = result
            .try_cast::<rhai::Array>()
            .with_context(|| format!("Agent '{}' {} did not return an array", name, func))?;

        let argv: Vec<String> = array
            .into_iter()
            .filter_map(|v| v.into_string().ok())
            .collect();
        if argv.is_empty() {
            anyhow::bail!("Agent '{}' {} returned an empty argv", name, func);
        }
        Ok(argv)
    }

    /// `running` / `idle` / `""` (unknown) for the agent's own view of a workspace.
    ///
    /// Distinct from pane liveness: an agent can hold a live pane while sitting idle at its
    /// prompt, and only the agent's own logs can tell the difference.
    pub fn agent_activity(&self, name: &str, ws_path: &Path) -> Option<String> {
        self.agent_string(name, "activity", ws_path)
    }

    /// The agent's summary of the session, for the list title fallback chain.
    pub fn agent_title(&self, name: &str, ws_path: &Path) -> Option<String> {
        self.agent_string(name, "title", ws_path)
    }

    /// The agent's session id in a workspace, for resume.
    pub fn agent_session_id(&self, name: &str, ws_path: &Path) -> Option<String> {
        self.agent_string(name, "session_id", ws_path)
    }

    fn agent_string(&self, name: &str, func: &str, ws_path: &Path) -> Option<String> {
        if !self.has_fn(Family::Agents, name, func) {
            return None;
        }
        let result = self
            .call(
                Family::Agents,
                name,
                func,
                (ws_path.display().to_string(),),
                PluginContext::default(),
            )
            .map_err(|e| {
                warn!(
                    event = "plugin.failed",
                    family = "agents",
                    plugin = name,
                    call = func,
                    "agent '{}' {} failed: {:#}",
                    name,
                    func,
                    e
                );
                e
            })
            .ok()?;
        result
            .into_string()
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    // ── Delivery resolvers ───────────────────────────────────────────

    pub fn list_delivery(&self) -> Vec<&str> {
        self.list(Family::Delivery)
    }

    /// Pull requests for a workspace's remote branches, as JSON values.
    ///
    /// Network-bound: callers cache the result and render from the cache.
    pub fn delivery_prs(
        &self,
        name: &str,
        ws_path: &Path,
        branches: &[String],
    ) -> Result<Vec<serde_json::Value>> {
        let mut ctx_map = rhai::Map::new();
        ctx_map.insert(
            "path".into(),
            rhai::Dynamic::from(ws_path.display().to_string()),
        );
        ctx_map.insert(
            "branches".into(),
            rhai::Dynamic::from(
                branches
                    .iter()
                    .map(|b| rhai::Dynamic::from(b.clone()))
                    .collect::<rhai::Array>(),
            ),
        );

        let result = self.call(
            Family::Delivery,
            name,
            "prs",
            (ctx_map,),
            PluginContext::default(),
        )?;

        let array = result
            .try_cast::<rhai::Array>()
            .with_context(|| format!("Delivery resolver '{}' prs did not return an array", name))?;

        Ok(array
            .into_iter()
            .filter_map(|v| rhai::serde::from_dynamic::<serde_json::Value>(&v).ok())
            .collect())
    }

    // ── Calling ──────────────────────────────────────────────────────

    /// Call a plugin function, returning the raw Dynamic result.
    pub fn call<A: rhai::FuncArgs>(
        &self,
        family: Family,
        name: &str,
        fn_name: &str,
        args: A,
        ctx: PluginContext,
    ) -> Result<rhai::Dynamic> {
        let meta = self
            .get_meta(family, name)
            .with_context(|| format!("No {} plugin found named '{}'", family.dir(), name))?;

        let ast = self.compile(meta)?;
        let engine = runtime::create_engine(Arc::new(ctx));
        let mut scope = rhai::Scope::new();

        engine
            .call_fn::<rhai::Dynamic>(&mut scope, &ast, fn_name, args)
            .map_err(|e| anyhow::anyhow!("{} '{}' {} error: {}", family.dir(), name, fn_name, e))
    }

    fn call_map<A: rhai::FuncArgs>(
        &self,
        family: Family,
        name: &str,
        fn_name: &str,
        args: A,
        ctx: PluginContext,
    ) -> Result<rhai::Map> {
        let result = self.call(family, name, fn_name, args, ctx)?;
        result.try_cast::<rhai::Map>().ok_or_else(|| {
            anyhow::anyhow!(
                "{} '{}' {} did not return a map",
                family.dir(),
                name,
                fn_name
            )
        })
    }

    /// Compile a plugin on demand, caching the result.
    fn compile(&self, meta: &PluginMeta) -> Result<rhai::AST> {
        let key = meta.cache_key();
        {
            let cache = self.compiled.lock().unwrap();
            if let Some(ast) = cache.get(&key) {
                return Ok(ast.clone());
            }
        }

        let source = meta.text()?;
        let engine = runtime::compiler();
        let ast = engine
            .compile(&source)
            .map_err(|e| anyhow::anyhow!("Failed to compile plugin '{}': {}", key, e))?;

        self.compiled.lock().unwrap().insert(key, ast.clone());
        Ok(ast)
    }
}

/// Scan a directory for `.rhai` plugins, extracting metadata only.
fn scan_dir(dir: &Path, family: Family, out: &mut HashMap<String, PluginMeta>) -> Result<()> {
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
                let (description, usage) = parse_doc_comments(&source);
                debug!(
                    "Scanned {} plugin '{}' from {}",
                    family.dir(),
                    name,
                    path.display()
                );
                out.insert(
                    name.clone(),
                    PluginMeta {
                        name,
                        family,
                        source: PluginSource::File(path),
                        description,
                        usage,
                    },
                );
            }
            Err(e) => warn!("Failed to read plugin {}: {}", path.display(), e),
        }
    }

    Ok(())
}

/// Extract a string value from a Rhai Map, treating unit as absent.
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

    fn manager_with(
        family: Family,
        name: &str,
        source: &str,
    ) -> (tempfile::TempDir, PluginManager) {
        let dir = tempfile::tempdir().unwrap();
        let family_dir = dir.path().join(family.dir());
        std::fs::create_dir_all(&family_dir).unwrap();
        std::fs::write(family_dir.join(format!("{}.rhai", name)), source).unwrap();
        let mgr = PluginManager::new(dir.path()).unwrap();
        (dir, mgr)
    }

    #[test]
    fn agents_ship_with_the_binary() {
        let mgr = PluginManager::new(Path::new("/nonexistent")).unwrap();
        assert!(mgr.has_agent("claude"), "claude must work out of the box");
        assert!(mgr.has_agent("codex"));
        assert!(mgr.has_agent("pi"));
        assert!(mgr.get_meta(Family::Agents, "claude").unwrap().is_builtin());
    }

    #[test]
    fn a_user_file_overrides_the_vendored_agent() {
        let (_dir, mgr) = manager_with(Family::Agents, "claude", r#"fn argv(ctx) { ["mine"] }"#);
        assert!(!mgr.get_meta(Family::Agents, "claude").unwrap().is_builtin());
        assert_eq!(
            mgr.agent_argv("claude", rhai::Map::new()).unwrap(),
            vec!["mine"]
        );
    }

    #[test]
    fn builtin_agent_argv_carries_prompt_and_model() {
        let mgr = PluginManager::new(Path::new("/nonexistent")).unwrap();
        let mut ctx = rhai::Map::new();
        ctx.insert(
            "prompt".into(),
            rhai::Dynamic::from("fix the bug".to_string()),
        );
        ctx.insert("model".into(), rhai::Dynamic::from("opus".to_string()));

        assert_eq!(
            mgr.agent_argv("claude", ctx).unwrap(),
            vec!["claude", "--model", "opus", "fix the bug"]
        );
    }

    #[test]
    fn builtin_agent_argv_adds_auto_approve_only_when_asked() {
        let mgr = PluginManager::new(Path::new("/nonexistent")).unwrap();
        let mut ctx = rhai::Map::new();
        ctx.insert("prompt".into(), rhai::Dynamic::from("go".to_string()));
        ctx.insert("auto_approve".into(), rhai::Dynamic::from(true));

        assert_eq!(
            mgr.agent_argv("claude", ctx).unwrap(),
            vec!["claude", "--dangerously-skip-permissions", "go"]
        );
    }

    #[test]
    fn resume_falls_back_to_argv_when_unimplemented() {
        let (_dir, mgr) =
            manager_with(Family::Agents, "minimal", r#"fn argv(ctx) { ["minimal"] }"#);
        assert_eq!(
            mgr.agent_resume_argv("minimal", rhai::Map::new()).unwrap(),
            vec!["minimal"]
        );
    }

    #[test]
    fn agent_introspection_is_optional() {
        let (_dir, mgr) = manager_with(Family::Agents, "quiet", r#"fn argv(ctx) { ["quiet"] }"#);
        assert!(mgr.agent_activity("quiet", Path::new("/tmp")).is_none());
        assert!(mgr.agent_title("quiet", Path::new("/tmp")).is_none());
    }

    #[test]
    fn claude_activity_reads_the_last_log_entry() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();

        // Claude's log location is derived from the workspace path, so fake HOME.
        let home = dir.path().join("home");
        let mangled = ws.display().to_string().replace(['/', '.'], "-");
        let log_dir = home.join(".claude/projects").join(&mangled);
        std::fs::create_dir_all(&log_dir).unwrap();
        std::fs::write(
            log_dir.join("sess-1.jsonl"),
            "{\"type\":\"summary\",\"summary\":\"Fix the flaky test\"}\n\
             {\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\"}]}}\n",
        )
        .unwrap();

        let prev = std::env::var("HOME").ok();
        std::env::set_var("HOME", &home);

        let mgr = PluginManager::new(Path::new("/nonexistent")).unwrap();
        assert_eq!(
            mgr.agent_activity("claude", &ws).as_deref(),
            Some("running")
        );
        assert_eq!(
            mgr.agent_title("claude", &ws).as_deref(),
            Some("Fix the flaky test")
        );
        assert_eq!(
            mgr.agent_session_id("claude", &ws).as_deref(),
            Some("sess-1")
        );

        match prev {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn task_resolvers_load_from_disk() {
        let (_dir, mgr) = manager_with(
            Family::Tasks,
            "mock",
            r#"fn info(id) {
                #{ id: id, title: "Task " + id, status: "in_progress", assignee: "claude" }
            }"#,
        );

        let info = mgr
            .resolve_info("mock", "abc-123", PluginContext::default())
            .unwrap();
        assert_eq!(info.title, "Task abc-123");
        assert_eq!(info.status.as_deref(), Some("in_progress"));
        assert_eq!(info.source, "mock");
    }

    #[test]
    fn set_field_is_pass_through() {
        let dir = tempfile::tempdir().unwrap();
        let tasks = dir.path().join("tasks");
        std::fs::create_dir_all(&tasks).unwrap();
        let out = dir.path().join("out.txt");
        std::fs::write(
            tasks.join("mock.rhai"),
            format!(
                r#"fn set_field(id, field, value) {{
                    fs::write("{}", id + "/" + field + "/" + value);
                }}"#,
                out.display()
            ),
        )
        .unwrap();

        let mgr = PluginManager::new(dir.path()).unwrap();
        mgr.resolve_set_field(
            "mock",
            "t-1",
            "status",
            "in-review",
            PluginContext::default(),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            "t-1/status/in-review"
        );
    }

    #[test]
    fn set_field_without_support_says_where_to_add_it() {
        let (_dir, mgr) = manager_with(Family::Tasks, "mock", r#"fn info(id) { #{ id: id } }"#);
        let err = mgr
            .resolve_set_field("mock", "t-1", "status", "done", PluginContext::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("set_field"), "{}", err);
        assert!(err.contains("tasks/mock.rhai"), "{}", err);
    }

    #[test]
    fn multi_source_falls_through_to_a_resolver_that_knows_the_id() {
        let dir = tempfile::tempdir().unwrap();
        let tasks = dir.path().join("tasks");
        std::fs::create_dir_all(&tasks).unwrap();
        std::fs::write(tasks.join("failing.rhai"), r#"fn other(id) { #{} }"#).unwrap();
        std::fs::write(
            tasks.join("working.rhai"),
            r#"fn info(id) { #{ id: id, title: "Found: " + id } }"#,
        )
        .unwrap();

        let mgr = PluginManager::new(dir.path()).unwrap();
        let sources = vec!["failing".to_string(), "working".to_string()];
        let task = mgr
            .resolve_info_multi(&sources, "test-1", PluginContext::default())
            .unwrap();
        assert_eq!(task.source, "working");
    }

    #[test]
    fn effective_sources_prefers_config_then_discovers() {
        let dir = tempfile::tempdir().unwrap();
        let tasks = dir.path().join("tasks");
        std::fs::create_dir_all(&tasks).unwrap();
        std::fs::write(tasks.join("alpha.rhai"), "fn info(id) { #{} }").unwrap();
        std::fs::write(tasks.join("beta.rhai"), "fn info(id) { #{} }").unwrap();

        let mgr = PluginManager::new(dir.path()).unwrap();
        assert_eq!(mgr.effective_sources(&["beta".to_string()]), vec!["beta"]);
        let mut discovered = mgr.effective_sources(&[]);
        discovered.sort();
        assert_eq!(discovered, vec!["alpha", "beta"]);
    }

    #[test]
    fn families_are_separate_namespaces() {
        let dir = tempfile::tempdir().unwrap();
        for family in ["tasks", "agents", "delivery"] {
            let d = dir.path().join(family);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("same.rhai"), "fn argv(ctx) { [\"x\"] }").unwrap();
        }

        let mgr = PluginManager::new(dir.path()).unwrap();
        assert!(mgr.has(Family::Tasks, "same"));
        assert!(mgr.has(Family::Agents, "same"));
        assert!(mgr.has(Family::Delivery, "same"));
    }

    #[test]
    fn delivery_prs_come_back_as_json() {
        let (_dir, mgr) = manager_with(
            Family::Delivery,
            "mock",
            r##"fn prs(ctx) {
                [#{ branch: ctx.branches[0], id: "#7", url: "u", state: "open", ci: "passing" }]
            }"##,
        );

        let prs = mgr
            .delivery_prs("mock", Path::new("/tmp"), &["feature".to_string()])
            .unwrap();
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0]["id"], "#7");
        assert_eq!(prs[0]["branch"], "feature");
    }

    #[test]
    fn contrib_plugins_compile() {
        let contrib = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("contrib/plugins");

        let engine = runtime::compiler();
        for family in Family::all() {
            let dir = contrib.join(family.dir());
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("rhai") {
                    continue;
                }
                let source = std::fs::read_to_string(&path).unwrap();
                engine
                    .compile(&source)
                    .unwrap_or_else(|e| panic!("Failed to compile {}: {}", path.display(), e));
            }
        }
    }

    /// Compiling proves a plugin parses, not that it implements what breq calls.
    #[test]
    fn contrib_task_plugins_implement_the_contract() {
        let contrib = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("contrib/plugins");

        let mgr = PluginManager::new(&contrib).unwrap();
        for source in mgr.list(Family::Tasks) {
            for fn_name in ["info", "claim", "set_field", "create"] {
                assert!(
                    mgr.has_fn(Family::Tasks, source, fn_name),
                    "contrib/plugins/tasks/{}.rhai defines no {}()",
                    source,
                    fn_name
                );
            }
        }
    }

    #[test]
    fn nothing_is_compiled_until_it_is_called() {
        let (_dir, mgr) = manager_with(Family::Tasks, "lazy", "fn info(id) { #{} }");
        assert!(mgr.compiled.lock().unwrap().is_empty());
        let _ = mgr.resolve_info("lazy", "x", PluginContext::default());
        assert!(mgr.compiled.lock().unwrap().contains_key("tasks/lazy"));
    }
}
