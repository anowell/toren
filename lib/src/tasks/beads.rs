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
    let beads: Vec<BeadResponse> =
        serde_json::from_str(&stdout).context("Failed to parse bd output")?;

    let bead = beads.into_iter().next()
        .ok_or_else(|| anyhow!("No bead found with id: {}", bead_id))?;

    Ok(Task {
        id: bead.id,
        title: bead.title,
        description: bead.description,
        provider: TaskProvider::Beads,
    })
}

/// Update bead status
pub fn update_bead_status(bead_id: &str, status: &str, working_dir: &Path) -> Result<()> {
    let output = Command::new("bd")
        .args(["update", bead_id, "--status", status])
        .current_dir(working_dir)
        .output()
        .context("Failed to execute bd update")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("bd update failed: {}", stderr.trim()));
    }

    Ok(())
}

/// Update bead assignee
pub fn update_bead_assignee(bead_id: &str, assignee: &str, working_dir: &Path) -> Result<()> {
    let output = Command::new("bd")
        .args(["update", bead_id, "--assignee", assignee])
        .current_dir(working_dir)
        .output()
        .context("Failed to execute bd update")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("bd update failed: {}", stderr.trim()));
    }

    Ok(())
}

/// Claim a bead (set status to in_progress and assignee)
pub fn claim_bead(bead_id: &str, assignee: &str, working_dir: &Path) -> Result<()> {
    let output = Command::new("bd")
        .args(["update", bead_id, "--status", "in_progress", "--assignee", assignee])
        .current_dir(working_dir)
        .output()
        .context("Failed to execute bd update")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("bd update failed: {}", stderr.trim()));
    }

    Ok(())
}
