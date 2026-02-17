pub mod assignment;
pub mod assignment_ops;
pub mod config;
pub mod segments;
pub mod tasks;
pub mod workspace;
pub mod workspace_setup;

pub use assignment::{
    ancillary_id, ancillary_number, ancillary_segment, number_to_word, word_to_number, Assignment,
    AssignmentManager, AssignmentRef, AssignmentSource, AssignmentStatus,
};
pub use assignment_ops::{
    abort_assignment, complete_assignment, prepare_resume, AbortOptions, CompleteOptions,
    CompleteResult, ResumeOptions, ResumeResult,
};
pub use config::{Config, IntentsConfig};
pub use segments::{Segment, SegmentManager};
pub use tasks::{fetch_task, generate_prompt, Task, TaskProvider};
pub use workspace::WorkspaceManager;
pub use workspace_setup::{
    render_template, BreqConfig, TaskInfo, WorkspaceContext, WorkspaceInfo, WorkspaceSetup,
    RepoInfo,
};
