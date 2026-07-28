pub mod agents;
pub mod alias;
pub mod config;
pub mod doctor;
pub mod fsutil;
pub mod logging;
pub mod naming;
pub mod place;
pub mod plugins;
pub mod process;
pub mod rmux;
pub mod scripts;
pub mod segments;
pub mod sessions;
pub mod sets;
pub mod state;
pub mod tasks;
pub mod teardown;
pub mod workspace;
pub mod workspace_setup;

pub use agents::AgentSpec;
pub use config::{
    default_config_path, expand_path, expand_path_str, tilde_shorten, toren_root,
    AncillariesConfig, Config, DeliveryConfig, TasksConfig,
};
pub use naming::{
    ancillary_id, ancillary_number, ancillary_segment, number_to_word, word_to_number,
};
pub use place::{Place, PlaceRegistry};
pub use plugins::{Family, PluginContext, PluginManager, PluginMeta};
pub use process::{ProcessInfo, WorkspaceProcessesRunning};
pub use segments::{Segment, SegmentManager};
pub use sets::{CollectOptions, PrInfo, SessionInfo, Sets, TaskView};
pub use state::{
    AgentSession, AgentState, BaseRevision, Cache, CacheEntry, DeliveryState, TaskLink,
    WorkspaceState,
};
pub use tasks::{format_link, infer_task_fields, split_link, InferredTaskFields, ResolvedTask};
pub use teardown::{teardown, TeardownOptions, TeardownOutcome};
pub use workspace::{
    detect_repo_type, CleanupMode, CommitInfo, GitWorktreeBackend, JjBackend, RepoType, VcsBackend,
    WorkspaceManager, WorkspaceOrigin,
};
pub use workspace_setup::{
    render_template, BreqConfig, ParentInfo, RepoInfo, SetupResult, TaskInfo, WorkspaceContext,
    WorkspaceInfo, WorkspaceSetup,
};
