use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::process::Command;

use super::{Task, TaskProvider};

#[derive(Debug, Deserialize)]
struct BeadResponse {
    id: String,
    title: String,
    description: Option<String>,
}

/// Fetch a bead by ID using the bd CLI
pub fn fetch_bead(bead_id: &str, working_dir: &Path) -> Result<Task> {
    let output = Command::new("bd")
        .args(["show", bead_id, "--json"])
        .current_dir(working_dir)
        .output()
        .context("Failed to execute bd command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("bd show failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let bead: BeadResponse =
        serde_json::from_str(&stdout).context("Failed to parse bd output")?;

    Ok(Task {
        id: bead.id,
        title: bead.title,
        description: bead.description,
        provider: TaskProvider::Beads,
    })
}
