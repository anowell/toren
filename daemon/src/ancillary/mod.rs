pub mod runtime;
pub mod work_log;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use tokio::sync::RwLock as TokioRwLock;
use tracing::info;

pub use runtime::{AncillaryWork, ClientInput, WorkStatus};
use toren_lib::Assignment;
pub use work_log::WorkEvent;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AncillaryStatus {
    /// Idle, no active assignment
    Idle,
    /// Starting work on an assignment
    Starting,
    /// Actively working on an assignment
    Working,
    /// Awaiting user input
    AwaitingInput,
    /// Completed work (will transition to Idle)
    Completed,
    /// Failed (will transition to Idle)
    Failed,
    /// Legacy: connected via external process
    Connected,
    /// Legacy: executing via external process
    Executing,
    /// Legacy: disconnected external process
    Disconnected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ancillary {
    pub id: String,
    pub segment: String,
    pub session_token: String,
    pub status: AncillaryStatus,
    pub connected_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_activity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_instruction: Option<String>,
    /// The workspace name if this ancillary is using a jj workspace
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// The actual working directory (segment path or workspace path)
    pub working_dir: PathBuf,
}

pub struct AncillaryManager {
    ancillaries: Arc<RwLock<HashMap<String, Ancillary>>>,
}

impl AncillaryManager {
    pub fn new() -> Self {
        Self {
            ancillaries: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn register(
        &self,
        id: String,
        segment: String,
        session_token: String,
        workspace: Option<String>,
        working_dir: PathBuf,
    ) {
        let ancillary = Ancillary {
            id: id.clone(),
            segment,
            session_token,
            status: AncillaryStatus::Connected,
            connected_at: chrono::Utc::now().to_rfc3339(),
            last_activity: None,
            current_instruction: None,
            workspace,
            working_dir,
        };

        let mut ancillaries = self.ancillaries.write().unwrap();
        ancillaries.insert(id.clone(), ancillary);
        tracing::info!("Ancillary {} registered", id);
    }

    /// Check if a workspace is already in use by another ancillary
    pub fn is_workspace_in_use(&self, working_dir: &Path) -> Option<String> {
        let ancillaries = self.ancillaries.read().unwrap();
        ancillaries
            .values()
            .find(|a| a.working_dir == working_dir)
            .map(|a| a.id.clone())
    }

    /// Release an ancillary from its workspace (but don't delete the workspace)
    #[allow(dead_code)]
    pub fn release_workspace(&self, id: &str) -> Option<(String, PathBuf)> {
        let ancillaries = self.ancillaries.read().unwrap();
        ancillaries
            .get(id)
            .and_then(|a| a.workspace.clone().map(|ws| (ws, a.working_dir.clone())))
    }

    pub fn unregister(&self, id: &str) {
        let mut ancillaries = self.ancillaries.write().unwrap();
        if ancillaries.remove(id).is_some() {
            tracing::info!("Ancillary {} unregistered", id);
        }
    }

    pub fn update_status(&self, id: &str, status: AncillaryStatus) {
        let mut ancillaries = self.ancillaries.write().unwrap();
        if let Some(ancillary) = ancillaries.get_mut(id) {
            ancillary.status = status;
            ancillary.last_activity = Some(chrono::Utc::now().to_rfc3339());
        }
    }

    pub fn set_instruction(&self, id: &str, instruction: Option<String>) {
        let mut ancillaries = self.ancillaries.write().unwrap();
        if let Some(ancillary) = ancillaries.get_mut(id) {
            ancillary.current_instruction = instruction;
            ancillary.last_activity = Some(chrono::Utc::now().to_rfc3339());
        }
    }

    pub fn get(&self, id: &str) -> Option<Ancillary> {
        let ancillaries = self.ancillaries.read().unwrap();
        ancillaries.get(id).cloned()
    }

    pub fn list(&self) -> Vec<Ancillary> {
        let ancillaries = self.ancillaries.read().unwrap();
        ancillaries.values().cloned().collect()
    }

    #[allow(dead_code)]
    pub fn find_by_session(&self, session_token: &str) -> Option<Ancillary> {
        let ancillaries = self.ancillaries.read().unwrap();
        ancillaries
            .values()
            .find(|a| a.session_token == session_token)
            .cloned()
    }
}

impl Default for AncillaryManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Manages active work for ancillaries (embedded runtime).
/// This is separate from AncillaryManager which tracks connection state.
pub struct WorkManager {
    /// Active work keyed by ancillary ID
    active_work: TokioRwLock<HashMap<String, Arc<AncillaryWork>>>,
}

impl WorkManager {
    pub fn new() -> Self {
        Self {
            active_work: TokioRwLock::new(HashMap::new()),
        }
    }

    /// Start work for an ancillary on an assignment
    pub async fn start_work(
        &self,
        ancillary_id: String,
        assignment: Assignment,
    ) -> Result<Arc<AncillaryWork>> {
        info!(
            "Starting work for {} on {}",
            ancillary_id, assignment.bead_id
        );

        let work = AncillaryWork::start(ancillary_id.clone(), assignment).await?;
        let work = Arc::new(work);

        let mut active = self.active_work.write().await;
        active.insert(ancillary_id, work.clone());

        Ok(work)
    }

    /// Get active work for an ancillary
    pub async fn get_work(&self, ancillary_id: &str) -> Option<Arc<AncillaryWork>> {
        let active = self.active_work.read().await;
        active.get(ancillary_id).cloned()
    }

    /// Stop work for an ancillary
    pub async fn stop_work(&self, ancillary_id: &str) -> Option<Arc<AncillaryWork>> {
        let mut active = self.active_work.write().await;
        if let Some(work) = active.remove(ancillary_id) {
            // Interrupt the work
            let _ = work.interrupt().await;
            Some(work)
        } else {
            None
        }
    }

    /// List all active work
    #[allow(dead_code)]
    pub async fn list_active(&self) -> Vec<(String, WorkStatus)> {
        let active = self.active_work.read().await;
        let mut result = Vec::new();
        for (id, work) in active.iter() {
            let status = work.status().await;
            result.push((id.clone(), status));
        }
        result
    }

    /// Check if ancillary has active work
    pub async fn has_active_work(&self, ancillary_id: &str) -> bool {
        let active = self.active_work.read().await;
        if let Some(work) = active.get(ancillary_id) {
            let status = work.status().await;
            !matches!(status, WorkStatus::Completed | WorkStatus::Failed { .. })
        } else {
            false
        }
    }
}

impl Default for WorkManager {
    fn default() -> Self {
        Self::new()
    }
}
