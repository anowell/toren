use anyhow::{Context, Result};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use toren_lib::Config;

const SESSION_FILE: &str = ".toren/sessions.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub token: String,
    pub created_at: String, // ISO 8601 timestamp
}

pub struct SecurityContext {
    pairing_token: String,
    sessions: Arc<RwLock<HashMap<String, Session>>>,
    session_file: PathBuf,
}

impl SecurityContext {
    pub fn new(_config: &Config) -> Result<Self> {
        // Check for PAIRING_TOKEN env var, otherwise generate random
        let pairing_token = std::env::var("PAIRING_TOKEN")
            .ok()
            .unwrap_or_else(|| Self::generate_pairing_token());

        // Determine session file path
        let session_file = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(SESSION_FILE);

        let context = Self {
            pairing_token,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            session_file,
        };

        // Load persisted sessions
        if let Err(e) = context.load_sessions() {
            tracing::warn!("Failed to load persisted sessions: {}", e);
        }

        Ok(context)
    }

    pub fn pairing_token(&self) -> String {
        self.pairing_token.clone()
    }

    pub fn validate_pairing_token(&self, token: &str) -> bool {
        self.pairing_token == token
    }

    pub fn validate_session(&self, token: &str) -> bool {
        let sessions = self.sessions.read().unwrap();
        sessions.values().any(|s| s.token == token)
    }

    pub fn create_session(&self) -> Result<Session> {
        let session_id = Self::generate_session_id();
        let session_token = Self::generate_session_token();

        let session = Session {
            id: session_id.clone(),
            token: session_token,
            created_at: chrono::Utc::now().to_rfc3339(),
        };

        // Store session
        {
            let mut sessions = self.sessions.write().unwrap();
            sessions.insert(session_id.clone(), session.clone());
        }

        // Persist to disk
        if let Err(e) = self.save_sessions() {
            tracing::warn!("Failed to persist session: {}", e);
        }

        Ok(session)
    }

    fn load_sessions(&self) -> Result<()> {
        if !self.session_file.exists() {
            return Ok(());
        }

        let content = fs::read_to_string(&self.session_file)
            .context("Failed to read session file")?;

        let sessions: HashMap<String, Session> = serde_json::from_str(&content)
            .context("Failed to parse session file")?;

        let mut guard = self.sessions.write().unwrap();
        *guard = sessions;

        tracing::info!("Loaded {} persisted sessions", guard.len());

        Ok(())
    }

    fn save_sessions(&self) -> Result<()> {
        // Create parent directory if it doesn't exist
        if let Some(parent) = self.session_file.parent() {
            fs::create_dir_all(parent)
                .context("Failed to create session directory")?;
        }

        let sessions = self.sessions.read().unwrap();
        let content = serde_json::to_string_pretty(&*sessions)
            .context("Failed to serialize sessions")?;

        fs::write(&self.session_file, content)
            .context("Failed to write session file")?;

        Ok(())
    }

    fn generate_pairing_token() -> String {
        let mut rng = rand::thread_rng();
        format!("{:06}", rng.gen_range(0..1_000_000))
    }

    fn generate_session_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    fn generate_session_token() -> String {
        use rand::distributions::Alphanumeric;
        let token: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(32)
            .map(char::from)
            .collect();
        token
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pairing_token_validation() {
        let config = Config::default();
        let ctx = SecurityContext::new(&config).unwrap();

        let token = ctx.pairing_token();
        assert!(ctx.validate_pairing_token(&token));
        assert!(!ctx.validate_pairing_token("wrong_token"));
    }

    #[test]
    fn test_session_creation() {
        let config = Config::default();
        let ctx = SecurityContext::new(&config).unwrap();

        let session = ctx.create_session().unwrap();
        assert!(ctx.validate_session(&session.token));
    }
}
