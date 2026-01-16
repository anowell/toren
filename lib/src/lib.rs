pub mod config;
pub mod segments;
pub mod tasks;
pub mod workspace;

pub use config::Config;
pub use segments::{Segment, SegmentManager, SegmentSource};
pub use tasks::{fetch_task, generate_prompt, Task, TaskProvider};
pub use workspace::WorkspaceManager;
