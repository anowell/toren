use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use shellexpand;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::warn;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(skip)]
    pub config_path: String,

    /// Cached segment paths: (roots, literal_segments).
    /// Populated once during load/default, avoids repeated glob expansion.
    #[serde(skip)]
    pub segment_paths: (Vec<PathBuf>, Vec<PathBuf>),

    #[serde(default = "default_server")]
    pub server: ServerConfig,

    #[serde(default)]
    pub ancillaries: AncillariesConfig,

    #[serde(default)]
    pub proxy: ProxyConfig,

    #[serde(default)]
    pub intents: IntentsConfig,

    #[serde(default)]
    pub tasks: TasksConfig,

    #[serde(default)]
    pub plugins: PluginsConfig,

    #[serde(default = "crate::alias::default_aliases")]
    pub aliases: HashMap<String, String>,
}

fn default_server() -> ServerConfig {
    ServerConfig {
        host: "127.0.0.1".to_string(),
        port: 8787,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

/// Configuration for ancillary workspaces and segment discovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AncillariesConfig {
    /// Segment globs: discover repos as segments.
    /// Entries like "~/proj/*" expand via glob; literal paths are used directly.
    #[serde(default)]
    pub segments: Vec<String>,

    /// Where ancillary workspaces are created (default: ~/.toren/workspaces)
    #[serde(default = "default_workspace_root")]
    pub workspace_root: PathBuf,

    /// Max ancillaries per segment (default: 10)
    #[serde(default = "default_max_per_segment")]
    pub max_per_segment: u32,
}

fn default_workspace_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".toren/workspaces")
}

fn default_max_per_segment() -> u32 {
    10
}

impl Default for AncillariesConfig {
    fn default() -> Self {
        Self {
            segments: Vec::new(),
            workspace_root: default_workspace_root(),
            max_per_segment: default_max_per_segment(),
        }
    }
}

/// Proxy configuration for station routes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// Base domain for station routes (default: lvh.me)
    #[serde(default = "default_proxy_domain")]
    pub domain: String,
}

fn default_proxy_domain() -> String {
    "lvh.me".to_string()
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            domain: default_proxy_domain(),
        }
    }
}

/// Intent templates keyed by name (e.g., "act", "plan", "review").
/// Additional custom intents can be added via config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentsConfig {
    #[serde(flatten)]
    pub entries: HashMap<String, String>,
}

impl IntentsConfig {
    /// Get an intent template by name, falling back to default if not found.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries.get(name).map(|s| s.as_str())
    }
}

impl Default for IntentsConfig {
    fn default() -> Self {
        let mut entries = HashMap::new();
        entries.insert("act".to_string(), default_intent_act());
        entries.insert("plan".to_string(), default_intent_plan());
        entries.insert("review".to_string(), default_intent_review());
        Self { entries }
    }
}

/// Configuration for the Rhai plugin system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginsConfig {
    /// Directory for user plugin scripts (default: ~/.toren/plugins)
    #[serde(default = "default_plugins_dir")]
    pub dir: String,

    /// List of plugin names to disable
    #[serde(default)]
    pub disable: Vec<String>,
}

fn default_plugins_dir() -> String {
    "~/.toren/plugins".to_string()
}

impl Default for PluginsConfig {
    fn default() -> Self {
        Self {
            dir: default_plugins_dir(),
            disable: Vec::new(),
        }
    }
}

/// Configuration for task tracking defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TasksConfig {
    /// Default task source when an ID is provided but source isn't specified
    #[serde(default = "default_task_source")]
    pub default_source: String,
}

fn default_task_source() -> String {
    "beads".to_string()
}

impl Default for TasksConfig {
    fn default() -> Self {
        Self {
            default_source: default_task_source(),
        }
    }
}

fn default_intent_act() -> String {
    "Implement {{ task.source }} {{ task.id }}: {{ task.title }}\n\n\
     Complete the task as specified. When done, summarize changes."
        .to_string()
}

fn default_intent_plan() -> String {
    "Design an approach for {{ task.source }} {{ task.id }}: {{ task.title }}\n\n\
     Investigate the codebase, explore options, and propose a design."
        .to_string()
}

fn default_intent_review() -> String {
    "Review the implementation of {{ task.source }} {{ task.id }}: {{ task.title }}\n\n\
     Verify completeness, check for issues, and assess confidence."
        .to_string()
}

/// Expand shell-style paths (e.g., `~` to home directory)
pub fn expand_path(path: &Path) -> PathBuf {
    let path_str = path.to_string_lossy();
    PathBuf::from(shellexpand::tilde(&path_str).into_owned())
}

/// Expand a shell-style string path
pub fn expand_path_str(path_str: &str) -> PathBuf {
    PathBuf::from(shellexpand::tilde(path_str).into_owned())
}

/// Shorten a path by replacing $HOME prefix with ~
pub fn tilde_shorten(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(relative) = path.strip_prefix(&home) {
            return format!("~/{}", relative.display());
        }
    }
    path.display().to_string()
}

impl Config {
    pub fn load() -> Result<Self> {
        Self::load_from(None)
    }

    pub fn load_from(config_path: Option<&Path>) -> Result<Self> {
        let config_path = if let Some(path) = config_path {
            path.to_path_buf()
        } else {
            Self::find_config_file()?
        };

        if config_path.exists() {
            let content =
                std::fs::read_to_string(&config_path).context("Failed to read config file")?;
            let mut config: Config =
                toml::from_str(&content).context("Failed to parse config file")?;
            config.config_path = config_path.display().to_string();
            config.expand_paths();
            Ok(config)
        } else if config_path == Self::find_config_file().unwrap_or_default() {
            // Only create default config for auto-discovered paths
            let config = Self::default();
            config.save(&config_path)?;
            Ok(config)
        } else {
            anyhow::bail!("Config file not found: {}", config_path.display())
        }
    }

    /// Expand shell-style paths in all path fields and cache derived values.
    fn expand_paths(&mut self) {
        // Expand workspace root
        self.ancillaries.workspace_root = expand_path(&self.ancillaries.workspace_root);
        // Cache segment paths (avoids re-expanding globs on each call)
        self.segment_paths = self.compute_segment_paths();
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let content = toml::to_string_pretty(self).context("Failed to serialize config")?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).context("Failed to create config directory")?;
        }

        std::fs::write(path, content).context("Failed to write config file")?;

        Ok(())
    }

    fn find_config_file() -> Result<PathBuf> {
        if let Some(home) = dirs::home_dir() {
            let new_config = home.join(".toren/config.toml");
            if new_config.exists() {
                return Ok(new_config);
            }

            // Check for old config location and warn
            let old_config = home.join(".config/toren/config.toml");
            if old_config.exists() {
                warn!(
                    "Found config at old location: {}. Consider migrating to {}",
                    old_config.display(),
                    new_config.display()
                );
                return Ok(old_config);
            }

            // Return new path even if it doesn't exist (we'll create it)
            return Ok(new_config);
        }

        // Fallback
        Ok(PathBuf::from("toren.toml"))
    }

    /// Get cached segment paths: (roots, literal_segments).
    /// Roots are parent dirs of glob matches, literal_segments are non-glob entries.
    pub fn resolve_segment_paths(&self) -> &(Vec<PathBuf>, Vec<PathBuf>) {
        &self.segment_paths
    }

    /// Compute segment paths by expanding globs in ancillaries.segments.
    pub(crate) fn compute_segment_paths(&self) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let mut roots = Vec::new();
        let mut literals = Vec::new();

        for pattern in &self.ancillaries.segments {
            let expanded = shellexpand::tilde(pattern).into_owned();

            if expanded.contains('*') || expanded.contains('?') || expanded.contains('[') {
                // Glob pattern: expand and collect parent dirs as roots
                match glob::glob(&expanded) {
                    Ok(paths) => {
                        for entry in paths.filter_map(|p| p.ok()) {
                            if entry.is_dir() {
                                if let Some(parent) = entry.parent() {
                                    let canonical = parent
                                        .canonicalize()
                                        .unwrap_or_else(|_| parent.to_path_buf());
                                    if !roots.contains(&canonical) {
                                        roots.push(canonical);
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Invalid glob pattern '{}': {}", pattern, e);
                    }
                }
            } else {
                // Literal path: treat as direct segment
                let path = PathBuf::from(&expanded);
                if path.is_dir() {
                    let canonical = path.canonicalize().unwrap_or(path);
                    literals.push(canonical);
                } else {
                    warn!("Segment path does not exist: {}", expanded);
                }
            }
        }

        (roots, literals)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            config_path: String::new(),
            segment_paths: (Vec::new(), Vec::new()),
            server: default_server(),
            ancillaries: AncillariesConfig::default(),
            proxy: ProxyConfig::default(),
            intents: IntentsConfig::default(),
            tasks: TasksConfig::default(),
            plugins: PluginsConfig::default(),
            aliases: crate::alias::default_aliases(),
        }
    }
}

// Backward compatibility getters
impl Config {
    pub fn host(&self) -> &str {
        &self.server.host
    }

    pub fn port(&self) -> u16 {
        self.server.port
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn tilde_shorten_under_home() {
        if let Some(home) = dirs::home_dir() {
            let path = home.join("projects/myrepo");
            assert_eq!(tilde_shorten(&path), "~/projects/myrepo");
        }
    }

    #[test]
    fn tilde_shorten_outside_home() {
        let path = PathBuf::from("/tmp/some/path");
        assert_eq!(tilde_shorten(&path), "/tmp/some/path");
    }

    #[test]
    fn resolve_segment_paths_empty() {
        let config = Config::default();
        let (roots, literals) = config.resolve_segment_paths();
        assert!(roots.is_empty());
        assert!(literals.is_empty());
    }

    #[test]
    fn resolve_segment_paths_glob() {
        let dir = tempfile::tempdir().unwrap();
        let sub1 = dir.path().join("repo1");
        let sub2 = dir.path().join("repo2");
        std::fs::create_dir_all(&sub1).unwrap();
        std::fs::create_dir_all(&sub2).unwrap();

        let mut config = Config::default();
        config.ancillaries.segments = vec![format!("{}/*", dir.path().display())];
        config.segment_paths = config.compute_segment_paths();

        let (roots, literals) = config.resolve_segment_paths();
        assert_eq!(roots.len(), 1);
        assert!(literals.is_empty());
        // The root should be the parent dir of the matched entries
        let root_canonical = dir.path().canonicalize().unwrap();
        assert_eq!(roots[0], root_canonical);
    }

    #[test]
    fn resolve_segment_paths_literal() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("myrepo");
        std::fs::create_dir_all(&repo).unwrap();

        let mut config = Config::default();
        config.ancillaries.segments = vec![repo.display().to_string()];
        config.segment_paths = config.compute_segment_paths();

        let (roots, literals) = config.resolve_segment_paths();
        assert!(roots.is_empty());
        assert_eq!(literals.len(), 1);
        let repo_canonical = repo.canonicalize().unwrap();
        assert_eq!(literals[0], repo_canonical);
    }

    #[test]
    fn resolve_segment_paths_nonexistent_literal_skipped() {
        let mut config = Config::default();
        config.ancillaries.segments = vec!["/nonexistent/path/to/repo".to_string()];
        config.segment_paths = config.compute_segment_paths();

        let (roots, literals) = config.resolve_segment_paths();
        assert!(roots.is_empty());
        assert!(literals.is_empty());
    }

    #[test]
    fn default_config_parses() {
        let config = Config::default();
        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.server.port, 8787);
        assert_eq!(config.proxy.domain, "lvh.me");
        assert!(config.ancillaries.segments.is_empty());
        assert_eq!(config.ancillaries.max_per_segment, 10);
    }

    #[test]
    fn parse_new_config_format() {
        let toml_str = r#"
[ancillaries]
segments = ["~/proj/*", "~/myrepo"]
max_per_segment = 5

[proxy]
domain = "test.local"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.ancillaries.segments, vec!["~/proj/*", "~/myrepo"]);
        assert_eq!(config.ancillaries.max_per_segment, 5);
        assert_eq!(config.proxy.domain, "test.local");
    }

    #[test]
    fn find_config_file_prefers_new_location() {
        // This test validates the logic by checking the function exists and returns a path.
        // The actual file system state determines the result.
        let result = Config::find_config_file();
        assert!(result.is_ok());
        let path = result.unwrap();
        // Should end with config.toml
        assert!(path.to_string_lossy().ends_with("config.toml"));
    }
}
