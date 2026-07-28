//! The debug log both front-ends write to.
//!
//! `~/.toren/logs/<component>.jsonl` is a development record, not a data structure: what breq or
//! the daemon did, with enough context to work out why something failed afterwards. It replaces
//! `completion_history.jsonl`, which pretended to be a structured record nothing ever read.
//!
//! Ordinary `info!` / `warn!` calls land here — there is no hand-rolled writer to keep in sync
//! with the call sites. The file rolls daily and keeps [`MAX_LOG_FILES`] days, because the file
//! it replaces grew forever and that was the bug.

use std::path::PathBuf;

use tracing::Subscriber;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

/// Days of history kept. A debug log answers "what happened just now"; anything older is the
/// agent's own record, the VCS, or gone.
const MAX_LOG_FILES: usize = 7;

/// Where the rolled files live.
pub fn log_dir() -> PathBuf {
    crate::config::toren_root().join("logs")
}

/// A JSON layer over the rolling file, to be composed with whatever a binary prints to a terminal.
///
/// `None` when the log cannot be opened: a debug log is never worth failing a command over, and
/// the terminal layer still works. Writes are synchronous so nothing is lost when breq `exec`s or
/// exits without unwinding.
pub fn file_layer<S>(component: &str) -> Option<Box<dyn Layer<S> + Send + Sync + 'static>>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    let dir = log_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("warning: no log at {}: {}", dir.display(), e);
        return None;
    }

    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(component)
        .filename_suffix("jsonl")
        .max_log_files(MAX_LOG_FILES)
        .build(&dir);

    match appender {
        Ok(appender) => Some(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(appender)
                .with_filter(LevelFilter::INFO)
                .boxed(),
        ),
        Err(e) => {
            eprintln!("warning: no log at {}: {}", dir.display(), e);
            None
        }
    }
}
