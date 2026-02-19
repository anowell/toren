//! Composite status signals for assignments.
//!
//! The real status is derived from four observable signals:
//! 1. Agent activity (busy/idle) — from Claude session log last-entry-type
//! 2. Bead assignee — from bd
//! 3. Has changes — from jj workspace
//! 4. Bead status — from bd

use serde::{Deserialize, Serialize};
use std::io::{Read as _, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Composite status signals for an assignment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompositeStatus {
    /// Agent activity: "busy" or "idle"
    pub agent_activity: String,
    /// Whether the workspace has changes since trunk (committed or uncommitted)
    pub has_changes: bool,
    /// Bead status: "open", "in_progress", "closed"
    pub bead_status: String,
    /// Bead assignee: e.g. "claude", "anthony", ""
    pub bead_assignee: String,
}

/// Check if a jj workspace has changes exclusive to it.
///
/// Two complementary checks:
/// 1. `jj log -r "::@ ~ ::default@ ~ empty()"` — finds non-empty commits exclusive
///    to this workspace (ancestors of @ not in default workspace). Catches committed
///    work after `jj commit` + `jj new` where @ is empty but commits below have content.
/// 2. `jj diff --stat` — detects uncommitted working-copy changes on @.
///
/// Both checks are needed: the revset misses working-copy changes when default@ is
/// a descendant of @ (common topology), and `jj diff` misses committed-then-new'd work.
pub fn workspace_has_changes(workspace_path: &Path) -> bool {
    if !workspace_path.exists() {
        return false;
    }

    // Check 1: non-empty commits exclusive to this workspace
    let log_output = std::process::Command::new("jj")
        .args([
            "log",
            "-r",
            "::@ ~ ::default@ ~ empty()",
            "--no-graph",
            "-T",
            r#"change_id ++ "\n""#,
        ])
        .current_dir(workspace_path)
        .output()
        .ok();

    if let Some(output) = log_output {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if !stdout.trim().is_empty() {
                return true;
            }
        }
        // Revset succeeded but found nothing, or failed (no default workspace).
        // Either way, fall through to check working copy.
    }

    // Check 2: uncommitted working-copy changes
    let diff_output = std::process::Command::new("jj")
        .args(["diff", "--stat"])
        .current_dir(workspace_path)
        .output()
        .ok();

    if let Some(output) = diff_output {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if !stdout.trim().is_empty() {
                return true;
            }
        }
    }

    false
}

/// Detect agent activity by checking the last entry type in Claude Code session logs.
///
/// Claude Code stores per-directory session logs at:
///   `~/.claude/projects/{dir_name}/{session_id}.jsonl`
/// where `dir_name` is the workspace path with `/` and `.` replaced by `-`.
///
/// Rather than just checking file modification recency (which misses long-running
/// tool executions like `sleep 90`), we read the last JSONL entry and check if it
/// indicates a mid-turn state. Mid-turn entry types include:
/// - `assistant` with `tool_use` — a tool is executing
/// - `user` with `tool_result` — Claude is processing tool output
/// - `user` with `user_message` — Claude is processing user input
/// - `assistant` with subtype containing `thinking` — extended thinking
/// - `progress` — streaming in progress
///
/// If the last entry is mid-turn AND the file was modified within 5 minutes,
/// the agent is busy. The 5-minute threshold catches stale sessions (crashes, etc.).
pub fn detect_agent_activity(workspace_path: &Path) -> String {
    if let Some(project_dir) = claude_project_dir(workspace_path) {
        if session_is_mid_turn(&project_dir) {
            return "busy".to_string();
        }
    }

    "idle".to_string()
}

/// Compute the Claude Code project directory for a workspace path.
///
/// Claude Code uses `~/.claude/projects/{dir_name}/` where `dir_name`
/// is the absolute workspace path with `/` and `.` replaced by `-`.
fn claude_project_dir(workspace_path: &Path) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let dir_name = workspace_path
        .to_str()?
        .replace(['/', '.'], "-");
    let project_dir = home.join(".claude").join("projects").join(&dir_name);
    if project_dir.is_dir() {
        Some(project_dir)
    } else {
        None
    }
}

/// Check if the most recent session log indicates a mid-turn state.
///
/// Finds the most recently modified `.jsonl`, reads its last line, and checks
/// whether the entry type indicates Claude is mid-turn (busy). Also requires
/// the file to have been modified within 5 minutes to avoid stale sessions.
fn session_is_mid_turn(project_dir: &Path) -> bool {
    let entries = match std::fs::read_dir(project_dir) {
        Ok(e) => e,
        Err(_) => return false,
    };

    // Find the most recently modified .jsonl file
    let mut most_recent: Option<(PathBuf, std::time::SystemTime)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            if let Ok(meta) = path.metadata() {
                if let Ok(modified) = meta.modified() {
                    most_recent = Some(match most_recent {
                        Some((_, prev_time)) if modified > prev_time => (path, modified),
                        Some(prev) => prev,
                        None => (path, modified),
                    });
                }
            }
        }
    }

    let (path, modified) = match most_recent {
        Some(v) => v,
        None => return false,
    };

    // Check staleness: file must have been modified within 5 minutes
    let age_secs = modified.elapsed().unwrap_or_default().as_secs();
    if age_secs > 300 {
        return false;
    }

    // Read the last line efficiently by seeking from the end
    let last_line = match read_last_line(&path) {
        Some(line) => line,
        None => return false,
    };

    is_mid_turn_entry(&last_line)
}

/// Read the last non-empty line of a file by seeking from the end.
fn read_last_line(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let file_len = file.metadata().ok()?.len();
    if file_len == 0 {
        return None;
    }

    // Read up to 256KB from the end — session log lines can be large due to
    // tool results (file contents, large outputs).
    let read_size = file_len.min(262144);
    file.seek(SeekFrom::End(-(read_size as i64))).ok()?;

    let mut buf = vec![0u8; read_size as usize];
    file.read_exact(&mut buf).ok()?;

    // Find the last non-empty line
    let text = String::from_utf8_lossy(&buf);
    text.lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(|s| s.to_string())
}

/// Determine if a JSONL entry indicates a mid-turn state.
///
/// Mid-turn entry types (Claude is busy):
/// - `assistant` role with `tool_use` content → tool is executing
/// - `user` role with `tool_result` content → Claude processing tool output
/// - `user` role with type `user_message` → Claude processing user input
/// - role containing `thinking` → extended thinking
/// - type is `progress` → streaming in progress
///
/// Idle entry types (turn is complete):
/// - `assistant` role with `text` content → response delivered
/// - type is `system` → session ended or system event
/// - type is `summary` → conversation summary
fn is_mid_turn_entry(line: &str) -> bool {
    // Parse the JSON line — session log entries have a `type` field and
    // sometimes a `message` field with `role` and `content`.
    let value: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return false,
    };

    let entry_type = value.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match entry_type {
        // assistant message — check content blocks for tool_use vs text
        "assistant" => {
            if let Some(message) = value.get("message") {
                if let Some(content) = message.get("content").and_then(|c| c.as_array()) {
                    // If any content block is tool_use, it's mid-turn
                    return content.iter().any(|block| {
                        block.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                    });
                }
                // If there's a stop_reason of "tool_use", it's mid-turn
                if message.get("stop_reason").and_then(|s| s.as_str()) == Some("tool_use") {
                    return true;
                }
            }
            // assistant message with text content or no content → idle
            false
        }
        // user message with tool_result → Claude processing tool output
        "user" => {
            if let Some(message) = value.get("message") {
                if let Some(content) = message.get("content").and_then(|c| c.as_array()) {
                    return content.iter().any(|block| {
                        block.get("type").and_then(|t| t.as_str()) == Some("tool_result")
                    });
                }
                // user text message → Claude about to process
                return true;
            }
            false
        }
        // Progress and thinking entries → mid-turn
        "progress" => true,
        t if t.contains("thinking") => true,
        // System, summary, result → idle
        _ => false,
    }
}
