//! The global config, in the language the per-repo `toren.kdl` already speaks.
//!
//! `~/.toren/config.kdl` replaces `config.toml`. A file in the old format is converted the first
//! time it is loaded and then left alone: the copy on disk is the record of what the old file
//! said, including the settings toren has since stopped having.

use anyhow::{Context, Result};
use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};
use serde::{Deserialize, Serialize};
use shellexpand;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use crate::workspace_setup::{kdl_value_as_i64, kdl_value_as_str};

/// The global config file.
const CONFIG_FILE: &str = "config.kdl";

/// What `config.kdl` replaced. Read once, to convert; never written.
const LEGACY_CONFIG_FILE: &str = "config.toml";

/// Return the toren root directory (~/.toren).
pub fn toren_root() -> PathBuf {
    dirs::home_dir()
        .expect("Could not determine home directory")
        .join(".toren")
}

/// Where the global config lives, whether or not it exists yet.
pub fn default_config_path() -> PathBuf {
    toren_root().join(CONFIG_FILE)
}

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
    pub tasks: TasksConfig,

    #[serde(default)]
    pub delivery: DeliveryConfig,

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

    /// Coding agent to use (e.g., "claude", "codex:o3"). Auto-detects if unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
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
            agent: None,
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

/// Which delivery resolver reads PR/CI state.
///
/// Optional: with exactly one delivery plugin installed, breq uses it. A workspace can also
/// override per-place with the workspace's own `delivery`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeliveryConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Configuration for task tracking defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TasksConfig {
    /// Ordered list of task sources to try when resolving.
    /// Accepts old `default_source = "myresolver"` format for backwards compat.
    #[serde(
        default = "default_task_sources",
        deserialize_with = "deserialize_sources",
        alias = "default_source"
    )]
    pub sources: Vec<String>,
}

fn default_task_sources() -> Vec<String> {
    vec![] // empty = auto-detect from installed task plugins
}

/// Accept both `sources = ["mysource"]` (new) and `default_source = "mysource"` (old).
fn deserialize_sources<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum SourcesCompat {
        Multiple(Vec<String>),
        Single(String),
    }
    match SourcesCompat::deserialize(d)? {
        SourcesCompat::Multiple(v) => Ok(v),
        SourcesCompat::Single(s) => Ok(vec![s]),
    }
}

impl TasksConfig {
    /// Primary source (first in list), if configured.
    /// Returns `None` when sources is empty (auto-detect from installed plugins).
    pub fn default_source(&self) -> Option<&str> {
        self.sources.first().map(|s| s.as_str())
    }
}

impl Default for TasksConfig {
    fn default() -> Self {
        Self {
            sources: default_task_sources(),
        }
    }
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

/// A `config.toml` left over from before KDL, in either location it was ever kept.
fn legacy_config_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    [
        home.join(".toren").join(LEGACY_CONFIG_FILE),
        home.join(".config/toren").join(LEGACY_CONFIG_FILE),
    ]
    .into_iter()
    .find(|path| path.exists())
}

/// Every setting in a TOML config that the current `Config` does not carry, as `section.key`.
///
/// `Config` has no `deny_unknown_fields`, which is how a table removed from the code went on
/// living in the file unmentioned. Migration is the one moment those are still readable.
fn unknown_toml_settings(content: &str) -> Result<Vec<String>> {
    /// `None` for a section whose keys are the user's own.
    fn known_keys(section: &str) -> Option<Option<&'static [&'static str]>> {
        match section {
            "server" => Some(Some(&["host", "port"])),
            "ancillaries" => Some(Some(&[
                "segments",
                "workspace_root",
                "max_per_segment",
                "agent",
            ])),
            "proxy" => Some(Some(&["domain"])),
            "tasks" => Some(Some(&["sources", "default_source"])),
            "delivery" => Some(Some(&["source"])),
            "aliases" => Some(None),
            _ => None,
        }
    }

    let doc: toml::Table = content.parse().context("Failed to parse config file")?;
    let mut unknown = Vec::new();

    for (section, value) in &doc {
        let Some(keys) = known_keys(section) else {
            unknown.push(section.clone());
            continue;
        };
        let (Some(keys), Some(table)) = (keys, value.as_table()) else {
            continue;
        };
        for key in table.keys() {
            if !keys.contains(&key.as_str()) {
                unknown.push(format!("{}.{}", section, key));
            }
        }
    }

    Ok(unknown)
}

fn child_nodes(node: &KdlNode) -> &[KdlNode] {
    node.children().map(|doc| doc.nodes()).unwrap_or(&[])
}

/// The first positional argument of a `key "value"` node.
fn node_string(node: &KdlNode) -> Result<String> {
    node.entries()
        .iter()
        .find(|entry| entry.name().is_none())
        .and_then(|entry| kdl_value_as_str(entry.value()))
        .with_context(|| format!("{}: expected a value", node.name().value()))
}

/// Every positional argument of a `key "one" "two"` node.
fn node_strings(node: &KdlNode) -> Result<Vec<String>> {
    node.entries()
        .iter()
        .filter(|entry| entry.name().is_none())
        .map(|entry| {
            kdl_value_as_str(entry.value())
                .with_context(|| format!("{}: unsupported value type", node.name().value()))
        })
        .collect()
}

fn node_i64(node: &KdlNode) -> Result<i64> {
    node.entries()
        .iter()
        .find(|entry| entry.name().is_none())
        .and_then(|entry| kdl_value_as_i64(entry.value()))
        .with_context(|| format!("{}: expected a number", node.name().value()))
}

fn node_u16(node: &KdlNode) -> Result<u16> {
    u16::try_from(node_i64(node)?).with_context(|| format!("{}: out of range", node.name().value()))
}

fn node_u32(node: &KdlNode) -> Result<u32> {
    u32::try_from(node_i64(node)?).with_context(|| format!("{}: out of range", node.name().value()))
}

fn warn_unknown(section: &str, name: &str) {
    warn!("Unknown node in {} '{}': {}", CONFIG_FILE, section, name);
}

/// A `name "value" ...` node.
fn setting(name: &str, values: impl IntoIterator<Item = KdlValue>) -> KdlNode {
    let mut node = KdlNode::new(name);
    for value in values {
        node.push(KdlEntry::new(value));
    }
    node
}

/// A `name { ... }` node.
fn section(name: &str, children: Vec<KdlNode>) -> KdlNode {
    let mut node = KdlNode::new(name);
    let mut doc = KdlDocument::new();
    *doc.nodes_mut() = children;
    node.set_children(doc);
    node
}

impl Config {
    pub fn load() -> Result<Self> {
        Self::load_from(None)
    }

    pub fn load_from(config_path: Option<&Path>) -> Result<Self> {
        match config_path {
            Some(path) if path.exists() => Self::read(path),
            Some(path) => anyhow::bail!("Config file not found: {}", path.display()),
            None => Self::load_discovered(),
        }
    }

    /// Load `~/.toren/config.kdl`, converting a `config.toml` on the way past and writing a
    /// default file when there is nothing to load at all.
    fn load_discovered() -> Result<Self> {
        let path = default_config_path();
        if path.exists() {
            return Self::read(&path);
        }

        if let Some(legacy) = legacy_config_path() {
            let (config, unknown) = Self::migrate(&legacy, &path)?;
            info!(
                event = "config.migrated",
                from = %tilde_shorten(&legacy),
                to = %tilde_shorten(&path),
                "Converted {} to {}",
                tilde_shorten(&legacy),
                tilde_shorten(&path)
            );
            for key in &unknown {
                warn!(
                    "{} sets '{}', which toren has no setting for — it is not in {}, and the \
                     original file is left where it is",
                    tilde_shorten(&legacy),
                    key,
                    CONFIG_FILE
                );
            }
            return Ok(config);
        }

        let config = Self::default();
        config.save(&path)?;
        config.settled(&path)
    }

    /// Read one config file, in whichever language its extension says it is in.
    fn read(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;

        let config = if path.extension().is_some_and(|ext| ext == "toml") {
            warn!(
                "{} is the old TOML format; convert it to {}",
                path.display(),
                CONFIG_FILE
            );
            Self::parse_toml(&content)
        } else {
            Self::parse_kdl(&content)
        }
        .with_context(|| format!("Failed to parse {}", path.display()))?;

        config.settled(path)
    }

    /// Convert a legacy `config.toml` into `config.kdl`, returning every setting the current
    /// `Config` does not carry rather than swallowing it. The old file is left where it is —
    /// this is a copy, not a move, so what was dropped stays readable.
    pub fn migrate(legacy: &Path, target: &Path) -> Result<(Self, Vec<String>)> {
        let content = std::fs::read_to_string(legacy)
            .with_context(|| format!("Failed to read {}", legacy.display()))?;
        let config = Self::parse_toml(&content)
            .with_context(|| format!("Failed to parse {}", legacy.display()))?;
        let unknown = unknown_toml_settings(&content)
            .with_context(|| format!("Failed to parse {}", legacy.display()))?;
        config.save(target)?;
        Ok((config.settled(target)?, unknown))
    }

    fn parse_toml(content: &str) -> Result<Self> {
        toml::from_str(content).context("Failed to parse config file")
    }

    /// Hand-walked, like `toren.kdl` — the shape is small enough that a warning for anything
    /// unrecognised is worth more than a derive that would drop it silently.
    pub fn parse_kdl(content: &str) -> Result<Self> {
        let doc: KdlDocument = content.parse()?;
        let mut config = Self::default();

        for node in doc.nodes() {
            let section = node.name().value();
            match section {
                "server" => {
                    for child in child_nodes(node) {
                        match child.name().value() {
                            "host" => config.server.host = node_string(child)?,
                            "port" => config.server.port = node_u16(child)?,
                            other => warn_unknown(section, other),
                        }
                    }
                }
                "ancillaries" => {
                    for child in child_nodes(node) {
                        match child.name().value() {
                            "segments" => config.ancillaries.segments = node_strings(child)?,
                            "workspace_root" => {
                                config.ancillaries.workspace_root =
                                    PathBuf::from(node_string(child)?)
                            }
                            "max_per_segment" => {
                                config.ancillaries.max_per_segment = node_u32(child)?
                            }
                            "agent" => config.ancillaries.agent = Some(node_string(child)?),
                            other => warn_unknown(section, other),
                        }
                    }
                }
                "proxy" => {
                    for child in child_nodes(node) {
                        match child.name().value() {
                            "domain" => config.proxy.domain = node_string(child)?,
                            other => warn_unknown(section, other),
                        }
                    }
                }
                "tasks" => {
                    for child in child_nodes(node) {
                        match child.name().value() {
                            "sources" => config.tasks.sources = node_strings(child)?,
                            other => warn_unknown(section, other),
                        }
                    }
                }
                "delivery" => {
                    for child in child_nodes(node) {
                        match child.name().value() {
                            "source" => config.delivery.source = Some(node_string(child)?),
                            other => warn_unknown(section, other),
                        }
                    }
                }
                // Alias names are the user's, so every child node is a setting here.
                "aliases" => {
                    for child in child_nodes(node) {
                        config
                            .aliases
                            .insert(child.name().value().to_string(), node_string(child)?);
                    }
                }
                other => {
                    warn!("Unknown top-level node in {}: {}", CONFIG_FILE, other);
                }
            }
        }

        Ok(config)
    }

    /// Render the config as KDL, from the struct rather than from whatever was read.
    pub fn to_kdl(&self) -> String {
        let mut doc = KdlDocument::new();

        doc.nodes_mut().push(section(
            "server",
            vec![
                setting("host", [KdlValue::from(self.server.host.as_str())]),
                setting("port", [KdlValue::from(i128::from(self.server.port))]),
            ],
        ));

        let mut ancillaries = Vec::new();
        if !self.ancillaries.segments.is_empty() {
            ancillaries.push(setting(
                "segments",
                self.ancillaries.segments.iter().map(|s| s.as_str().into()),
            ));
        }
        ancillaries.push(setting(
            "workspace_root",
            [tilde_shorten(&self.ancillaries.workspace_root).into()],
        ));
        ancillaries.push(setting(
            "max_per_segment",
            [KdlValue::from(i128::from(self.ancillaries.max_per_segment))],
        ));
        if let Some(agent) = &self.ancillaries.agent {
            ancillaries.push(setting("agent", [agent.as_str().into()]));
        }
        doc.nodes_mut().push(section("ancillaries", ancillaries));

        doc.nodes_mut().push(section(
            "proxy",
            vec![setting("domain", [self.proxy.domain.as_str().into()])],
        ));

        if !self.tasks.sources.is_empty() {
            doc.nodes_mut().push(section(
                "tasks",
                vec![setting(
                    "sources",
                    self.tasks.sources.iter().map(|s| s.as_str().into()),
                )],
            ));
        }

        if let Some(source) = &self.delivery.source {
            doc.nodes_mut().push(section(
                "delivery",
                vec![setting("source", [source.as_str().into()])],
            ));
        }

        if !self.aliases.is_empty() {
            let mut names: Vec<&String> = self.aliases.keys().collect();
            names.sort();
            let aliases = names
                .into_iter()
                .map(|name| setting(name, [self.aliases[name].as_str().into()]))
                .collect();
            doc.nodes_mut().push(section("aliases", aliases));
        }

        doc.autoformat();
        doc.to_string()
    }

    /// Record where this config came from and derive everything that depends on its paths.
    fn settled(mut self, path: &Path) -> Result<Self> {
        self.config_path = path.display().to_string();
        self.expand_paths();
        Ok(self)
    }

    /// Expand shell-style paths in all path fields and cache derived values.
    fn expand_paths(&mut self) {
        // Expand workspace root
        self.ancillaries.workspace_root = expand_path(&self.ancillaries.workspace_root);
        // Cache segment paths (avoids re-expanding globs on each call)
        self.segment_paths = self.compute_segment_paths();
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).context("Failed to create config directory")?;
        }

        crate::fsutil::write_atomic(path, self.to_kdl())?;

        Ok(())
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
            tasks: TasksConfig::default(),
            delivery: DeliveryConfig::default(),
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
    fn parse_kdl_reads_every_section() {
        let config = Config::parse_kdl(
            r#"
server {
    host "0.0.0.0"
    port 9000
}

ancillaries {
    segments "~/proj/*" "~/myrepo"
    workspace_root "~/work"
    max_per_segment 5
    agent "claude"
}

proxy {
    domain "test.local"
}

tasks {
    sources "runes" "github"
}

delivery {
    source "github"
}

aliases {
    show "breq get $1"
}
"#,
        )
        .unwrap();

        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.server.port, 9000);
        assert_eq!(config.ancillaries.segments, vec!["~/proj/*", "~/myrepo"]);
        assert_eq!(config.ancillaries.workspace_root, PathBuf::from("~/work"));
        assert_eq!(config.ancillaries.max_per_segment, 5);
        assert_eq!(config.ancillaries.agent.as_deref(), Some("claude"));
        assert_eq!(config.proxy.domain, "test.local");
        assert_eq!(config.tasks.sources, vec!["runes", "github"]);
        assert_eq!(config.delivery.source.as_deref(), Some("github"));
        assert_eq!(config.aliases.get("show").unwrap(), "breq get $1");
    }

    /// The example is documentation that can go stale — the one it replaced documented three
    /// sections `Config` never had. Nothing unknown, and every value it shows must land.
    #[test]
    fn the_shipped_example_matches_the_struct() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../config.kdl.example");
        let content = std::fs::read_to_string(path).unwrap();

        let mut parsed = None;
        let logged = captured(|| parsed = Some(Config::parse_kdl(&content).unwrap()));
        assert!(!logged.contains("Unknown"), "{}", logged);

        let config = parsed.unwrap();
        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.server.port, 8787);
        assert_eq!(config.ancillaries.max_per_segment, 10);
        assert_eq!(config.proxy.domain, "lvh.me");
        assert_eq!(config.ancillaries.segments.len(), 2);
    }

    #[test]
    fn an_empty_kdl_config_is_the_default_one() {
        let config = Config::parse_kdl("").unwrap();
        assert_eq!(config.server.port, Config::default().server.port);
        assert_eq!(config.proxy.domain, Config::default().proxy.domain);
    }

    #[test]
    fn kdl_round_trips_through_the_struct() {
        let mut config = Config::default();
        config.ancillaries.segments = vec!["~/proj/*".into()];
        config.ancillaries.agent = Some("codex".into());
        config.tasks.sources = vec!["runes".into()];
        config.delivery.source = Some("github".into());
        config.aliases.insert("show".into(), "breq get $1".into());

        let parsed = Config::parse_kdl(&config.to_kdl()).unwrap();
        assert_eq!(parsed.ancillaries.segments, config.ancillaries.segments);
        assert_eq!(parsed.ancillaries.agent, config.ancillaries.agent);
        assert_eq!(parsed.tasks.sources, config.tasks.sources);
        assert_eq!(parsed.delivery.source, config.delivery.source);
        assert_eq!(parsed.aliases, config.aliases);
        assert_eq!(parsed.server.port, config.server.port);
    }

    #[test]
    fn a_rendered_config_keeps_paths_in_tilde_form() {
        let mut config = Config::default();
        config.ancillaries.workspace_root = default_workspace_root();
        assert!(
            config
                .to_kdl()
                .contains("workspace_root \"~/.toren/workspaces\""),
            "{}",
            config.to_kdl()
        );
    }

    #[test]
    fn unknown_nodes_are_warned_about_rather_than_dropped_silently() {
        let logged = captured(|| {
            let config = Config::parse_kdl(
                r#"
intents {
    plan "something we removed"
}

server {
    port 9001
    timeout 30
}
"#,
            )
            .unwrap();
            assert_eq!(config.server.port, 9001, "known settings still load");
        });

        assert!(
            logged.contains("Unknown top-level node in config.kdl: intents"),
            "{}",
            logged
        );
        assert!(
            logged.contains("Unknown node in config.kdl 'server': timeout"),
            "{}",
            logged
        );
    }

    #[test]
    fn migration_writes_kdl_and_leaves_the_old_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("config.toml");
        let target = dir.path().join("config.kdl");
        std::fs::write(
            &legacy,
            r#"
[server]
host = "127.0.0.1"
port = 8788

[ancillaries]
segments = ["~/proj/*"]
max_per_segment = 4

[aliases]
show = "breq get $1"
"#,
        )
        .unwrap();

        let (config, unknown) = Config::migrate(&legacy, &target).unwrap();

        assert!(unknown.is_empty(), "{:?}", unknown);
        assert!(legacy.exists(), "the old file is left where it is");
        assert_eq!(config.config_path, target.display().to_string());
        assert_eq!(config.server.port, 8788);

        let written = Config::parse_kdl(&std::fs::read_to_string(&target).unwrap()).unwrap();
        assert_eq!(written.server.port, 8788);
        assert_eq!(written.ancillaries.segments, vec!["~/proj/*"]);
        assert_eq!(written.ancillaries.max_per_segment, 4);
        assert_eq!(written.aliases.get("show").unwrap(), "breq get $1");
    }

    #[test]
    fn migration_reports_settings_toren_no_longer_has() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("config.toml");
        std::fs::write(
            &legacy,
            r#"
[intents]
ship = "breq complete"

[ancillaries]
segments = ["~/proj/*"]
pool_size = 10
"#,
        )
        .unwrap();

        let (config, unknown) = Config::migrate(&legacy, &dir.path().join("config.kdl")).unwrap();

        assert!(unknown.contains(&"intents".to_string()), "{:?}", unknown);
        assert!(
            unknown.contains(&"ancillaries.pool_size".to_string()),
            "{:?}",
            unknown
        );
        assert_eq!(config.ancillaries.segments, vec!["~/proj/*"]);
    }

    #[test]
    fn a_legacy_task_source_still_reads() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("config.toml");
        std::fs::write(&legacy, "[tasks]\ndefault_source = \"runes\"\n").unwrap();

        let (config, unknown) = Config::migrate(&legacy, &dir.path().join("config.kdl")).unwrap();
        assert_eq!(config.tasks.sources, vec!["runes"]);
        assert!(unknown.is_empty(), "{:?}", unknown);
    }

    /// Run `f` with every tracing event collected, so a warning can be asserted on.
    fn captured(f: impl FnOnce()) -> String {
        use std::io::Write;
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::fmt::MakeWriter;

        #[derive(Clone, Default)]
        struct Buffer(Arc<Mutex<Vec<u8>>>);

        impl Write for Buffer {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        impl<'a> MakeWriter<'a> for Buffer {
            type Writer = Self;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let buffer = Buffer::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(buffer.clone())
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, f);

        let bytes = buffer.0.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }
}
