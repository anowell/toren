//! Choosing and launching a coding agent.
//!
//! Everything agent-specific lives in an agent resolver plugin: how to build argv, and how to
//! read the agent's own session state. This module is the thin Rust side — which agent, which
//! model, and handing the resulting argv to rmux (or exec).
//!
//! Adding an agent is one `.rhai` file, no release.

use anyhow::{Context, Result};
use std::ffi::{OsStr, OsString};
use std::fmt;

use crate::plugins::PluginManager;

/// The `PATH` a command spawned from this process would be looked up in.
fn current_path() -> Option<OsString> {
    std::env::var_os("PATH")
}

/// Whether `binary` resolves as an executable in `path`, ignoring the working directory: a name
/// that only works because of where the daemon happens to be running is not installed.
fn resolves_in(binary: &str, path: Option<&OsStr>) -> bool {
    which::which_in_global(binary, path).is_ok_and(|mut found| found.next().is_some())
}

/// An agent plus an optional model override, e.g. `claude:opus` or `codex:o3`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSpec {
    pub name: String,
    pub model: Option<String>,
}

impl AgentSpec {
    /// Parse `"claude"` or `"claude:opus"`.
    pub fn parse(s: &str) -> Self {
        match s.split_once(':') {
            Some((name, model)) if !model.is_empty() => Self {
                name: name.to_string(),
                model: Some(model.to_string()),
            },
            _ => Self {
                name: s.trim_end_matches(':').to_string(),
                model: None,
            },
        }
    }

    /// Resolve which agent to use.
    ///
    /// Priority: explicit request > the workspace's own last agent > config > first installed
    /// agent whose binary is on PATH. The workspace's own choice sits high on purpose — coming
    /// back to a workspace should reach the agent that has the session history there.
    ///
    /// An explicit request for an unknown agent is an error the user should see. A *stored*
    /// choice that no longer resolves (a removed plugin) is not — it falls through to the next
    /// source rather than wedging the workspace.
    pub fn resolve(
        plugins: &PluginManager,
        requested: Option<&str>,
        workspace_agent: Option<&Self>,
        configured: Option<&str>,
    ) -> Result<Self> {
        if let Some(requested) = requested {
            let spec = Self::parse(requested);
            if !plugins.has_agent(&spec.name) {
                anyhow::bail!(
                    "Unknown agent '{}'. Installed: {}. Add one at \
                     ~/.toren/plugins/agents/{}.rhai",
                    spec.name,
                    plugins.list_agents().join(", "),
                    spec.name
                );
            }
            return Ok(spec);
        }

        let stored = workspace_agent
            .cloned()
            .into_iter()
            .chain(configured.map(Self::parse));
        for spec in stored {
            if plugins.has_agent(&spec.name) {
                return Ok(spec);
            }
            tracing::warn!(
                "Stored agent '{}' is not installed; falling back",
                spec.name
            );
        }

        Self::detect(plugins)
    }

    /// First installed agent whose binary is on PATH.
    pub fn detect(plugins: &PluginManager) -> Result<Self> {
        for name in plugins.list_agents() {
            let spec = Self::parse(name);
            if let Ok(binary) = spec.binary(plugins) {
                if resolves_in(&binary, current_path().as_deref()) {
                    return Ok(spec);
                }
            }
        }
        anyhow::bail!(
            "No coding agent found on PATH. Installed agent plugins: {}",
            plugins.list_agents().join(", ")
        )
    }

    /// Whether this agent's launch binary resolves on `path`, a `PATH`-shaped list of directories.
    ///
    /// An agent that cannot say what it launches counts as installed: a plugin whose `argv` is
    /// broken belongs in the list, failing loudly when someone starts it, rather than quietly
    /// missing from it.
    pub fn installed_in(&self, plugins: &PluginManager, path: Option<&OsStr>) -> bool {
        match self.binary(plugins) {
            Ok(binary) => resolves_in(&binary, path),
            Err(_) => true,
        }
    }

    /// [`Self::installed_in`] against this process's own `PATH` — what a spawn would use.
    pub fn installed(&self, plugins: &PluginManager) -> bool {
        self.installed_in(plugins, current_path().as_deref())
    }

    /// The program an agent launches, taken from its own argv contract.
    pub fn binary(&self, plugins: &PluginManager) -> Result<String> {
        let argv = plugins
            .agent_argv(&self.name, self.ctx_map(None, false, None))
            .with_context(|| format!("Agent '{}' failed to build argv", self.name))?;
        Ok(argv[0].clone())
    }

    /// Full argv for a fresh run.
    pub fn argv(
        &self,
        plugins: &PluginManager,
        prompt: Option<&str>,
        auto_approve: bool,
    ) -> Result<Vec<String>> {
        plugins.agent_argv(&self.name, self.ctx_map(prompt, auto_approve, None))
    }

    /// Full argv for resuming one *named* session — the one the caller picked, via
    /// [`crate::sessions::resume_target`].
    pub fn resume_argv_for(
        &self,
        plugins: &PluginManager,
        session_id: Option<&str>,
        prompt: Option<&str>,
        auto_approve: bool,
    ) -> Result<Vec<String>> {
        plugins.agent_resume_argv(&self.name, self.ctx_map(prompt, auto_approve, session_id))
    }

    fn ctx_map(
        &self,
        prompt: Option<&str>,
        auto_approve: bool,
        session_id: Option<&str>,
    ) -> rhai::Map {
        let mut map = rhai::Map::new();
        map.insert(
            "prompt".into(),
            match prompt {
                Some(p) => rhai::Dynamic::from(p.to_string()),
                None => rhai::Dynamic::UNIT,
            },
        );
        map.insert(
            "model".into(),
            match &self.model {
                Some(m) => rhai::Dynamic::from(m.clone()),
                None => rhai::Dynamic::UNIT,
            },
        );
        map.insert("auto_approve".into(), rhai::Dynamic::from(auto_approve));
        map.insert(
            "session_id".into(),
            match session_id {
                Some(s) => rhai::Dynamic::from(s.to_string()),
                None => rhai::Dynamic::UNIT,
            },
        );
        map
    }

    /// The packed `name[:model]` form `-a` accepts and config stores.
    pub fn packed(&self) -> String {
        match &self.model {
            Some(model) => format!("{}:{}", self.name, model),
            None => self.name.clone(),
        }
    }
}

impl fmt::Display for AgentSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)?;
        if let Some(model) = &self.model {
            write!(f, " ({})", model)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn plugins() -> PluginManager {
        PluginManager::new(Path::new("/nonexistent")).unwrap()
    }

    #[test]
    fn parses_model_suffix() {
        assert_eq!(
            AgentSpec::parse("claude:opus"),
            AgentSpec {
                name: "claude".into(),
                model: Some("opus".into())
            }
        );
        assert_eq!(
            AgentSpec::parse("codex"),
            AgentSpec {
                name: "codex".into(),
                model: None
            }
        );
    }

    #[test]
    fn resolution_prefers_the_explicit_request() {
        let plugins = plugins();
        let stored = AgentSpec::parse("claude");
        let spec = AgentSpec::resolve(&plugins, Some("codex"), Some(&stored), Some("pi")).unwrap();
        assert_eq!(spec.name, "codex");
    }

    #[test]
    fn resolution_falls_back_to_the_workspace_agent() {
        let plugins = plugins();
        let stored = AgentSpec::parse("claude:opus");
        let spec = AgentSpec::resolve(&plugins, None, Some(&stored), Some("pi")).unwrap();
        assert_eq!(spec.name, "claude");
        assert_eq!(spec.model.as_deref(), Some("opus"));
    }

    #[test]
    fn unknown_agents_name_where_to_add_one() {
        let plugins = plugins();
        let err = AgentSpec::resolve(&plugins, Some("nope"), None, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("plugins/agents/nope.rhai"), "{}", err);
    }

    #[test]
    fn argv_comes_from_the_plugin() {
        let plugins = plugins();
        let spec = AgentSpec::parse("claude:opus");
        assert_eq!(
            spec.argv(&plugins, Some("go"), false).unwrap(),
            vec!["claude", "--model", "opus", "go"]
        );
        assert_eq!(spec.binary(&plugins).unwrap(), "claude");
        assert_eq!(spec.packed(), "claude:opus");
    }

    /// A directory holding an executable named `binary`, to stand in for a PATH entry.
    fn path_with(binary: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(binary);
        std::fs::write(&file, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        dir
    }

    #[test]
    fn installed_follows_the_agents_own_launch_binary() {
        let plugins = plugins();
        let dir = path_with("claude");
        let path = Some(dir.path().as_os_str());

        assert!(AgentSpec::parse("claude:opus").installed_in(&plugins, path));
        assert!(!AgentSpec::parse("codex").installed_in(&plugins, path));
        assert!(!AgentSpec::parse("claude").installed_in(&plugins, None));
    }

    #[test]
    fn an_agent_that_cannot_build_argv_still_counts_as_installed() {
        let dir = tempfile::tempdir().unwrap();
        let agents = dir.path().join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            agents.join("broken.rhai"),
            r#"fn argv(ctx) { throw "nope" }"#,
        )
        .unwrap();
        let plugins = PluginManager::new(dir.path()).unwrap();

        let spec = AgentSpec::parse("broken");
        assert!(spec.binary(&plugins).is_err());
        assert!(spec.installed_in(&plugins, None));
    }
}
