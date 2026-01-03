use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandSet {
    pub id: String,
    pub name: String,
    pub vcs: Option<String>,
    pub commands: Vec<CommandDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandDef {
    pub id: String,
    pub label: String,
    pub command: String,
    #[serde(default)]
    pub params: Vec<CommandParam>,
    pub category: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub auto_approve: bool,
    #[serde(default)]
    pub requires_vcs: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandParam {
    pub name: String,
    #[serde(rename = "type")]
    pub param_type: String,
    pub prompt: String,
    #[serde(default)]
    pub default: Option<String>,
}

pub struct PluginManager {
    command_sets: HashMap<String, CommandSet>,
    plugin_dirs: Vec<PathBuf>,
}

impl PluginManager {
    pub fn new() -> Self {
        Self {
            command_sets: HashMap::new(),
            plugin_dirs: Vec::new(),
        }
    }

    pub fn add_plugin_dir(&mut self, dir: PathBuf) {
        self.plugin_dirs.push(dir);
    }

    pub fn load_all(&mut self) -> Result<()> {
        for dir in &self.plugin_dirs.clone() {
            if !dir.exists() {
                std::fs::create_dir_all(dir)
                    .context("Failed to create plugin directory")?;
                continue;
            }

            self.load_from_dir(dir)?;
        }

        info!("Loaded {} command sets", self.command_sets.len());
        Ok(())
    }

    fn load_from_dir(&mut self, dir: &Path) -> Result<()> {
        for entry in std::fs::read_dir(dir).context("Failed to read plugin directory")? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().and_then(|s| s.to_str()) == Some("yaml")
                || path.extension().and_then(|s| s.to_str()) == Some("yml")
            {
                match self.load_command_set(&path) {
                    Ok(command_set) => {
                        info!("Loaded command set: {} from {}", command_set.name, path.display());
                        self.command_sets.insert(command_set.id.clone(), command_set);
                    }
                    Err(e) => {
                        warn!("Failed to load command set from {}: {}", path.display(), e);
                    }
                }
            }
        }

        Ok(())
    }

    fn load_command_set(&self, path: &Path) -> Result<CommandSet> {
        let content = std::fs::read_to_string(path)
            .context("Failed to read command set file")?;

        let command_set: CommandSet = serde_yaml::from_str(&content)
            .context("Failed to parse command set YAML")?;

        Ok(command_set)
    }

    pub fn get_command_set(&self, id: &str) -> Option<&CommandSet> {
        self.command_sets.get(id)
    }

    pub fn list_command_sets(&self) -> Vec<&CommandSet> {
        self.command_sets.values().collect()
    }

    pub fn find_command(&self, command_id: &str) -> Option<(&CommandSet, &CommandDef)> {
        for command_set in self.command_sets.values() {
            if let Some(cmd) = command_set.commands.iter().find(|c| c.id == command_id) {
                return Some((command_set, cmd));
            }
        }
        None
    }

    pub fn interpolate_command(
        &self,
        command: &str,
        params: &HashMap<String, String>,
    ) -> String {
        let mut result = command.to_string();

        for (key, value) in params {
            let placeholder = format!("{{{}}}", key);
            result = result.replace(&placeholder, value);
        }

        result
    }
}
