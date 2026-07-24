//! Append-only record of workspaces that have been torn down.
//!
//! `~/.toren/completion_history.jsonl` outlives the workspace it describes, which is the point:
//! once teardown deletes `<ws>/.toren/`, this and the transcripts are all that remain of an
//! incarnation. Stamped with the uid so a reused slot name never conflates two of them.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;

/// One torn-down workspace incarnation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeardownRecord {
    /// Instance uid, if the workspace was decorated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    pub workspace: String,
    pub segment: String,
    /// Task links at teardown time, as `source:id`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<String>,
    /// Final revision, captured before the working copy went away.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// RFC 3339.
    pub torn_down_at: String,
}

pub fn history_path() -> PathBuf {
    crate::config::toren_root().join("completion_history.jsonl")
}

/// Append a record. Best-effort by nature — a failure here must not fail a teardown.
pub fn record_teardown(record: &TeardownRecord) -> Result<()> {
    let path = history_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut line = serde_json::to_string(record)?;
    line.push('\n');

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("Failed to open {}", path.display()))?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_serialize_compactly() {
        let record = TeardownRecord {
            uid: Some("k3m9xz".into()),
            workspace: "one".into(),
            segment: "toren".into(),
            tasks: vec!["runes:tor-1".into()],
            revision: None,
            torn_down_at: "2026-07-24T00:00:00Z".into(),
        };
        let line = serde_json::to_string(&record).unwrap();
        assert!(line.contains(r#""uid":"k3m9xz""#));
        assert!(
            !line.contains("revision"),
            "absent fields stay out: {}",
            line
        );
    }
}
