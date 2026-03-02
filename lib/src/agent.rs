use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;
use std::process::Command;

/// Supported coding agent backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Codex,
    Gemini,
    Opencode,
}

impl AgentKind {
    /// Binary name on PATH.
    pub fn binary_name(self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Gemini => "gemini",
            AgentKind::Opencode => "opencode",
        }
    }

    /// Human-readable display name.
    pub fn display_name(self) -> &'static str {
        match self {
            AgentKind::Claude => "Claude",
            AgentKind::Codex => "Codex",
            AgentKind::Gemini => "Gemini",
            AgentKind::Opencode => "OpenCode",
        }
    }

    /// All known agent kinds in detection priority order.
    pub fn all() -> &'static [AgentKind] {
        &[
            AgentKind::Claude,
            AgentKind::Codex,
            AgentKind::Gemini,
            AgentKind::Opencode,
        ]
    }

    /// The CLI flag used to specify a model override.
    fn model_flag(self) -> &'static str {
        match self {
            AgentKind::Claude => "--model",
            AgentKind::Codex => "-m",
            AgentKind::Gemini => "--model",
            AgentKind::Opencode => "--model",
        }
    }

    /// The CLI flag for auto-approve / skip-permissions (daemon mode).
    fn auto_approve_flag(self) -> Option<&'static str> {
        match self {
            AgentKind::Claude => Some("--dangerously-skip-permissions"),
            AgentKind::Codex => Some("--full-auto"),
            AgentKind::Gemini => None,
            AgentKind::Opencode => None,
        }
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

/// A resolved agent: kind + optional model override.
#[derive(Debug, Clone)]
pub struct Agent {
    pub kind: AgentKind,
    pub model: Option<String>,
}

impl Agent {
    /// Parse an agent string like `"claude"`, `"codex:o3"`, or `"gemini:flash"`.
    pub fn parse(s: &str) -> Result<Self> {
        let (name, model) = match s.split_once(':') {
            Some((n, m)) => (n, Some(m.to_string())),
            None => (s, None),
        };

        let kind = match name {
            "claude" => AgentKind::Claude,
            "codex" => AgentKind::Codex,
            "gemini" => AgentKind::Gemini,
            "opencode" => AgentKind::Opencode,
            _ => bail!("Unknown agent: '{}'. Expected one of: claude, codex, gemini, opencode", name),
        };

        Ok(Agent { kind, model })
    }

    /// Auto-detect the first available agent on PATH.
    pub fn detect() -> Result<Self> {
        for &kind in AgentKind::all() {
            if which::which(kind.binary_name()).is_ok() {
                return Ok(Agent { kind, model: None });
            }
        }
        bail!(
            "No coding agent found on PATH. Install one of: claude, codex, gemini, opencode"
        )
    }

    /// Build a `std::process::Command` for interactive (breq) use.
    ///
    /// - Sets the binary and working directory
    /// - Adds model flag if a model override is set
    /// - For Claude: adds `--append-system-prompt` for intent text
    /// - For others: prepends intent text to the prompt
    /// - Adds the prompt as a positional argument
    pub fn build_command(
        &self,
        prompt: &str,
        cwd: &Path,
        system_prompt: Option<&str>,
    ) -> Command {
        let mut cmd = Command::new(self.kind.binary_name());
        cmd.current_dir(cwd);

        if let Some(ref model) = self.model {
            cmd.arg(self.kind.model_flag()).arg(model);
        }

        match self.kind {
            AgentKind::Claude => {
                if let Some(sp) = system_prompt {
                    cmd.arg("--append-system-prompt").arg(sp);
                }
                cmd.arg(prompt);
            }
            _ => {
                // Non-Claude agents: prepend intent to prompt
                let full_prompt = match system_prompt {
                    Some(sp) => format!("{}\n\n---\n\n{}", sp, prompt),
                    None => prompt.to_string(),
                };
                cmd.arg(&full_prompt);
            }
        }

        cmd
    }

    /// Build a `tokio::process::Command` for daemon (headless) use.
    ///
    /// Same as `build_command` but also adds auto-approve flags.
    pub fn build_daemon_command(
        &self,
        prompt: &str,
        cwd: &Path,
        system_prompt: Option<&str>,
    ) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(self.kind.binary_name());
        cmd.current_dir(cwd);

        if let Some(ref model) = self.model {
            cmd.arg(self.kind.model_flag()).arg(model);
        }

        // Auto-approve flag for daemon mode
        if let Some(flag) = self.kind.auto_approve_flag() {
            cmd.arg(flag);
        }

        // Print mode for non-interactive use (Claude-specific)
        if self.kind == AgentKind::Claude {
            cmd.arg("--print");
        }

        match self.kind {
            AgentKind::Claude => {
                if let Some(sp) = system_prompt {
                    cmd.arg("--append-system-prompt").arg(sp);
                }
                cmd.arg(prompt);
            }
            _ => {
                let full_prompt = match system_prompt {
                    Some(sp) => format!("{}\n\n---\n\n{}", sp, prompt),
                    None => prompt.to_string(),
                };
                cmd.arg(&full_prompt);
            }
        }

        cmd
    }
}

impl fmt::Display for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind.display_name())?;
        if let Some(ref model) = self.model {
            write!(f, " ({})", model)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_agent() {
        let agent = Agent::parse("claude").unwrap();
        assert_eq!(agent.kind, AgentKind::Claude);
        assert_eq!(agent.model, None);
    }

    #[test]
    fn parse_agent_with_model() {
        let agent = Agent::parse("codex:o3").unwrap();
        assert_eq!(agent.kind, AgentKind::Codex);
        assert_eq!(agent.model, Some("o3".to_string()));
    }

    #[test]
    fn parse_agent_with_model_claude() {
        let agent = Agent::parse("claude:sonnet-4").unwrap();
        assert_eq!(agent.kind, AgentKind::Claude);
        assert_eq!(agent.model, Some("sonnet-4".to_string()));
    }

    #[test]
    fn parse_all_agents() {
        for name in &["claude", "codex", "gemini", "opencode"] {
            let agent = Agent::parse(name).unwrap();
            assert_eq!(agent.kind.binary_name(), *name);
        }
    }

    #[test]
    fn parse_unknown_agent_errors() {
        assert!(Agent::parse("unknown").is_err());
    }

    #[test]
    fn display_names() {
        assert_eq!(AgentKind::Claude.display_name(), "Claude");
        assert_eq!(AgentKind::Codex.display_name(), "Codex");
        assert_eq!(AgentKind::Gemini.display_name(), "Gemini");
        assert_eq!(AgentKind::Opencode.display_name(), "OpenCode");
    }

    #[test]
    fn binary_names() {
        assert_eq!(AgentKind::Claude.binary_name(), "claude");
        assert_eq!(AgentKind::Codex.binary_name(), "codex");
        assert_eq!(AgentKind::Gemini.binary_name(), "gemini");
        assert_eq!(AgentKind::Opencode.binary_name(), "opencode");
    }

    #[test]
    fn build_command_claude_with_system_prompt() {
        let agent = Agent::parse("claude:sonnet-4").unwrap();
        let cmd = agent.build_command("fix the bug", Path::new("/tmp"), Some("You are a coder"));
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();
        assert_eq!(args, vec![
            "--model", "sonnet-4",
            "--append-system-prompt", "You are a coder",
            "fix the bug",
        ]);
    }

    #[test]
    fn build_command_codex_no_system_prompt() {
        let agent = Agent::parse("codex").unwrap();
        let cmd = agent.build_command("fix the bug", Path::new("/tmp"), None);
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();
        assert_eq!(args, vec!["fix the bug"]);
    }

    #[test]
    fn build_command_codex_with_system_prompt_prepends() {
        let agent = Agent::parse("codex:o3").unwrap();
        let cmd = agent.build_command("fix the bug", Path::new("/tmp"), Some("You are a coder"));
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();
        assert_eq!(args, vec![
            "-m", "o3",
            "You are a coder\n\n---\n\nfix the bug",
        ]);
    }

    #[test]
    fn agent_display() {
        let a1 = Agent::parse("claude").unwrap();
        assert_eq!(a1.to_string(), "Claude");

        let a2 = Agent::parse("codex:o3").unwrap();
        assert_eq!(a2.to_string(), "Codex (o3)");
    }

    #[test]
    fn auto_approve_flags() {
        assert_eq!(AgentKind::Claude.auto_approve_flag(), Some("--dangerously-skip-permissions"));
        assert_eq!(AgentKind::Codex.auto_approve_flag(), Some("--full-auto"));
        assert_eq!(AgentKind::Gemini.auto_approve_flag(), None);
        assert_eq!(AgentKind::Opencode.auto_approve_flag(), None);
    }
}
