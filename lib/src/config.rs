use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use shellexpand;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(skip)]
    pub config_path: String,

    #[serde(default = "default_server")]
    pub server: ServerConfig,

    #[serde(default)]
    pub segments: SegmentsConfig,

    #[serde(default = "default_approved_directories")]
    pub approved_directories: Vec<PathBuf>,

    #[serde(default)]
    pub auto_approve: AutoApproveConfig,

    #[serde(default)]
    pub ancillary: AncillaryConfig,
}

fn default_server() -> ServerConfig {
    ServerConfig {
        host: "127.0.0.1".to_string(),
        port: 8787,
    }
}

fn default_approved_directories() -> Vec<PathBuf> {
    vec![std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

/// Configuration for segment discovery.
/// Segments are directories under configured roots.
/// Any subdirectory of a root is automatically a valid segment.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SegmentsConfig {
    /// Root directories that can contain segments.
    /// Any immediate child directory under a root is a valid segment.
    #[serde(default)]
    pub roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AncillaryConfig {
    pub max_concurrent: usize,
    pub default_model: String,
    #[serde(default)]
    pub workspace_root: Option<PathBuf>,
    /// Template for generating prompts from task IDs.
    /// Available placeholders: {{task_id}}, {{task_provider}}
    #[serde(default = "default_task_prompt_template")]
    pub task_prompt_template: String,
    /// Default pool size for ancillaries per segment
    #[serde(default = "default_pool_size")]
    pub pool_size: u32,
}

fn default_pool_size() -> u32 {
    10
}

fn default_task_prompt_template() -> String {
    "implement bead {{task_id}}".to_string()
}

impl Default for AncillaryConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 5,
            default_model: "claude-sonnet-4-5-20250929".to_string(),
            workspace_root: None,
            task_prompt_template: default_task_prompt_template(),
            pool_size: default_pool_size(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoApproveConfig {
    pub non_vcs_commands: bool,
    pub vcs_commands: bool,
    pub file_operations: bool,
}

impl Default for AutoApproveConfig {
    fn default() -> Self {
        Self {
            non_vcs_commands: true,
            vcs_commands: false,
            file_operations: false,
        }
    }
}

/// Expand shell-style paths (e.g., `~` to home directory)
fn expand_path(path: &Path) -> PathBuf {
    let path_str = path.to_string_lossy();
    PathBuf::from(shellexpand::tilde(&path_str).into_owned())
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

    /// Expand shell-style paths in all path fields
    fn expand_paths(&mut self) {
        // Expand segment roots
        self.segments.roots = self.segments.roots.iter().map(|p| expand_path(p)).collect();

        // Expand approved directories
        self.approved_directories = self
            .approved_directories
            .iter()
            .map(|p| expand_path(p))
            .collect();

        // Expand workspace root
        if let Some(ref ws_root) = self.ancillary.workspace_root {
            self.ancillary.workspace_root = Some(expand_path(ws_root));
        }
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
        // Try toren.toml in current directory first
        let local_config = PathBuf::from("toren.toml");
        if local_config.exists() {
            return Ok(local_config);
        }

        // Try .toren/config.toml (legacy)
        let legacy_config = PathBuf::from(".toren/config.toml");
        if legacy_config.exists() {
            return Ok(legacy_config);
        }

        // Try home directory
        if let Some(home) = dirs::home_dir() {
            let home_config = home.join(".config/toren/config.toml");
            if home_config.exists() {
                return Ok(home_config);
            }
            // Return home path even if it doesn't exist (we'll create it)
            return Ok(home_config);
        }

        // Fallback to local toren.toml
        Ok(local_config)
    }
}

impl Default for Config {
    fn default() -> Self {
        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        Self {
            config_path: String::new(),
            server: default_server(),
            segments: SegmentsConfig::default(),
            approved_directories: vec![current_dir],
            auto_approve: AutoApproveConfig::default(),
            ancillary: AncillaryConfig::default(),
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
