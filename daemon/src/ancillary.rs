use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AncillaryStatus {
    Connected,
    Executing,
    Idle,
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

    pub fn register(&self, id: String, segment: String, session_token: String) {
        let ancillary = Ancillary {
            id: id.clone(),
            segment,
            session_token,
            status: AncillaryStatus::Connected,
            connected_at: chrono::Utc::now().to_rfc3339(),
            last_activity: None,
            current_instruction: None,
        };

        let mut ancillaries = self.ancillaries.write().unwrap();
        ancillaries.insert(id.clone(), ancillary);
        tracing::info!("Ancillary {} registered", id);
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
