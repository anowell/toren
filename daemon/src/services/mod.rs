use anyhow::Result;
use std::sync::Arc;

use crate::security::SecurityContext;
use toren_lib::Config;

pub mod command;
pub mod filesystem;
pub mod vcs;

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
