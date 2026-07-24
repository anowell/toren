pub mod agents;
pub mod alias;
pub mod annotations;
pub mod config;
pub mod doctor;
pub mod fsutil;
pub mod history;
pub mod naming;
pub mod place;
pub mod plugins;
pub mod process;
pub mod rmux;
pub mod scripts;
pub mod segments;
pub mod sets;
pub mod tasks;
pub mod teardown;
pub mod transcripts;
pub mod workspace;
pub mod workspace_setup;

pub use agents::AgentSpec;
pub use annotations::{Annotations, Cache, CacheEntry};
pub use config::{
    expand_path, expand_path_str, tilde_shorten, toren_root, AncillariesConfig, Config,
    DeliveryConfig, TasksConfig,
};
pub use history::{record_teardown, TeardownRecord};
pub use naming::{
    ancillary_id, ancillary_number, ancillary_segment, number_to_word, word_to_number,
};
pub use place::{Place, PlaceRegistry};
pub use plugins::{Family, PluginContext, PluginManager, PluginMeta};
pub use process::{ProcessInfo, WorkspaceProcessesRunning};
pub use segments::{Segment, SegmentManager};
pub use sets::{CollectOptions, PrInfo, SessionInfo, Sets, TaskView};
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
