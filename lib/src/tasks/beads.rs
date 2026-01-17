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

/// Create a new bead from a prompt/description.
/// Returns the created bead ID.
pub fn create_bead(
    title: &str,
    description: Option<&str>,
    working_dir: &Path,
) -> Result<String> {
    let mut args = vec!["create", "--silent", "--title", title];

    if let Some(desc) = description {
        args.push("--description");
        args.push(desc);
    }

    let output = Command::new("bd")
        .args(&args)
        .current_dir(working_dir)
        .output()
        .context("Failed to execute bd create")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("bd create failed: {}", stderr.trim()));
    }

    let bead_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if bead_id.is_empty() {
        return Err(anyhow!("bd create returned empty bead ID"));
    }

    Ok(bead_id)
}

/// Create a bead from a prompt and immediately claim it.
/// Returns the created bead ID.
pub fn create_and_claim_bead(
    title: &str,
    description: Option<&str>,
    assignee: &str,
    working_dir: &Path,
) -> Result<String> {
    let bead_id = create_bead(title, description, working_dir)?;
    claim_bead(&bead_id, assignee, working_dir)?;
    Ok(bead_id)
}
