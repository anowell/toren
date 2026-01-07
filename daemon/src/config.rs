use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SegmentsConfig {
    #[serde(default)]
    pub globs: Vec<String>,

    #[serde(default)]
    pub roots: Vec<PathBuf>,

    #[serde(default)]
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AncillaryConfig {
    pub max_concurrent: usize,
    pub default_model: String,
    #[serde(default)]
    pub workspace_root: Option<PathBuf>,
}

impl Default for AncillaryConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 5,
            default_model: "claude-sonnet-4-5-20250929".to_string(),
            workspace_root: None,
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

impl Config {
    pub fn load() -> Result<Self> {
        let config_path = Self::find_config_file()?;

        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)
                .context("Failed to read config file")?;
            let mut config: Config = toml::from_str(&content)
                .context("Failed to parse config file")?;
            config.config_path = config_path.display().to_string();
            Ok(config)
        } else {
            // Create default config
            let config = Self::default();
            config.save(&config_path)?;
            Ok(config)
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let content = toml::to_string_pretty(self)
            .context("Failed to serialize config")?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .context("Failed to create config directory")?;
        }

        std::fs::write(path, content)
            .context("Failed to write config file")?;

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
        let current_dir = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."));

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
