use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::SystemTime;
use tracing::{debug, info};

/// How the assignment was created
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum AssignmentSource {
    /// Created from an external reference (e.g., bead)
    #[serde(alias = "Bead")]
    Reference,
    /// Created from a prompt (task may have been auto-created)
    Prompt { original_prompt: String },
}

/// Current status of an assignment.
/// Assignments are always Active — terminal actions (complete/abort) dissolve the link
/// and record a CompletionRecord for history.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum AssignmentStatus {
    #[default]
    Active,
}

// Custom serde: always serializes as "active", deserializes any legacy variant as Active
impl Serialize for AssignmentStatus {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("active")
    }
}

impl<'de> Deserialize<'de> for AssignmentStatus {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        // Accept any legacy status and map to Active
        match s.as_str() {
            "pending" | "active" | "completed" | "aborted" => Ok(AssignmentStatus::Active),
            _ => Ok(AssignmentStatus::Active),
        }
    }
}

/// Record of a completed/aborted assignment for history
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRecord {
    /// Original assignment ID
    pub assignment_id: String,
    /// Ancillary that worked on it
    pub ancillary_id: String,
    /// Task identifier (e.g., bead ID)
    #[serde(alias = "external_id", alias = "bead_id", default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Segment name
    pub segment: String,
    /// When the assignment was completed/aborted (RFC 3339)
    pub completed_at: String,
    /// How it ended
    pub reason: CompletionReason,
    /// Final jj revision hash (if available)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_revision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionReason {
    Completed,
    Aborted,
}

/// An assignment links an ancillary to a workspace.
/// This is the central work unit shared between CLI (breq) and daemon (toren).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assignment {
    /// Unique identifier for this assignment
    pub id: String,
    /// Ancillary identifier (e.g., "Toren One")
    pub ancillary_id: String,
    /// Task identifier (e.g., bead ID "breq-a1b2") — optional
    #[serde(alias = "external_id", alias = "bead_id", default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Segment name (e.g., "toren")
    pub segment: String,
    /// Absolute path to the workspace
    pub workspace_path: PathBuf,
    /// How this assignment was created
    pub source: AssignmentSource,
    /// Current status
    pub status: AssignmentStatus,
    /// When the assignment was created (RFC 3339)
    pub created_at: String,
    /// When the assignment was last updated (RFC 3339)
    pub updated_at: String,
    /// Task title for display purposes
    #[serde(default, alias = "title", alias = "bead_title", skip_serializing_if = "Option::is_none")]
    pub task_title: Option<String>,
    /// Task URL (e.g., link to issue tracker)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_url: Option<String>,
    /// Task source (e.g., "beads", "github")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_source: Option<String>,
    /// Claude session ID for cross-interface handoff (breq <-> toren)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Numeric ancillary number, derived from ancillary_id (e.g., "Toren One" -> 1)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ancillary_num: Option<u32>,
    /// Base branch at the time of assignment (for git worktrees).
    /// Used as the comparison reference for has_changes and workspace_info.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
}

/// Max number that gets a word name (1-99 use English words, 100+ use digits)
const MAX_WORD_NUMBER: u32 = 99;

/// Convert a number to its word form (1 -> "One", 21 -> "Twenty-One", etc.)
pub fn number_to_word(n: u32) -> String {
    if n == 0 {
        return "Zero".to_string();
    }
    if n <= MAX_WORD_NUMBER {
        english_numbers::convert(n as i64, english_numbers::Formatting::all())
    } else {
        n.to_string()
    }
}

/// Lazily-built reverse map from lowercase word form to number
fn word_to_number_map() -> &'static HashMap<String, u32> {
    static MAP: OnceLock<HashMap<String, u32>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m = HashMap::new();
        m.insert("zero".to_string(), 0);
        for n in 1..=MAX_WORD_NUMBER {
            let word = english_numbers::convert(n as i64, english_numbers::Formatting::all());
            m.insert(word.to_lowercase(), n);
        }
        m
    })
}

/// Convert a word to its number form ("One" -> 1, "Twenty-One" -> 21, etc.)
pub fn word_to_number(word: &str) -> Option<u32> {
    // Try the word map first (handles "One", "Twenty-One", etc.)
    if let Some(&n) = word_to_number_map().get(&word.to_lowercase()) {
        return Some(n);
    }
    // Fall back to plain number parsing (handles "100", "101", etc.)
    word.parse::<u32>().ok()
}

/// Generate an ancillary ID from segment name and number
pub fn ancillary_id(segment: &str, number: u32) -> String {
    let segment_cap = capitalize(segment);
    format!("{} {}", segment_cap, number_to_word(number))
}

/// Extract the number from an ancillary ID
pub fn ancillary_number(ancillary_id: &str) -> Option<u32> {
    ancillary_id
        .split_whitespace()
        .last()
        .and_then(word_to_number)
}

/// Extract the segment from an ancillary ID (lowercased)
pub fn ancillary_segment(ancillary_id: &str) -> Option<String> {
    ancillary_id
        .split_whitespace()
        .next()
        .map(|s| s.to_lowercase())
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().chain(chars).collect(),
    }
}

/// Reference type for command disambiguation
#[derive(Debug, Clone, PartialEq)]
pub enum AssignmentRef {
    /// Reference by task ID (e.g., bead ID "breq-a1b2")
    TaskId(String),
    /// Reference by ancillary ID (e.g., "Toren One" or just "One")
    Ancillary(String),
}

impl AssignmentRef {
    /// Parse a reference string into an AssignmentRef.
    ///
    /// Rules:
    /// - Contains hyphen -> treat as task ID
    /// - Contains space -> treat as ancillary name
    /// - Otherwise -> try ancillary name first, then task ID
    pub fn parse(s: &str, segment: &str) -> Self {
        if s.contains('-') {
            AssignmentRef::TaskId(s.to_string())
        } else if s.contains(' ') {
            AssignmentRef::Ancillary(s.to_string())
        } else {
            // Try to interpret as ancillary number word
            if word_to_number(s).is_some() {
                let full_id = format!("{} {}", capitalize(segment), capitalize(s));
                AssignmentRef::Ancillary(full_id)
            } else {
                AssignmentRef::TaskId(s.to_string())
            }
        }
    }
}

/// Manages assignments between ancillaries.
/// Persistent storage in ~/.toren/assignments.json.
/// Used by both CLI (breq) and daemon (toren).
///
/// Automatically reloads from disk when the file has been modified externally
/// (e.g., by breq while the daemon is running).
pub struct AssignmentManager {
    /// Path to the assignments.json file
    storage_path: PathBuf,
    /// Assignments keyed by assignment ID
    assignments: HashMap<String, Assignment>,
    /// Last known modification time of the assignments file
    last_mtime: Option<SystemTime>,
}

impl AssignmentManager {
    /// Create a new AssignmentManager with persistent storage in ~/.toren/
    pub fn new() -> Result<Self> {
        let storage_path = dirs::home_dir()
            .context("Could not determine home directory")?
            .join(".toren")
            .join("assignments.json");

        let mut mgr = Self {
            storage_path,
            assignments: HashMap::new(),
            last_mtime: None,
        };
        mgr.load()?;
        Ok(mgr)
    }

    /// Load assignments from disk
    fn load(&mut self) -> Result<()> {
        if !self.storage_path.exists() {
            debug!(
                "No existing assignments file at {}",
                self.storage_path.display()
            );
            self.last_mtime = None;
            return Ok(());
        }

        let metadata = std::fs::metadata(&self.storage_path)
            .with_context(|| format!("Failed to stat {}", self.storage_path.display()))?;
        let mtime = metadata.modified().ok();

        let content = std::fs::read_to_string(&self.storage_path)
            .with_context(|| format!("Failed to read {}", self.storage_path.display()))?;

        let assignments: Vec<Assignment> =
            serde_json::from_str(&content).with_context(|| "Failed to parse assignments.json")?;

        self.assignments.clear();
        for mut a in assignments {
            // Backfill ancillary_num for assignments created before this field existed
            if a.ancillary_num.is_none() {
                a.ancillary_num = ancillary_number(&a.ancillary_id);
            }
            self.assignments.insert(a.id.clone(), a);
        }
        self.last_mtime = mtime;

        debug!("Loaded {} assignments from disk", self.assignments.len());
        Ok(())
    }

    /// Reload from disk if the file has been modified externally.
    /// Called automatically before read operations to stay in sync
    /// when another process (e.g., breq) modifies assignments.json.
    fn reload_if_changed(&mut self) {
        let current_mtime = self
            .storage_path
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok());

        if current_mtime != self.last_mtime {
            debug!("assignments.json changed on disk, reloading");
            if let Err(e) = self.load() {
                tracing::warn!("Failed to reload assignments from disk: {}", e);
            }
        }
    }

    /// Save assignments to disk
    pub fn save(&mut self) -> Result<()> {
        let assignments: Vec<&Assignment> = self.assignments.values().collect();
        let content = serde_json::to_string_pretty(&assignments)
            .with_context(|| "Failed to serialize assignments")?;

        // Ensure parent directory exists
        if let Some(parent) = self.storage_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::write(&self.storage_path, content)
            .with_context(|| format!("Failed to write {}", self.storage_path.display()))?;

        // Update tracked mtime so we don't reload our own writes
        self.last_mtime = self
            .storage_path
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok());

        debug!("Saved {} assignments to disk", self.assignments.len());
        Ok(())
    }

    /// Create a new assignment.
    ///
    /// `task_id` is an optional task reference (e.g., bead ID).
    /// `source` indicates how the assignment was created.
    pub fn create(
        &mut self,
        ancillary_id: &str,
        task_id: Option<&str>,
        source: AssignmentSource,
        segment: &str,
        workspace_path: PathBuf,
        task_title: Option<String>,
        base_branch: Option<String>,
        task_url: Option<&str>,
        task_source: Option<&str>,
    ) -> Result<Assignment> {
        let now = chrono::Utc::now().to_rfc3339();
        let id = uuid::Uuid::new_v4().to_string();

        let assignment = Assignment {
            ancillary_num: ancillary_number(ancillary_id),
            id,
            ancillary_id: ancillary_id.to_string(),
            task_id: task_id.map(|s| s.to_string()),
            segment: segment.to_string(),
            workspace_path,
            source,
            status: AssignmentStatus::Active,
            created_at: now.clone(),
            updated_at: now,
            task_title,
            task_url: task_url.map(|s| s.to_string()),
            task_source: task_source.map(|s| s.to_string()),
            session_id: None,
            base_branch,
        };

        self.assignments
            .insert(assignment.id.clone(), assignment.clone());
        self.save()?;

        info!(
            "Created assignment: {} -> {:?}",
            ancillary_id, assignment.task_id
        );
        Ok(assignment)
    }

    /// Create a new assignment from an existing bead (backward-compat wrapper).
    pub fn create_from_bead(
        &mut self,
        ancillary_id: &str,
        bead_id: &str,
        segment: &str,
        workspace_path: PathBuf,
        bead_title: Option<String>,
        base_branch: Option<String>,
    ) -> Result<Assignment> {
        self.create(
            ancillary_id,
            Some(bead_id),
            AssignmentSource::Reference,
            segment,
            workspace_path,
            bead_title,
            base_branch,
            None,
            Some("beads"),
        )
    }

    /// Create a new assignment from a prompt (backward-compat wrapper).
    pub fn create_from_prompt(
        &mut self,
        ancillary_id: &str,
        bead_id: &str,
        original_prompt: &str,
        segment: &str,
        workspace_path: PathBuf,
        bead_title: Option<String>,
        base_branch: Option<String>,
    ) -> Result<Assignment> {
        self.create(
            ancillary_id,
            Some(bead_id),
            AssignmentSource::Prompt {
                original_prompt: original_prompt.to_string(),
            },
            segment,
            workspace_path,
            bead_title,
            base_branch,
            None,
            Some("beads"),
        )
    }

    /// Update assignment session ID (for cross-interface handoff)
    pub fn update_session_id(
        &mut self,
        assignment_id: &str,
        session_id: Option<String>,
    ) -> Result<bool> {
        if let Some(assignment) = self.assignments.get_mut(assignment_id) {
            assignment.session_id = session_id;
            assignment.updated_at = chrono::Utc::now().to_rfc3339();
            self.save()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Touch the updated_at timestamp for an assignment
    pub fn touch(&mut self, assignment_id: &str) -> Result<bool> {
        if let Some(assignment) = self.assignments.get_mut(assignment_id) {
            assignment.updated_at = chrono::Utc::now().to_rfc3339();
            self.save()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Record a completion (or abort) and remove the assignment from active storage.
    /// Appends a CompletionRecord to ~/.toren/completion_history.jsonl.
    pub fn record_completion(
        &mut self,
        assignment: &Assignment,
        reason: CompletionReason,
        final_revision: Option<String>,
    ) -> Result<()> {
        let record = CompletionRecord {
            assignment_id: assignment.id.clone(),
            ancillary_id: assignment.ancillary_id.clone(),
            task_id: assignment.task_id.clone(),
            segment: assignment.segment.clone(),
            completed_at: chrono::Utc::now().to_rfc3339(),
            reason,
            final_revision,
        };

        // Append to completion history file
        let history_path = self
            .storage_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("completion_history.jsonl");

        let mut line = serde_json::to_string(&record)
            .with_context(|| "Failed to serialize completion record")?;
        line.push('\n');

        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&history_path)
            .with_context(|| format!("Failed to open {}", history_path.display()))?;
        file.write_all(line.as_bytes())?;

        debug!(
            "Recorded completion for assignment {} ({})",
            assignment.id,
            match record.reason {
                CompletionReason::Completed => "completed",
                CompletionReason::Aborted => "aborted",
            }
        );

        Ok(())
    }

    /// Get assignment by ID
    pub fn get(&mut self, assignment_id: &str) -> Option<&Assignment> {
        self.reload_if_changed();
        self.assignments.get(assignment_id)
    }

    /// Get all assignments for a task ID
    pub fn get_by_task_id(&mut self, task_id: &str) -> Vec<&Assignment> {
        self.reload_if_changed();
        self.assignments
            .values()
            .filter(|a| a.task_id.as_deref() == Some(task_id))
            .collect()
    }

    /// Get all assignments for an ancillary
    pub fn get_by_ancillary(&mut self, ancillary_id: &str) -> Vec<&Assignment> {
        self.reload_if_changed();
        self.assignments
            .values()
            .filter(|a| a.ancillary_id.to_lowercase() == ancillary_id.to_lowercase())
            .collect()
    }

    /// Get active assignment for an ancillary (should be at most one).
    /// All assignments are active (terminal actions remove the record).
    pub fn get_active_for_ancillary(&mut self, ancillary_id: &str) -> Option<&Assignment> {
        self.reload_if_changed();
        self.assignments
            .values()
            .find(|a| a.ancillary_id.to_lowercase() == ancillary_id.to_lowercase())
    }

    /// Remove assignment by ID
    pub fn remove(&mut self, assignment_id: &str) -> Result<Option<Assignment>> {
        let removed = self.assignments.remove(assignment_id);
        if removed.is_some() {
            self.save()?;
        }
        Ok(removed)
    }

    /// Remove all assignments for an ancillary
    pub fn dismiss_ancillary(&mut self, ancillary_id: &str) -> Result<Vec<Assignment>> {
        let ids: Vec<_> = self
            .assignments
            .values()
            .filter(|a| a.ancillary_id.to_lowercase() == ancillary_id.to_lowercase())
            .map(|a| a.id.clone())
            .collect();

        let removed: Vec<Assignment> = ids
            .iter()
            .filter_map(|id| self.assignments.remove(id))
            .collect();

        if !removed.is_empty() {
            self.save()?;
            info!(
                "Dismissed {} assignment(s) for ancillary {}",
                removed.len(),
                ancillary_id
            );
        }

        Ok(removed)
    }

    /// Remove all assignments for a task ID
    pub fn dismiss_task_id(&mut self, task_id: &str) -> Result<Vec<Assignment>> {
        let ids: Vec<_> = self
            .assignments
            .values()
            .filter(|a| a.task_id.as_deref() == Some(task_id))
            .map(|a| a.id.clone())
            .collect();

        let removed: Vec<Assignment> = ids
            .iter()
            .filter_map(|id| self.assignments.remove(id))
            .collect();

        if !removed.is_empty() {
            self.save()?;
            info!(
                "Dismissed {} assignment(s) for task ID {}",
                removed.len(),
                task_id
            );
        }

        Ok(removed)
    }

    /// List all assignments
    pub fn list(&mut self) -> Vec<&Assignment> {
        self.reload_if_changed();
        self.assignments.values().collect()
    }

    /// List assignments for a specific segment
    pub fn list_segment(&mut self, segment: &str) -> Vec<&Assignment> {
        self.reload_if_changed();
        self.assignments
            .values()
            .filter(|a| a.segment.to_lowercase() == segment.to_lowercase())
            .collect()
    }

    /// List active assignments (all assignments are active — terminal actions remove them).
    /// Sorted by ancillary number.
    pub fn list_active(&mut self) -> Vec<&Assignment> {
        self.reload_if_changed();
        let mut assignments: Vec<&Assignment> = self.assignments.values().collect();
        assignments.sort_by_key(|a| ancillary_number(&a.ancillary_id).unwrap_or(u32::MAX));
        assignments
    }

    /// List active assignments for a specific segment, sorted by ancillary number.
    pub fn list_active_segment(&mut self, segment: &str) -> Vec<&Assignment> {
        self.reload_if_changed();
        let mut assignments: Vec<&Assignment> = self.assignments
            .values()
            .filter(|a| a.segment.to_lowercase() == segment.to_lowercase())
            .collect();
        assignments.sort_by_key(|a| ancillary_number(&a.ancillary_id).unwrap_or(u32::MAX));
        assignments
    }

    /// Find the next available ancillary for a segment.
    /// Implements round-robin selection, skipping ancillaries that have assignment
    /// records or existing workspaces.
    ///
    /// `existing_workspaces` should contain workspace names (e.g. "one", "two") that
    /// already exist on disk, so we avoid colliding with workspaces that outlived
    /// their assignment records.
    pub fn next_available_ancillary(
        &mut self,
        segment: &str,
        pool_size: u32,
        existing_workspaces: &[String],
    ) -> String {
        self.reload_if_changed();

        // Any assignment record (regardless of status) means the number is occupied.
        // Records are removed by complete_assignment/abort_assignment when the
        // workspace is cleaned up, so a lingering record means the workspace may
        // still exist.
        let mut occupied: std::collections::HashSet<u32> = self
            .assignments
            .values()
            .filter(|a| a.segment.to_lowercase() == segment.to_lowercase())
            .filter_map(|a| ancillary_number(&a.ancillary_id))
            .collect();

        // Also mark numbers for workspaces that exist on disk (e.g. a workspace
        // kept after its assignment was manually dismissed).
        for ws_name in existing_workspaces {
            if let Some(n) = word_to_number(ws_name) {
                occupied.insert(n);
            }
        }

        // Find first available in pool
        for n in 1..=pool_size {
            if !occupied.contains(&n) {
                return ancillary_id(segment, n);
            }
        }

        // All pool slots used, find next available beyond pool
        let max = occupied.iter().max().copied().unwrap_or(0);
        ancillary_id(segment, max + 1)
    }

    /// Resolve an AssignmentRef to matching assignments
    pub fn resolve(&mut self, ref_: &AssignmentRef) -> Vec<&Assignment> {
        self.reload_if_changed();
        match ref_ {
            AssignmentRef::TaskId(task_id) => self.assignments
                .values()
                .filter(|a| a.task_id.as_deref() == Some(task_id.as_str()))
                .collect(),
            AssignmentRef::Ancillary(ancillary_id) => self.assignments
                .values()
                .filter(|a| a.ancillary_id.to_lowercase() == ancillary_id.to_lowercase())
                .collect(),
        }
    }

    /// Resolve to active assignments only (all assignments are active).
    pub fn resolve_active(&mut self, ref_: &AssignmentRef) -> Vec<&Assignment> {
        self.resolve(ref_)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_number_to_word() {
        assert_eq!(number_to_word(1), "One");
        assert_eq!(number_to_word(10), "Ten");
        assert_eq!(number_to_word(20), "Twenty");
        assert_eq!(number_to_word(21), "Twenty-One");
        assert_eq!(number_to_word(42), "Forty-Two");
        assert_eq!(number_to_word(99), "Ninety-Nine");
        assert_eq!(number_to_word(100), "100");
    }

    #[test]
    fn test_word_to_number() {
        assert_eq!(word_to_number("One"), Some(1));
        assert_eq!(word_to_number("one"), Some(1));
        assert_eq!(word_to_number("TEN"), Some(10));
        assert_eq!(word_to_number("Twenty-One"), Some(21));
        assert_eq!(word_to_number("twenty-one"), Some(21));
        assert_eq!(word_to_number("Ninety-Nine"), Some(99));
        assert_eq!(word_to_number("100"), Some(100));
        assert_eq!(word_to_number("invalid"), None);
    }

    #[test]
    fn test_ancillary_id() {
        assert_eq!(ancillary_id("toren", 1), "Toren One");
        assert_eq!(ancillary_id("toren", 5), "Toren Five");
        assert_eq!(ancillary_id("toren", 21), "Toren Twenty-One");
    }

    #[test]
    fn test_ancillary_number() {
        assert_eq!(ancillary_number("Toren One"), Some(1));
        assert_eq!(ancillary_number("Toren Five"), Some(5));
        assert_eq!(ancillary_number("Toren Twenty-One"), Some(21));
        assert_eq!(ancillary_number("Toren 100"), Some(100));
    }

    #[test]
    fn test_assignment_ref_parse() {
        assert_eq!(
            AssignmentRef::parse("breq-a1b2", "toren"),
            AssignmentRef::TaskId("breq-a1b2".to_string())
        );
        assert_eq!(
            AssignmentRef::parse("Toren One", "toren"),
            AssignmentRef::Ancillary("Toren One".to_string())
        );
        assert_eq!(
            AssignmentRef::parse("one", "toren"),
            AssignmentRef::Ancillary("Toren One".to_string())
        );
        assert_eq!(
            AssignmentRef::parse("a1b2", "toren"),
            AssignmentRef::TaskId("a1b2".to_string())
        );
    }

    #[test]
    fn test_serde_backward_compat() {
        // Old format with external_id/title should deserialize into task_id/task_title
        let json = r#"{
            "id": "test-id",
            "ancillary_id": "Toren One",
            "external_id": "breq-abc",
            "segment": "toren",
            "workspace_path": "/tmp/ws",
            "source": {"type": "Reference"},
            "status": "active",
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
            "title": "Test Title"
        }"#;
        let assignment: Assignment = serde_json::from_str(json).unwrap();
        assert_eq!(assignment.task_id.as_deref(), Some("breq-abc"));
        assert_eq!(assignment.task_title.as_deref(), Some("Test Title"));

        // Old format with bead_id/bead_title
        let json2 = r#"{
            "id": "test-id2",
            "ancillary_id": "Toren Two",
            "bead_id": "breq-def",
            "segment": "toren",
            "workspace_path": "/tmp/ws2",
            "source": {"type": "Reference"},
            "status": "active",
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
            "bead_title": "Bead Title"
        }"#;
        let assignment2: Assignment = serde_json::from_str(json2).unwrap();
        assert_eq!(assignment2.task_id.as_deref(), Some("breq-def"));
        assert_eq!(assignment2.task_title.as_deref(), Some("Bead Title"));

        // CompletionRecord backward compat
        let cr_json = r#"{
            "assignment_id": "a1",
            "ancillary_id": "Toren One",
            "external_id": "breq-xyz",
            "segment": "toren",
            "completed_at": "2024-01-01T00:00:00Z",
            "reason": "completed"
        }"#;
        let record: CompletionRecord = serde_json::from_str(cr_json).unwrap();
        assert_eq!(record.task_id.as_deref(), Some("breq-xyz"));
    }
}
