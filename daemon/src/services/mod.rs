use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;

use crate::security::SecurityContext;
use toren_lib::Config;

pub mod command;
pub mod filesystem;
pub mod vcs;

/// Derive approved directories from config segments and workspace root.
/// Expands segment globs/paths and includes the workspace root.
pub fn derive_approved_directories(config: &Config) -> Vec<PathBuf> {
    let (roots, literals) = config.resolve_segment_paths();
    let mut dirs: Vec<PathBuf> = roots.clone();
    dirs.extend(literals.iter().cloned());
    let ws_root = &config.ancillaries.workspace_root;
    let canonical = ws_root.canonicalize().unwrap_or_else(|_| ws_root.clone());
    if !dirs.contains(&canonical) {
        dirs.push(canonical);
    }
    dirs
}

#[derive(Clone)]
pub struct Services {
    pub filesystem: Arc<filesystem::FilesystemService>,
    pub command: Arc<command::CommandService>,
    pub vcs: Arc<vcs::VcsService>,
}

impl Services {
    pub async fn new(config: &Config, _security_ctx: &SecurityContext) -> Result<Self> {
        let filesystem = Arc::new(filesystem::FilesystemService::new(config)?);
        let command = Arc::new(command::CommandService::new(config)?);
        let vcs = Arc::new(vcs::VcsService::new(config)?);

        Ok(Self {
            filesystem,
            command,
            vcs,
        })
    }
}
