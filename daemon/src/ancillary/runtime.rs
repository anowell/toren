use anyhow::{Context, Result};
use claude_agent_sdk_rs::{
    query_stream, ClaudeAgentOptions, ContentBlock, Message, PermissionMode,
};
use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::{error, info, warn};

use super::work_log::{WorkLog, WorkOp};
use toren_lib::{Agent, AgentKind, Assignment};

/// Status of an ancillary's work execution
#[derive(Debug, Clone, PartialEq)]
pub enum WorkStatus {
    /// Starting up, spawning Claude Code
    Starting,
    /// Actively working
    Working,
    /// Waiting for user input/approval
    #[allow(dead_code)]
    AwaitingInput,
    /// Work completed successfully
    Completed,
    /// Work failed
    Failed { error: String },
}

impl std::fmt::Display for WorkStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkStatus::Starting => write!(f, "starting"),
            WorkStatus::Working => write!(f, "working"),
            WorkStatus::AwaitingInput => write!(f, "awaiting_input"),
            WorkStatus::Completed => write!(f, "completed"),
            WorkStatus::Failed { error } => write!(f, "failed: {}", error),
        }
    }
}

/// Input that clients can send to an ancillary
#[derive(Debug, Clone)]
pub enum ClientInput {
    /// Send a message to Claude
    Message { content: String, client_id: String },
    /// Interrupt the current work
    Interrupt,
}

/// An ancillary work execution context
pub struct AncillaryWork {
    /// Ancillary identifier (e.g., "Toren One")
    #[allow(dead_code)]
    pub ancillary_id: String,
    /// The assignment being worked on
    #[allow(dead_code)]
    pub assignment: Assignment,
    /// Current work status
    status: Arc<RwLock<WorkStatus>>,
    /// Work log for persistence and replay
    work_log: Arc<RwLock<WorkLog>>,
    /// Broadcast channel for work events (to clients)
    event_tx: broadcast::Sender<super::work_log::WorkEvent>,
    /// Input channel (from clients)
    input_tx: mpsc::Sender<ClientInput>,
    /// Handle to the work task
    task_handle: Option<tokio::task::JoinHandle<()>>,
}

impl AncillaryWork {
    /// Start work on an assignment
    pub async fn start(ancillary_id: String, assignment: Assignment, agent: Agent) -> Result<Self> {
        let work_log =
            WorkLog::open(&ancillary_id, &assignment.id).context("Failed to open work log")?;

        let (event_tx, _) = broadcast::channel(1000);
        let (input_tx, input_rx) = mpsc::channel(100);

        let status = Arc::new(RwLock::new(WorkStatus::Starting));
        let work_log = Arc::new(RwLock::new(work_log));

        let mut work = Self {
            ancillary_id: ancillary_id.clone(),
            assignment: assignment.clone(),
            status: status.clone(),
            work_log: work_log.clone(),
            event_tx: event_tx.clone(),
            input_tx,
            task_handle: None,
        };

        // Log assignment started
        {
            let mut log = work_log.write().await;
            let event = log.append(WorkOp::AssignmentStarted {
                task_id: assignment.task_id.clone().unwrap_or_default(),
            })?;
            let _ = event_tx.send(event);
        }

        // Spawn the work task
        let task_handle = tokio::spawn(Self::work_loop(
            ancillary_id,
            assignment,
            agent,
            status,
            work_log,
            event_tx,
            input_rx,
        ));

        work.task_handle = Some(task_handle);
        Ok(work)
    }

    /// The main work loop that runs a coding agent
    async fn work_loop(
        ancillary_id: String,
        assignment: Assignment,
        agent: Agent,
        status: Arc<RwLock<WorkStatus>>,
        work_log: Arc<RwLock<WorkLog>>,
        event_tx: broadcast::Sender<super::work_log::WorkEvent>,
        mut input_rx: mpsc::Receiver<ClientInput>,
    ) {
        info!(
            "{} starting work on {:?} via {}",
            ancillary_id, assignment.task_id, agent
        );

        // Update status to working
        {
            let mut s = status.write().await;
            *s = WorkStatus::Working;
        }
        Self::log_status(&work_log, &event_tx, "working").await;

        // Build the prompt from the assignment
        let prompt = match &assignment.source {
            toren_lib::AssignmentSource::Prompt { original_prompt } => original_prompt.clone(),
            toren_lib::AssignmentSource::Reference => {
                // Fetch task info and render using the act intent template
                let task_id = assignment.task_id.clone().unwrap_or_default();
                let title = assignment
                    .task_title
                    .clone()
                    .unwrap_or_else(|| task_id.clone());
                let ctx = toren_lib::WorkspaceContext {
                    ws: toren_lib::WorkspaceInfo {
                        name: assignment
                            .workspace_path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("")
                            .to_string(),
                        num: assignment.ancillary_num.unwrap_or(0),
                        path: assignment.workspace_path.display().to_string(),
                    },
                    repo: toren_lib::RepoInfo {
                        root: String::new(),
                        name: assignment.segment.clone(),
                    },
                    task: Some(toren_lib::TaskInfo {
                        id: task_id.clone(),
                        title,
                        description: None,
                        url: assignment.task_url.clone(),
                        source: assignment.task_source.clone(),
                    }),
                    vars: std::collections::HashMap::new(),
                };
                // TODO: read intent template from config (requires passing config to work loop)
                let template = toren_lib::config::IntentsConfig::default()
                    .entries.get("act").cloned().unwrap_or_default();
                toren_lib::render_template(&template, &ctx)
                    .unwrap_or_else(|_| format!("implement {}", task_id))
            }
        };

        match agent.kind {
            AgentKind::Claude => {
                // Use the Claude Agent SDK for native streaming
                Self::run_claude_sdk(
                    &ancillary_id,
                    &assignment,
                    &agent,
                    &prompt,
                    &status,
                    &work_log,
                    &event_tx,
                    &mut input_rx,
                )
                .await;
            }
            _ => {
                // Subprocess path for non-Claude agents
                Self::run_subprocess(
                    &ancillary_id,
                    &assignment,
                    &agent,
                    &prompt,
                    &status,
                    &work_log,
                    &event_tx,
                    &mut input_rx,
                )
                .await;
            }
        }
    }

    /// Run work via the Claude Agent SDK (native streaming).
    async fn run_claude_sdk(
        ancillary_id: &str,
        assignment: &Assignment,
        agent: &Agent,
        prompt: &str,
        status: &Arc<RwLock<WorkStatus>>,
        work_log: &Arc<RwLock<WorkLog>>,
        event_tx: &broadcast::Sender<super::work_log::WorkEvent>,
        input_rx: &mut mpsc::Receiver<ClientInput>,
    ) {
        let options = if let Some(ref model) = agent.model {
            ClaudeAgentOptions::builder()
                .cwd(assignment.workspace_path.clone())
                .permission_mode(PermissionMode::BypassPermissions)
                .max_turns(50u32)
                .model(model.clone())
                .build()
        } else {
            ClaudeAgentOptions::builder()
                .cwd(assignment.workspace_path.clone())
                .permission_mode(PermissionMode::BypassPermissions)
                .max_turns(50u32)
                .build()
        };

        // Run the query and stream results
        match query_stream(prompt, Some(options)).await {
            Ok(mut stream) => {
                while let Some(result) = stream.next().await {
                    match result {
                        Ok(message) => {
                            Self::handle_message(ancillary_id, message, work_log, event_tx)
                                .await;
                        }
                        Err(e) => {
                            error!("{} stream error: {}", ancillary_id, e);
                            Self::log_op(
                                work_log,
                                event_tx,
                                WorkOp::AssignmentFailed {
                                    error: e.to_string(),
                                },
                            )
                            .await;
                            let mut s = status.write().await;
                            *s = WorkStatus::Failed {
                                error: e.to_string(),
                            };
                            return;
                        }
                    }

                    // Check for interrupt
                    if let Ok(input) = input_rx.try_recv() {
                        match input {
                            ClientInput::Interrupt => {
                                warn!("{} interrupted", ancillary_id);
                                Self::log_op(
                                    work_log,
                                    event_tx,
                                    WorkOp::AssignmentFailed {
                                        error: "Interrupted by user".to_string(),
                                    },
                                )
                                .await;
                                let mut s = status.write().await;
                                *s = WorkStatus::Failed {
                                    error: "Interrupted".to_string(),
                                };
                                return;
                            }
                            ClientInput::Message { content, client_id } => {
                                // Log user message but can't inject mid-stream with current SDK
                                Self::log_op(
                                    work_log,
                                    event_tx,
                                    WorkOp::UserMessage { content, client_id },
                                )
                                .await;
                            }
                        }
                    }
                }

                // Completed successfully
                info!("{} completed work on {:?}", ancillary_id, assignment.task_id);
                Self::log_op(work_log, event_tx, WorkOp::AssignmentCompleted).await;
                let mut s = status.write().await;
                *s = WorkStatus::Completed;
            }
            Err(e) => {
                error!("{} failed to start: {}", ancillary_id, e);
                Self::log_op(
                    work_log,
                    event_tx,
                    WorkOp::AssignmentFailed {
                        error: e.to_string(),
                    },
                )
                .await;
                let mut s = status.write().await;
                *s = WorkStatus::Failed {
                    error: e.to_string(),
                };
            }
        }
    }

    /// Run work via subprocess for non-Claude agents.
    async fn run_subprocess(
        ancillary_id: &str,
        assignment: &Assignment,
        agent: &Agent,
        prompt: &str,
        status: &Arc<RwLock<WorkStatus>>,
        work_log: &Arc<RwLock<WorkLog>>,
        event_tx: &broadcast::Sender<super::work_log::WorkEvent>,
        input_rx: &mut mpsc::Receiver<ClientInput>,
    ) {
        use tokio::io::{AsyncBufReadExt, BufReader};

        let mut cmd = agent.build_daemon_command(prompt, &assignment.workspace_path, None);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        match cmd.spawn() {
            Ok(mut child) => {
                // Stream stdout — emit each line as it arrives
                if let Some(stdout) = child.stdout.take() {
                    let work_log = work_log.clone();
                    let event_tx = event_tx.clone();
                    let aid = ancillary_id.to_string();
                    tokio::spawn(async move {
                        let reader = BufReader::new(stdout);
                        let mut lines = reader.lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            if !line.is_empty() {
                                Self::log_op(
                                    &work_log,
                                    &event_tx,
                                    WorkOp::AssistantMessage {
                                        content: line,
                                    },
                                )
                                .await;
                            }
                        }
                        info!("{} stdout stream ended", aid);
                    });
                }

                // Stream stderr
                if let Some(stderr) = child.stderr.take() {
                    let aid = ancillary_id.to_string();
                    tokio::spawn(async move {
                        let reader = BufReader::new(stderr);
                        let mut lines = reader.lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            if !line.is_empty() {
                                info!("{} stderr: {}", aid, line);
                            }
                        }
                    });
                }

                // Wait for process to complete, with interrupt support
                loop {
                    tokio::select! {
                        exit_result = child.wait() => {
                            match exit_result {
                                Ok(exit_status) => {
                                    if exit_status.success() {
                                        info!(
                                            "{} completed work on {:?}",
                                            ancillary_id, assignment.task_id
                                        );
                                        Self::log_op(work_log, event_tx, WorkOp::AssignmentCompleted).await;
                                        let mut s = status.write().await;
                                        *s = WorkStatus::Completed;
                                    } else {
                                        let err_msg = format!(
                                            "{} exited with {}",
                                            agent.kind.binary_name(),
                                            exit_status
                                        );
                                        error!("{} {}", ancillary_id, err_msg);
                                        Self::log_op(
                                            work_log,
                                            event_tx,
                                            WorkOp::AssignmentFailed { error: err_msg.clone() },
                                        )
                                        .await;
                                        let mut s = status.write().await;
                                        *s = WorkStatus::Failed { error: err_msg };
                                    }
                                }
                                Err(e) => {
                                    let err_msg = format!("Failed to wait for {}: {}", agent.kind.binary_name(), e);
                                    error!("{} {}", ancillary_id, err_msg);
                                    Self::log_op(
                                        work_log,
                                        event_tx,
                                        WorkOp::AssignmentFailed { error: err_msg.clone() },
                                    )
                                    .await;
                                    let mut s = status.write().await;
                                    *s = WorkStatus::Failed { error: err_msg };
                                }
                            }
                            return;
                        }
                        input = input_rx.recv() => {
                            match input {
                                Some(ClientInput::Interrupt) => {
                                    warn!("{} interrupted, killing subprocess", ancillary_id);
                                    let _ = child.kill().await;
                                    Self::log_op(
                                        work_log,
                                        event_tx,
                                        WorkOp::AssignmentFailed {
                                            error: "Interrupted by user".to_string(),
                                        },
                                    )
                                    .await;
                                    let mut s = status.write().await;
                                    *s = WorkStatus::Failed {
                                        error: "Interrupted".to_string(),
                                    };
                                    return;
                                }
                                Some(ClientInput::Message { content, client_id }) => {
                                    Self::log_op(
                                        work_log,
                                        event_tx,
                                        WorkOp::UserMessage { content, client_id },
                                    )
                                    .await;
                                    // Continue waiting — can't inject messages into subprocess
                                }
                                None => {
                                    // Channel closed, continue waiting for process
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                let err_msg = format!("Failed to spawn {}: {}", agent.kind.binary_name(), e);
                error!("{} {}", ancillary_id, err_msg);
                Self::log_op(
                    work_log,
                    event_tx,
                    WorkOp::AssignmentFailed { error: err_msg.clone() },
                )
                .await;
                let mut s = status.write().await;
                *s = WorkStatus::Failed { error: err_msg };
            }
        }
    }

    /// Handle a message from Claude
    async fn handle_message(
        ancillary_id: &str,
        message: Message,
        work_log: &Arc<RwLock<WorkLog>>,
        event_tx: &broadcast::Sender<super::work_log::WorkEvent>,
    ) {
        match message {
            Message::Assistant(assistant_msg) => {
                // Extract text content from content blocks
                let text: String = assistant_msg
                    .message
                    .content
                    .iter()
                    .filter_map(|block| {
                        if let ContentBlock::Text(text_block) = block {
                            Some(text_block.text.clone())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                if !text.is_empty() {
                    Self::log_op(
                        work_log,
                        event_tx,
                        WorkOp::AssistantMessage { content: text },
                    )
                    .await;
                }

                // Also log tool uses
                for block in &assistant_msg.message.content {
                    if let ContentBlock::ToolUse(tool_use) = block {
                        Self::log_op(
                            work_log,
                            event_tx,
                            WorkOp::ToolCall {
                                id: tool_use.id.clone(),
                                name: tool_use.name.clone(),
                                input: tool_use.input.clone(),
                            },
                        )
                        .await;
                    }
                }
            }
            Message::Result(result_msg) => {
                // Capture session ID for cross-interface handoff
                if !result_msg.session_id.is_empty() {
                    Self::log_op(
                        work_log,
                        event_tx,
                        WorkOp::StatusChange {
                            status: format!("session_id:{}", result_msg.session_id),
                        },
                    )
                    .await;
                }
                info!("{} result: {:?}", ancillary_id, result_msg);
            }
            Message::System(sys_msg) => {
                // Capture session ID early from system messages
                if let Some(ref sid) = sys_msg.session_id {
                    Self::log_op(
                        work_log,
                        event_tx,
                        WorkOp::StatusChange {
                            status: format!("session_id:{}", sid),
                        },
                    )
                    .await;
                }
                info!("{} system message: {}", ancillary_id, sys_msg.subtype);
            }
            _ => {
                // Other message types (StreamEvent, User, etc.)
            }
        }
    }

    async fn log_op(
        work_log: &Arc<RwLock<WorkLog>>,
        event_tx: &broadcast::Sender<super::work_log::WorkEvent>,
        op: WorkOp,
    ) {
        let mut log = work_log.write().await;
        if let Ok(event) = log.append(op) {
            let _ = event_tx.send(event);
        }
    }

    async fn log_status(
        work_log: &Arc<RwLock<WorkLog>>,
        event_tx: &broadcast::Sender<super::work_log::WorkEvent>,
        status: &str,
    ) {
        Self::log_op(
            work_log,
            event_tx,
            WorkOp::StatusChange {
                status: status.to_string(),
            },
        )
        .await;
    }

    /// Get the current work status
    pub async fn status(&self) -> WorkStatus {
        self.status.read().await.clone()
    }

    /// Subscribe to work events (returns receiver and current seq)
    pub fn subscribe(&self) -> (broadcast::Receiver<super::work_log::WorkEvent>, u64) {
        let rx = self.event_tx.subscribe();
        let seq = futures::executor::block_on(async { self.work_log.read().await.current_seq() });
        (rx, seq)
    }

    /// Get a sender for client input
    pub fn input_sender(&self) -> mpsc::Sender<ClientInput> {
        self.input_tx.clone()
    }

    /// Read work log events from a given sequence
    pub async fn read_log_from(&self, from_seq: u64) -> Result<Vec<super::work_log::WorkEvent>> {
        let log = self.work_log.read().await;
        log.read_from(from_seq)
    }

    /// Send input to the ancillary
    pub async fn send_input(&self, input: ClientInput) -> Result<()> {
        self.input_tx
            .send(input)
            .await
            .context("Failed to send input to ancillary")
    }

    /// Interrupt the work
    pub async fn interrupt(&self) -> Result<()> {
        self.send_input(ClientInput::Interrupt).await
    }
}

impl Drop for AncillaryWork {
    fn drop(&mut self) {
        if let Some(handle) = self.task_handle.take() {
            handle.abort();
        }
    }
}
