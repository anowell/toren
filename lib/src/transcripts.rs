//! Where raw pane output is recorded.
//!
//! `~/.toren/transcripts/<segment>/<workspace>/<uid>/<window>.raw` — raw bytes, escape
//! sequences and all, appended from the moment a pane starts being mirrored.
//!
//! Treated purely as logs: never archived at teardown (teardown just deletes the workspace),
//! pruned by age. Keyed by instance uid, so a workspace slot reused three times keeps three
//! separate records instead of one confusing interleaved file — this is the durable answer to
//! "what did workspace two do last month", long after `<ws>/.toren/` is gone.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// `~/.toren/transcripts`
pub fn root() -> PathBuf {
    crate::config::toren_root().join("transcripts")
}

/// The directory holding one incarnation's transcripts.
pub fn dir(segment: &str, workspace: &str, uid: Option<&str>) -> PathBuf {
    root()
        .join(sanitize(segment))
        .join(sanitize(workspace))
        .join(sanitize(uid.unwrap_or("nouid")))
}

/// The transcript file for one window of one incarnation.
pub fn path(segment: &str, workspace: &str, uid: Option<&str>, window: &str) -> PathBuf {
    dir(segment, workspace, uid).join(format!("{}.raw", sanitize(window)))
}

/// Create the directory a transcript lives in.
pub fn ensure_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    Ok(())
}

/// Delete transcript directories untouched for longer than `max_age_days`.
///
/// Returns the directories removed. Age comes from the newest file inside, so an incarnation
/// still being written to is never pruned out from under itself.
pub fn prune(max_age_days: u64) -> Result<Vec<PathBuf>> {
    let root = root();
    if !root.exists() {
        return Ok(Vec::new());
    }

    let max_age = std::time::Duration::from_secs(max_age_days * 86_400);
    let mut removed = Vec::new();

    // root/<segment>/<workspace>/<uid>
    for segment in read_dirs(&root) {
        for workspace in read_dirs(&segment) {
            for incarnation in read_dirs(&workspace) {
                let Some(age) = newest_age(&incarnation) else {
                    continue;
                };
                if age > max_age {
                    std::fs::remove_dir_all(&incarnation)
                        .with_context(|| format!("Failed to remove {}", incarnation.display()))?;
                    removed.push(incarnation);
                }
            }
        }
    }

    Ok(removed)
}

fn read_dirs(path: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}

/// How long since anything in `dir` was written.
fn newest_age(dir: &Path) -> Option<std::time::Duration> {
    let entries = std::fs::read_dir(dir).ok()?;
    entries
        .flatten()
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .filter_map(|t| t.elapsed().ok())
        .min()
}

/// Reduce a path component to something safe to nest under the transcript root.
fn sanitize(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "x".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcripts_are_keyed_by_incarnation() {
        let a = path("toren", "one", Some("aaa111"), "agent");
        let b = path("toren", "one", Some("bbb222"), "agent");
        assert_ne!(a, b, "a reused slot must not share a transcript");
        assert!(a.ends_with("toren/one/aaa111/agent.raw"));
    }

    #[test]
    fn path_components_cannot_escape_the_root() {
        let p = path("../../etc", "one", Some("x"), "agent");
        assert!(p.starts_with(root()));
        assert!(!p.display().to_string().contains(".."));
    }

    #[test]
    fn prune_leaves_fresh_incarnations_alone() {
        // Directly exercises the age rule; the root is process-global so we test the helper.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("agent.raw"), b"hello").unwrap();
        let age = newest_age(dir.path()).unwrap();
        assert!(age < std::time::Duration::from_secs(60));
    }
}
