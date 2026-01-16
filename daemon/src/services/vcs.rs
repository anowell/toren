use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

use toren_lib::Config;

pub struct VcsService {
    approved_directories: Vec<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum VcsType {
    Git,
    Jj,
    None,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VcsStatus {
    pub vcs_type: VcsType,
    pub branch: Option<String>,
    pub modified: Vec<String>,
    pub added: Vec<String>,
    pub deleted: Vec<String>,
}

impl VcsService {
    pub fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            approved_directories: config.approved_directories.clone(),
        })
    }

    pub fn detect_vcs(&self, path: &Path) -> Result<VcsType> {
        self.validate_directory(path)?;

        // Check for jj first
        if path.join(".jj").exists() {
            return Ok(VcsType::Jj);
        }

        // Check for git
        if path.join(".git").exists() {
            return Ok(VcsType::Git);
        }

        // Walk up to find VCS root
        let mut current = path;
        while let Some(parent) = current.parent() {
            if parent.join(".jj").exists() {
                return Ok(VcsType::Jj);
            }
            if parent.join(".git").exists() {
                return Ok(VcsType::Git);
            }
            current = parent;
        }

        Ok(VcsType::None)
    }

    pub fn status(&self, path: &Path) -> Result<VcsStatus> {
        self.validate_directory(path)?;

        let vcs_type = self.detect_vcs(path)?;

        match vcs_type {
            VcsType::Git => self.git_status(path),
            VcsType::Jj => self.jj_status(path),
            VcsType::None => Ok(VcsStatus {
                vcs_type: VcsType::None,
                branch: None,
                modified: vec![],
                added: vec![],
                deleted: vec![],
            }),
        }
    }

    pub fn diff(&self, path: &Path) -> Result<String> {
        self.validate_directory(path)?;

        let vcs_type = self.detect_vcs(path)?;

        match vcs_type {
            VcsType::Git => self.git_diff(path),
            VcsType::Jj => self.jj_diff(path),
            VcsType::None => Ok(String::new()),
        }
    }

    fn git_status(&self, path: &Path) -> Result<VcsStatus> {
        let output = Command::new("git")
            .args(["status", "--porcelain=v1", "--branch"])
            .current_dir(path)
            .output()
            .context("Failed to run git status")?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut branch = None;
        let mut modified = Vec::new();
        let mut added = Vec::new();
        let mut deleted = Vec::new();

        for line in stdout.lines() {
            if line.starts_with("##") {
                let parts: Vec<&str> = line[3..].split("...").collect();
                branch = Some(parts[0].to_string());
            } else if line.len() >= 3 {
                let status = &line[0..2];
                let file = &line[3..];

                match status.trim() {
                    "M" | "MM" | "AM" => modified.push(file.to_string()),
                    "A" | "AA" => added.push(file.to_string()),
                    "D" | "DD" => deleted.push(file.to_string()),
                    _ => {}
                }
            }
        }

        Ok(VcsStatus {
            vcs_type: VcsType::Git,
            branch,
            modified,
            added,
            deleted,
        })
    }

    fn git_diff(&self, path: &Path) -> Result<String> {
        let output = Command::new("git")
            .args(["diff", "HEAD"])
            .current_dir(path)
            .output()
            .context("Failed to run git diff")?;

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    fn jj_status(&self, path: &Path) -> Result<VcsStatus> {
        let output = Command::new("jj")
            .args(["status"])
            .current_dir(path)
            .output()
            .context("Failed to run jj status")?;

        let stdout = String::from_utf8_lossy(&output.stdout);

        // Parse jj status output
        let mut modified = Vec::new();
        let mut added = Vec::new();
        let mut deleted = Vec::new();

        for line in stdout.lines() {
            let line = line.trim();
            if line.starts_with("M ") {
                modified.push(line[2..].to_string());
            } else if line.starts_with("A ") {
                added.push(line[2..].to_string());
            } else if line.starts_with("D ") {
                deleted.push(line[2..].to_string());
            }
        }

        // Get current branch/change info
        let branch_output = Command::new("jj")
            .args(["log", "-r", "@", "--no-graph", "-T", "description"])
            .current_dir(path)
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string());

        Ok(VcsStatus {
            vcs_type: VcsType::Jj,
            branch: branch_output,
            modified,
            added,
            deleted,
        })
    }

    fn jj_diff(&self, path: &Path) -> Result<String> {
        let output = Command::new("jj")
            .args(["diff"])
            .current_dir(path)
            .output()
            .context("Failed to run jj diff")?;

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    fn validate_directory(&self, path: &Path) -> Result<()> {
        let canonical = path.canonicalize().context("Invalid directory")?;

        for approved in &self.approved_directories {
            let approved_canonical = approved
                .canonicalize()
                .context("Failed to canonicalize approved directory")?;

            if canonical.starts_with(&approved_canonical) {
                return Ok(());
            }
        }

        anyhow::bail!("Directory not in approved list: {}", path.display())
    }
}
