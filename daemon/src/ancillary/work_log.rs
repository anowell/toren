use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;

/// A single work event in the ancillary's work log
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkEvent {
    /// Monotonic sequence number
    pub seq: u64,
    /// When the event occurred
    pub timestamp: DateTime<Utc>,
    /// The operation that occurred
    pub op: WorkOp,
}

/// Operations that an ancillary performs during work
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkOp {
    // Claude conversation
    AssistantMessage { content: String },
    UserMessage { content: String, client_id: String },
    ThinkingStart,
    ThinkingEnd,

    // Tool execution
    ToolCall {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        id: String,
        output: Value,
        is_error: bool,
    },

    // File operations
    FileRead { path: PathBuf },
    FileWrite { path: PathBuf },

    // Command execution
    CommandStart {
        command: String,
        args: Vec<String>,
    },
    CommandOutput {
        stdout: Option<String>,
        stderr: Option<String>,
    },
    CommandExit { code: i32 },

    // Lifecycle
    AssignmentStarted { bead_id: String },
    AssignmentCompleted,
    AssignmentFailed { error: String },
    StatusChange { status: String },

    // Observability
    ClientConnected { client_id: String },
    ClientDisconnected { client_id: String },
}

/// Persistent work log for an ancillary assignment.
/// Uses a hybrid memory/disk approach:
/// - Recent events kept in memory for fast access
/// - All events appended to disk for durability and replay
pub struct WorkLog {
    /// Recent events in memory for fast access
    hot: VecDeque<WorkEvent>,
    /// Maximum events to keep in hot buffer
    hot_limit: usize,
    /// File handle for append-only persistence
    file: BufWriter<File>,
    /// Path to the log file
    log_path: PathBuf,
    /// Next sequence number
    next_seq: u64,
}

impl WorkLog {
    /// Create or open a work log for the given ancillary and assignment
    pub fn open(ancillary_id: &str, assignment_id: &str) -> Result<Self> {
        let log_dir = dirs::home_dir()
            .context("Could not determine home directory")?
            .join(".toren")
            .join("ancillaries")
            .join(ancillary_id.to_lowercase().replace(' ', "-"))
            .join("work");

        std::fs::create_dir_all(&log_dir)
            .with_context(|| format!("Failed to create log directory: {}", log_dir.display()))?;

        let log_path = log_dir.join(format!("{}.jsonl", assignment_id));

        // Open file for append, create if doesn't exist
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("Failed to open work log: {}", log_path.display()))?;

        // Determine next_seq by counting existing lines
        let next_seq = Self::count_events(&log_path)?;

        // Load recent events into hot buffer
        let hot = Self::load_tail(&log_path, 1000)?;

        Ok(Self {
            hot,
            hot_limit: 1000,
            file: BufWriter::new(file),
            log_path,
            next_seq,
        })
    }

    /// Count events in an existing log file
    fn count_events(path: &PathBuf) -> Result<u64> {
        if !path.exists() {
            return Ok(0);
        }

        let file = File::open(path)?;
        let reader = BufReader::new(file);
        Ok(reader.lines().count() as u64)
    }

    /// Load the last N events from a log file
    fn load_tail(path: &PathBuf, limit: usize) -> Result<VecDeque<WorkEvent>> {
        if !path.exists() {
            return Ok(VecDeque::new());
        }

        let file = File::open(path)?;
        let reader = BufReader::new(file);

        let events: Vec<WorkEvent> = reader
            .lines()
            .filter_map(|line| line.ok())
            .filter_map(|line| serde_json::from_str(&line).ok())
            .collect();

        // Take last `limit` events
        let start = events.len().saturating_sub(limit);
        Ok(events[start..].iter().cloned().collect())
    }

    /// Append a new event to the log
    pub fn append(&mut self, op: WorkOp) -> Result<WorkEvent> {
        let event = WorkEvent {
            seq: self.next_seq,
            timestamp: Utc::now(),
            op,
        };
        self.next_seq += 1;

        // Write to disk
        let json = serde_json::to_string(&event)?;
        writeln!(self.file, "{}", json)?;
        self.file.flush()?;

        // Keep in hot buffer
        self.hot.push_back(event.clone());
        if self.hot.len() > self.hot_limit {
            self.hot.pop_front();
        }

        Ok(event)
    }

    /// Get the current sequence number (next event will have this seq)
    pub fn current_seq(&self) -> u64 {
        self.next_seq
    }

    /// Read events starting from a given sequence number.
    /// Returns events from `from_seq` up to current.
    pub fn read_from(&self, from_seq: u64) -> Result<Vec<WorkEvent>> {
        // Check if we can serve from hot buffer
        if let Some(first_hot) = self.hot.front() {
            if from_seq >= first_hot.seq {
                // All requested events are in hot buffer
                return Ok(self
                    .hot
                    .iter()
                    .filter(|e| e.seq >= from_seq)
                    .cloned()
                    .collect());
            }
        }

        // Need to read from disk
        let file = File::open(&self.log_path)?;
        let reader = BufReader::new(file);

        let events: Vec<WorkEvent> = reader
            .lines()
            .filter_map(|line| line.ok())
            .filter_map(|line| serde_json::from_str(&line).ok())
            .filter(|e: &WorkEvent| e.seq >= from_seq)
            .collect();

        Ok(events)
    }

    /// Get the path to the log file
    pub fn path(&self) -> &PathBuf {
        &self.log_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_work_log_append_and_read() {
        // Use a temp directory for testing
        let temp_dir = TempDir::new().unwrap();
        std::env::set_var("HOME", temp_dir.path());

        let mut log = WorkLog::open("Test One", "test-assignment").unwrap();

        // Append some events
        log.append(WorkOp::AssignmentStarted {
            bead_id: "breq-test".to_string(),
        })
        .unwrap();

        log.append(WorkOp::AssistantMessage {
            content: "Hello!".to_string(),
        })
        .unwrap();

        log.append(WorkOp::ToolCall {
            id: "tool-1".to_string(),
            name: "read_file".to_string(),
            input: serde_json::json!({"path": "/tmp/test"}),
        })
        .unwrap();

        // Read back
        let events = log.read_from(0).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].seq, 0);
        assert_eq!(events[1].seq, 1);
        assert_eq!(events[2].seq, 2);

        // Read from middle
        let events = log.read_from(1).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, 1);
    }
}
