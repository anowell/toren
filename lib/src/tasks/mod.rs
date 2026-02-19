pub mod beads;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Represents a task from any supported task tracking system
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub provider: TaskProvider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskProvider {
    Beads,
}

impl std::fmt::Display for TaskProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskProvider::Beads => write!(f, "beads"),
        }
    }
}

/// Fetch a task by ID, auto-detecting the provider based on available systems
pub fn fetch_task(task_id: &str, working_dir: &std::path::Path) -> Result<Task> {
    // For now, only beads is supported
    beads::fetch_bead(task_id, working_dir)
}

/// Generate a prompt from a task using the provided template.
/// Supports minijinja variables: task.id, task.title, plus any ws/repo context if provided.
/// Falls back to simple string replacement for backwards compatibility with {{task_id}}.
pub fn generate_prompt(task: &Task, template: &str) -> String {
    let ctx = crate::workspace_setup::WorkspaceContext {
        ws: crate::workspace_setup::WorkspaceInfo {
            name: String::new(),
            num: 0,
            path: String::new(),
        },
        repo: crate::workspace_setup::RepoInfo {
            root: String::new(),
            name: String::new(),
        },
        vars: std::collections::HashMap::new(),
        config: None,
        task: Some(crate::workspace_setup::TaskInfo {
            id: task.id.clone(),
            title: task.title.clone(),
        }),
    };
    crate::workspace_setup::render_template(template, &ctx).unwrap_or_else(|_| {
        // Fallback to simple replacement for backwards compatibility
        template
            .replace("{{task_id}}", &task.id)
            .replace("{{task_provider}}", &task.provider.to_string())
    })
}
