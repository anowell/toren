pub mod agent;
pub mod alias;
pub mod assignment;
pub mod assignment_ops;
pub mod composite_status;
pub mod config;
pub mod plugins;
pub mod process;
pub mod rmux;
pub mod segments;
pub mod tasks;
pub mod workspace;
pub mod workspace_setup;

pub use agent::{Agent, AgentKind};
pub use assignment::{
    ancillary_id, ancillary_number, ancillary_segment, number_to_word, word_to_number, Assignment,
    AssignmentManager, AssignmentRef, AssignmentSource, AssignmentStatus, CompletionReason,
    CompletionRecord,
};
pub use assignment_ops::{
    abort_assignment, clean_assignment, complete_assignment, prepare_resume,
    render_auto_commit_message, AbortOptions, CleanOptions, CleanResult, CompleteOptions,
    CompleteResult, ResumeOptions, ResumeResult, DEFAULT_AUTO_COMMIT_MESSAGE,
};
pub use composite_status::CompositeStatus;
pub use config::{
    expand_path, expand_path_str, tilde_shorten, toren_root, AncillariesConfig, Config,
    IntentsConfig, TasksConfig,
};
pub use plugins::{DeferredAction, PluginContext, PluginManager, PluginMeta, PluginResult};
pub use process::{ProcessInfo, WorkspaceProcessesRunning};
pub use segments::{Segment, SegmentManager};
pub use tasks::{generate_prompt, infer_task_fields, InferredTaskFields, ResolvedTask};
pub use workspace::{
    detect_repo_type, CleanupMode, CommitInfo, GitWorktreeBackend, JjBackend, RepoType, VcsBackend,
    WorkspaceManager,
};
pub use workspace_setup::{
    render_template, BreqConfig, RepoInfo, SetupResult, TaskInfo, WorkspaceContext, WorkspaceInfo,
    WorkspaceSetup,
};
