//! Rhai plugin system for breq.
//!
//! Plugins are Rhai scripts that call native Rust operations via a host API.
//! User plugins are discovered from `~/.toren/plugins/*.rhai`.
//! Community/example plugins live in `contrib/plugins/` and can be copied
//! into the user plugin directory.

pub mod builtin;
pub mod runtime;

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, warn};

use crate::config::PluginsConfig;

/// A loaded plugin (compiled AST with optional doc-comment metadata).
pub struct Plugin {
    pub name: String,
    pub path: PathBuf,
    pub ast: rhai::AST,
    /// First paragraph of `///` doc comments (short description).
    pub description: Option<String>,
    /// Full collected `///` doc-comment text (shown by `--help`).
    pub usage: Option<String>,
}

/// Parse leading `///` doc comments from a Rhai source string.
///
/// Returns `(description, usage)` where:
/// - `description` is the first paragraph (lines before a blank `///` line)
/// - `usage` is the full collected text
fn parse_doc_comments(source: &str) -> (Option<String>, Option<String>) {
    let mut lines: Vec<String> = Vec::new();
    for raw in source.lines() {
        let trimmed = raw.trim_start();
        if let Some(rest) = trimmed.strip_prefix("///") {
            // Strip one optional leading space after ///
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

    (desc, usg)
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
/// return a map like `#{ action: "cmd", task_id: "x" }`. The host interprets
/// these after the script completes.
#[derive(Debug, Clone)]
pub enum DeferredAction {
    /// Start a Claude session via `breq cmd`.
    Cmd {
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
}

/// Manages plugin discovery, loading, and execution.
pub struct PluginManager {
    plugins: HashMap<String, Plugin>,
}

impl PluginManager {
    /// Create a new PluginManager: discover user plugins from the configured directory.
    pub fn new(config: &PluginsConfig) -> Result<Self> {
        let mut mgr = Self {
            plugins: HashMap::new(),
        };

        // Discover user plugins
        let dir = crate::config::expand_path_str(&config.dir);
        if dir.exists() {
            mgr.load_from_dir(&dir)?;
        }

        // Remove disabled plugins
        for name in &config.disable {
            if mgr.plugins.remove(name).is_some() {
                info!("Plugin '{}' disabled by config", name);
            }
        }

        Ok(mgr)
    }

    /// Check if a plugin exists by name.
    pub fn has(&self, name: &str) -> bool {
        self.plugins.contains_key(name)
    }

    /// List all loaded plugin names.
    pub fn list(&self) -> Vec<&str> {
        self.plugins.keys().map(|s| s.as_str()).collect()
    }

    /// List plugin names with their short descriptions, sorted by name.
    pub fn list_with_descriptions(&self) -> Vec<(&str, Option<&str>)> {
        let mut items: Vec<_> = self
            .plugins
            .iter()
            .map(|(name, p)| (name.as_str(), p.description.as_deref()))
            .collect();
        items.sort_by_key(|(name, _)| *name);
        items
    }

    /// Get the full usage text for a plugin (from `///` doc comments).
    pub fn usage(&self, name: &str) -> Option<&str> {
        self.plugins.get(name).and_then(|p| p.usage.as_deref())
    }

    /// Run a plugin by name with the given arguments and context.
    pub fn run(&self, name: &str, args: &[String], ctx: PluginContext) -> Result<PluginResult> {
        let plugin = self
            .plugins
            .get(name)
            .with_context(|| format!("Plugin '{}' not found", name))?;

        let ctx = Arc::new(ctx);
        let engine = runtime::create_engine(ctx);

        runtime::run_ast(&engine, &plugin.ast, args)
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
                        let (description, usage) = parse_doc_comments(&source);
                        info!("Loaded plugin '{}' from {}", name, path.display());
                        self.plugins.insert(
                            name.clone(),
                            Plugin {
                                name: name.clone(),
                                path: path.clone(),
                                ast,
                                description,
                                usage,
                            },
                        );
                    }
                    Err(e) => {
                        warn!("Failed to compile plugin '{}' ({}): {}", name, path.display(), e);
                    }
                },
                Err(e) => {
                    warn!("Failed to read plugin '{}' ({}): {}", name, path.display(), e);
                }
            }
        }

        Ok(())
    }
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
        assert!(mgr.plugins.is_empty());
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
        assert!(mgr.plugins.is_empty());
    }

    #[test]
    fn test_contrib_plugins_compile() {
        // Verify the contrib plugin scripts are valid Rhai
        let engine = rhai::Engine::new();
        let contrib_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("contrib/plugins");

        for name in &["assign", "complete", "abort"] {
            let path = contrib_dir.join(format!("{}.rhai", name));
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|_| panic!("Failed to read {}", path.display()));
            engine.compile(&source)
                .unwrap_or_else(|e| panic!("Failed to compile {}: {}", name, e));
        }
    }

    #[test]
    fn test_doc_comments_basic() {
        let source = "/// Short description.\n///\n/// Detailed usage text\n/// spanning lines.\nlet x = 1;";
        let (desc, usage) = parse_doc_comments(source);
        assert_eq!(desc.as_deref(), Some("Short description."));
        assert_eq!(
            usage.as_deref(),
            Some("Short description.\n\nDetailed usage text\nspanning lines.")
        );
    }

    #[test]
    fn test_doc_comments_missing() {
        let source = "// regular comment\nlet x = 1;";
        let (desc, usage) = parse_doc_comments(source);
        assert!(desc.is_none());
        assert!(usage.is_none());
    }

    #[test]
    fn test_doc_comments_single_paragraph() {
        let source = "/// Just a one-liner.\nlet x = 1;";
        let (desc, usage) = parse_doc_comments(source);
        assert_eq!(desc.as_deref(), Some("Just a one-liner."));
        assert_eq!(usage.as_deref(), Some("Just a one-liner."));
    }

    #[test]
    fn test_doc_comments_no_space_after_slashes() {
        let source = "///No space here.\nlet x = 1;";
        let (desc, _usage) = parse_doc_comments(source);
        assert_eq!(desc.as_deref(), Some("No space here."));
    }

    #[test]
    fn test_list_with_descriptions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("beta.rhai"), "/// Beta plugin.\nlet x = 1;").unwrap();
        std::fs::write(dir.path().join("alpha.rhai"), "/// Alpha plugin.\nlet x = 1;").unwrap();
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
            .compile(r#"#{ action: "cmd", task_id: "test-123", task_title: "Test task" }"#)
            .unwrap();

        let result = runtime::run_ast(&engine, &ast, &[]).unwrap();
        match result {
            PluginResult::Action(DeferredAction::Cmd {
                task_id,
                task_title,
                ..
            }) => {
                assert_eq!(task_id.as_deref(), Some("test-123"));
                assert_eq!(task_title.as_deref(), Some("Test task"));
            }
            _ => panic!("Expected DeferredAction::Cmd"),
        }
    }
}
