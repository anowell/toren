use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, info};

/// How the assignment was created
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum AssignmentSource {
    /// Created from an existing bead
    Bead,
    /// Created from a prompt (bead was auto-created)
    Prompt { original_prompt: String },
}

/// Current status of an assignment
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentStatus {
    /// Assignment created, ancillary not yet connected
    #[default]
    Pending,
    /// Ancillary is connected and working
    Active,
    /// Work approved/finished
    Completed,
    /// Work discarded
    Aborted,
}

/// An assignment links an ancillary to a bead in a workspace.
/// This is the central work unit shared between CLI (breq) and daemon (toren).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assignment {
    /// Unique identifier for this assignment
    pub id: String,
    /// Ancillary identifier (e.g., "Toren One")
    pub ancillary_id: String,
    /// Bead identifier (e.g., "breq-a1b2") - always present
    pub bead_id: String,
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
    /// Title of the associated bead (for display purposes)
    #[serde(default)]
    pub bead_title: Option<String>,
    /// Claude session ID for cross-interface handoff (breq <-> toren)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Number words for ancillary naming (One through Twenty)
const NUMBER_WORDS: &[&str] = &[
    "One",
    "Two",
    "Three",
    "Four",
    "Five",
    "Six",
    "Seven",
    "Eight",
    "Nine",
    "Ten",
    "Eleven",
    "Twelve",
    "Thirteen",
    "Fourteen",
    "Fifteen",
    "Sixteen",
    "Seventeen",
    "Eighteen",
    "Nineteen",
    "Twenty",
];

/// Convert a number to its word form (1 -> "One", 2 -> "Two", etc.)
pub fn number_to_word(n: u32) -> String {
    if n == 0 {
        return "Zero".to_string();
    }
    let idx = (n - 1) as usize;
    if idx < NUMBER_WORDS.len() {
        NUMBER_WORDS[idx].to_string()
    } else {
        // For numbers beyond Twenty, use numeric suffix
        format!("N{}", n)
    }
}

/// Convert a word to its number form ("One" -> 1, "Two" -> 2, etc.)
pub fn word_to_number(word: &str) -> Option<u32> {
    // Check for numeric suffix (N21, N22, etc.)
    if let Some(stripped) = word.strip_prefix('N') {
        if let Ok(n) = stripped.parse::<u32>() {
            return Some(n);
        }
    }

    let lower = word.to_lowercase();
    NUMBER_WORDS
        .iter()
        .position(|w| w.to_lowercase() == lower)
        .map(|i| (i + 1) as u32)
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
    /// Reference by bead ID (e.g., "breq-a1b2")
    Bead(String),
    /// Reference by ancillary ID (e.g., "Toren One" or just "One")
    Ancillary(String),
}

impl AssignmentRef {
    /// Parse a reference string into an AssignmentRef.
    ///
    /// Rules:
    /// - Contains hyphen -> treat as bead ID
    /// - Contains space -> treat as ancillary name
    /// - Otherwise -> try ancillary name first, then bead ID
    pub fn parse(s: &str, segment: &str) -> Self {
        if s.contains('-') {
            AssignmentRef::Bead(s.to_string())
        } else if s.contains(' ') {
            AssignmentRef::Ancillary(s.to_string())
        } else {
            // Try to interpret as ancillary number word
            if word_to_number(s).is_some() {
                let full_id = format!("{} {}", capitalize(segment), capitalize(s));
                AssignmentRef::Ancillary(full_id)
            } else {
                AssignmentRef::Bead(s.to_string())
            }
        }
    }
}

/// Manages assignments between ancillaries and beads.
/// Persistent storage in ~/.toren/assignments.json.
/// Used by both CLI (breq) and daemon (toren).
pub struct AssignmentManager {
    /// Path to the assignments.json file
    storage_path: PathBuf,
    /// Assignments keyed by assignment ID
    assignments: HashMap<String, Assignment>,
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
            return Ok(());
        }

        let content = std::fs::read_to_string(&self.storage_path)
            .with_context(|| format!("Failed to read {}", self.storage_path.display()))?;

        let assignments: Vec<Assignment> =
            serde_json::from_str(&content).with_context(|| "Failed to parse assignments.json")?;

        self.assignments.clear();
        for a in assignments {
            self.assignments.insert(a.id.clone(), a);
        }

        info!("Loaded {} assignments from disk", self.assignments.len());
        Ok(())
    }

    /// Save assignments to disk
    pub fn save(&self) -> Result<()> {
        let assignments: Vec<&Assignment> = self.assignments.values().collect();
        let content = serde_json::to_string_pretty(&assignments)
            .with_context(|| "Failed to serialize assignments")?;

        // Ensure parent directory exists
        if let Some(parent) = self.storage_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::write(&self.storage_path, content)
            .with_context(|| format!("Failed to write {}", self.storage_path.display()))?;

        debug!("Saved {} assignments to disk", self.assignments.len());
        Ok(())
    }

    /// Create a new assignment from an existing bead
    pub fn create_from_bead(
        &mut self,
        ancillary_id: &str,
        bead_id: &str,
        segment: &str,
        workspace_path: PathBuf,
        bead_title: Option<String>,
    ) -> Result<Assignment> {
        let now = chrono::Utc::now().to_rfc3339();
        let id = uuid::Uuid::new_v4().to_string();

        let assignment = Assignment {
            id,
            ancillary_id: ancillary_id.to_string(),
            bead_id: bead_id.to_string(),
            segment: segment.to_string(),
            workspace_path,
            source: AssignmentSource::Bead,
            status: AssignmentStatus::Pending,
            created_at: now.clone(),
            updated_at: now,
            bead_title,
            session_id: None,
        };

        self.assignments
            .insert(assignment.id.clone(), assignment.clone());
        self.save()?;

        info!(
            "Created assignment from bead: {} -> {}",
            ancillary_id, bead_id
        );
        Ok(assignment)
    }

    /// Create a new assignment from a prompt (auto-creates bead)
    pub fn create_from_prompt(
        &mut self,
        ancillary_id: &str,
        bead_id: &str,
        original_prompt: &str,
        segment: &str,
        workspace_path: PathBuf,
        bead_title: Option<String>,
    ) -> Result<Assignment> {
        let now = chrono::Utc::now().to_rfc3339();
        let id = uuid::Uuid::new_v4().to_string();

        let assignment = Assignment {
            id,
            ancillary_id: ancillary_id.to_string(),
            bead_id: bead_id.to_string(),
            segment: segment.to_string(),
            workspace_path,
            source: AssignmentSource::Prompt {
                original_prompt: original_prompt.to_string(),
            },
            status: AssignmentStatus::Pending,
            created_at: now.clone(),
            updated_at: now,
            bead_title,
            session_id: None,
        };

        self.assignments
            .insert(assignment.id.clone(), assignment.clone());
        self.save()?;

        info!(
            "Created assignment from prompt: {} -> {}",
            ancillary_id, bead_id
        );
        Ok(assignment)
    }

    /// Update assignment status
    pub fn update_status(&mut self, assignment_id: &str, status: AssignmentStatus) -> Result<bool> {
        if let Some(assignment) = self.assignments.get_mut(assignment_id) {
            assignment.status = status;
            assignment.updated_at = chrono::Utc::now().to_rfc3339();
            self.save()?;
            Ok(true)
        } else {
            Ok(false)
        }
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

    /// Get assignment by ID
    pub fn get(&self, assignment_id: &str) -> Option<&Assignment> {
        self.assignments.get(assignment_id)
    }

    /// Get all assignments for a bead
    pub fn get_by_bead(&self, bead_id: &str) -> Vec<&Assignment> {
        self.assignments
            .values()
            .filter(|a| a.bead_id == bead_id)
            .collect()
    }

    /// Get all assignments for an ancillary
    pub fn get_by_ancillary(&self, ancillary_id: &str) -> Vec<&Assignment> {
        self.assignments
            .values()
            .filter(|a| a.ancillary_id.to_lowercase() == ancillary_id.to_lowercase())
            .collect()
    }

    /// Get active assignment for an ancillary (should be at most one)
    pub fn get_active_for_ancillary(&self, ancillary_id: &str) -> Option<&Assignment> {
        self.assignments.values().find(|a| {
            a.ancillary_id.to_lowercase() == ancillary_id.to_lowercase()
                && matches!(
                    a.status,
                    AssignmentStatus::Pending | AssignmentStatus::Active
                )
        })
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

    /// Remove all assignments for a bead
    pub fn dismiss_bead(&mut self, bead_id: &str) -> Result<Vec<Assignment>> {
        let ids: Vec<_> = self
            .assignments
            .values()
            .filter(|a| a.bead_id == bead_id)
            .map(|a| a.id.clone())
            .collect();

        let removed: Vec<Assignment> = ids
            .iter()
            .filter_map(|id| self.assignments.remove(id))
            .collect();

        if !removed.is_empty() {
            self.save()?;
            info!(
                "Dismissed {} assignment(s) for bead {}",
                removed.len(),
                bead_id
            );
        }

        Ok(removed)
    }

    /// List all assignments
    pub fn list(&self) -> Vec<&Assignment> {
        self.assignments.values().collect()
    }

    /// List assignments for a specific segment
    pub fn list_segment(&self, segment: &str) -> Vec<&Assignment> {
        self.assignments
            .values()
            .filter(|a| a.segment.to_lowercase() == segment.to_lowercase())
            .collect()
    }

    /// List active (pending or active) assignments
    pub fn list_active(&self) -> Vec<&Assignment> {
        self.assignments
            .values()
            .filter(|a| {
                matches!(
                    a.status,
                    AssignmentStatus::Pending | AssignmentStatus::Active
                )
            })
            .collect()
    }

    /// List active assignments for a specific segment
    pub fn list_active_segment(&self, segment: &str) -> Vec<&Assignment> {
        self.assignments
            .values()
            .filter(|a| {
                a.segment.to_lowercase() == segment.to_lowercase()
                    && matches!(
                        a.status,
                        AssignmentStatus::Pending | AssignmentStatus::Active
                    )
            })
            .collect()
    }

    /// Find the next available ancillary for a segment.
    /// Implements round-robin selection, skipping ancillaries with active assignments.
    pub fn next_available_ancillary(&self, segment: &str, pool_size: u32) -> String {
        let active_assignments = self.list_segment(segment);

        // Get ancillary numbers with active assignments
        let assigned_numbers: std::collections::HashSet<u32> = active_assignments
            .iter()
            .filter(|a| {
                matches!(
                    a.status,
                    AssignmentStatus::Pending | AssignmentStatus::Active
                )
            })
            .filter_map(|a| ancillary_number(&a.ancillary_id))
            .collect();

        // Find first available in pool
        for n in 1..=pool_size {
            if !assigned_numbers.contains(&n) {
                return ancillary_id(segment, n);
            }
        }

        // All pool slots used, find next available beyond pool
        let max_assigned = assigned_numbers.iter().max().copied().unwrap_or(0);
        ancillary_id(segment, max_assigned + 1)
    }

    /// Resolve an AssignmentRef to matching active assignments
    pub fn resolve(&self, ref_: &AssignmentRef) -> Vec<&Assignment> {
        match ref_ {
            AssignmentRef::Bead(bead_id) => self.get_by_bead(bead_id),
            AssignmentRef::Ancillary(ancillary_id) => self.get_by_ancillary(ancillary_id),
        }
    }

    /// Resolve to active assignments only
    pub fn resolve_active(&self, ref_: &AssignmentRef) -> Vec<&Assignment> {
        self.resolve(ref_)
            .into_iter()
            .filter(|a| {
                matches!(
                    a.status,
                    AssignmentStatus::Pending | AssignmentStatus::Active
                )
            })
            .collect()
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
        assert_eq!(number_to_word(21), "N21");
    }

    #[test]
    fn test_word_to_number() {
        assert_eq!(word_to_number("One"), Some(1));
        assert_eq!(word_to_number("one"), Some(1));
        assert_eq!(word_to_number("TEN"), Some(10));
        assert_eq!(word_to_number("N21"), Some(21));
        assert_eq!(word_to_number("invalid"), None);
    }

    #[test]
    fn test_ancillary_id() {
        assert_eq!(ancillary_id("toren", 1), "Toren One");
        assert_eq!(ancillary_id("toren", 5), "Toren Five");
    }

    #[test]
    fn test_ancillary_number() {
        assert_eq!(ancillary_number("Toren One"), Some(1));
        assert_eq!(ancillary_number("Toren Five"), Some(5));
        assert_eq!(ancillary_number("Toren N21"), Some(21));
    }

    #[test]
    fn test_assignment_ref_parse() {
        assert_eq!(
            AssignmentRef::parse("breq-a1b2", "toren"),
            AssignmentRef::Bead("breq-a1b2".to_string())
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
            AssignmentRef::Bead("a1b2".to_string())
        );
    }
}
