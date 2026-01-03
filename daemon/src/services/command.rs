use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::config::Config;

pub struct CommandService {
    approved_directories: Vec<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CommandRequest {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type")]
pub enum CommandOutput {
    Stdout { line: String },
    Stderr { line: String },
    Exit { code: i32 },
    Error { message: String },
}

impl CommandService {
    pub fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            approved_directories: config.approved_directories.clone(),
        })
    }

    pub async fn execute(
        &self,
        request: CommandRequest,
    ) -> Result<mpsc::Receiver<CommandOutput>> {
        let cwd = if let Some(cwd_str) = request.cwd {
            PathBuf::from(cwd_str)
        } else {
            std::env::current_dir()?
        };

        self.validate_directory(&cwd)?;

        let (tx, rx) = mpsc::channel(100);

        tokio::spawn(async move {
            let result = Self::run_command(request.command, request.args, cwd, tx.clone()).await;

            if let Err(e) = result {
                let _ = tx
                    .send(CommandOutput::Error {
                        message: e.to_string(),
                    })
                    .await;
            }
        });

        Ok(rx)
    }

    async fn run_command(
        command: String,
        args: Vec<String>,
        cwd: PathBuf,
        tx: mpsc::Sender<CommandOutput>,
    ) -> Result<()> {
        let mut child = Command::new(&command)
            .args(&args)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to spawn command")?;

        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let tx_stdout = tx.clone();
        let stdout_handle = tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                if tx_stdout
                    .send(CommandOutput::Stdout { line })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        let tx_stderr = tx.clone();
        let stderr_handle = tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                if tx_stderr
                    .send(CommandOutput::Stderr { line })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        let status = child.wait().await?;

        stdout_handle.await?;
        stderr_handle.await?;

        let code = status.code().unwrap_or(-1);
        let _ = tx.send(CommandOutput::Exit { code }).await;

        Ok(())
    }

    fn validate_directory(&self, path: &PathBuf) -> Result<()> {
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
