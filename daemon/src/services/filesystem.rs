use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use toren_lib::Config;

pub struct FilesystemService {
    approved_directories: Vec<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FileInfo {
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DirListing {
    pub path: String,
    pub entries: Vec<FileInfo>,
}

impl FilesystemService {
    pub fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            approved_directories: config.approved_directories.clone(),
        })
    }

    pub fn read_file(&self, path: &Path) -> Result<String> {
        self.validate_path(path)?;

        std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read file: {}", path.display()))
    }

    pub fn write_file(&self, path: &Path, content: &str) -> Result<()> {
        self.validate_path(path)?;

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).context("Failed to create parent directory")?;
        }

        std::fs::write(path, content)
            .with_context(|| format!("Failed to write file: {}", path.display()))
    }

    pub fn list_directory(&self, path: &Path) -> Result<DirListing> {
        self.validate_path(path)?;

        let entries: Result<Vec<_>> = std::fs::read_dir(path)
            .context("Failed to read directory")?
            .map(|entry| {
                let entry = entry?;
                let metadata = entry.metadata()?;
                let modified = metadata
                    .modified()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();

                Ok(FileInfo {
                    path: entry.path().display().to_string(),
                    is_dir: metadata.is_dir(),
                    size: metadata.len(),
                    modified,
                })
            })
            .collect();

        Ok(DirListing {
            path: path.display().to_string(),
            entries: entries?,
        })
    }

    #[allow(dead_code)]
    pub fn file_exists(&self, path: &Path) -> Result<bool> {
        self.validate_path(path)?;
        Ok(path.exists())
    }

    fn validate_path(&self, path: &Path) -> Result<()> {
        let canonical = match path.canonicalize() {
            Ok(p) => p,
            Err(_) => {
                // If path doesn't exist, try to canonicalize parent
                if let Some(parent) = path.parent() {
                    parent
                        .canonicalize()
                        .context("Failed to canonicalize parent directory")?
                } else {
                    return Err(anyhow!("Invalid path"));
                }
            }
        };

        for approved in &self.approved_directories {
            let approved_canonical = approved
                .canonicalize()
                .context("Failed to canonicalize approved directory")?;

            if canonical.starts_with(&approved_canonical) {
                return Ok(());
            }
        }

        Err(anyhow!(
            "Path not in approved directories: {}",
            path.display()
        ))
    }
}
