use anyhow::{Context, Result};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use toren_lib::Config;

/// Web session tokens, alongside the rest of toren's global state.
const SESSION_FILE: &str = "sessions.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub token: String,
    pub created_at: String, // ISO 8601 timestamp
}

/// On-disk shape of the session file: a schema version wrapped around the sessions.
#[derive(Debug, Serialize, Deserialize)]
struct SessionFile {
    version: u32,
    #[serde(default)]
    sessions: HashMap<String, Session>,
}

pub struct SecurityContext {
    pairing_token: String,
    sessions: Arc<RwLock<HashMap<String, Session>>>,
    session_file: PathBuf,
    /// Cleared when the session file would not load. Rewriting a file this daemon could not
    /// read would un-pair every client listed in it.
    persist: bool,
}

impl SecurityContext {
    pub fn new(_config: &Config) -> Result<Self> {
        Self::with_session_file(toren_lib::toren_root().join(SESSION_FILE))
    }

    /// The same context over an explicit session file, so a test never reaches ~/.toren.
    fn with_session_file(session_file: PathBuf) -> Result<Self> {
        // Check for PAIRING_TOKEN env var, otherwise generate random
        let pairing_token = std::env::var("PAIRING_TOKEN")
            .ok()
            .unwrap_or_else(Self::generate_pairing_token);

        let mut context = Self {
            pairing_token,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            session_file,
            persist: true,
        };

        // Load persisted sessions
        if let Err(e) = context.load_sessions() {
            tracing::warn!(
                "Failed to load persisted sessions: {}; leaving {} untouched",
                e,
                context.session_file.display()
            );
            context.persist = false;
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

        let content =
            fs::read_to_string(&self.session_file).context("Failed to read session file")?;

        let value: serde_json::Value =
            serde_json::from_str(&content).context("Failed to parse session file")?;

        // Pre-version files were the bare session map.
        let sessions: HashMap<String, Session> = match value.get("version").and_then(|v| v.as_u64())
        {
            Some(version) => {
                if version > toren_lib::state::SCHEMA_VERSION as u64 {
                    anyhow::bail!(
                        "{} is schema version {}, newer than this daemon understands ({})",
                        self.session_file.display(),
                        version,
                        toren_lib::state::SCHEMA_VERSION
                    );
                }
                serde_json::from_value::<SessionFile>(value)
                    .context("Failed to parse session file")?
                    .sessions
            }
            None => serde_json::from_value(value).context("Failed to parse session file")?,
        };

        let mut guard = self.sessions.write().unwrap();
        *guard = sessions;

        tracing::info!("Loaded {} persisted sessions", guard.len());

        Ok(())
    }

    fn save_sessions(&self) -> Result<()> {
        if !self.persist {
            anyhow::bail!(
                "{} was not readable at startup; this session stays in memory",
                self.session_file.display()
            );
        }

        // Create parent directory if it doesn't exist
        if let Some(parent) = self.session_file.parent() {
            fs::create_dir_all(parent).context("Failed to create session directory")?;
        }

        let file = {
            let sessions = self.sessions.read().unwrap();
            SessionFile {
                version: toren_lib::state::SCHEMA_VERSION,
                sessions: sessions.clone(),
            }
        };
        let content =
            serde_json::to_string_pretty(&file).context("Failed to serialize sessions")?;

        toren_lib::fsutil::write_atomic(&self.session_file, content)
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

    fn context(dir: &tempfile::TempDir) -> SecurityContext {
        SecurityContext::with_session_file(dir.path().join("sessions.json")).unwrap()
    }

    #[test]
    fn test_pairing_token_validation() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(&dir);

        let token = ctx.pairing_token();
        assert!(ctx.validate_pairing_token(&token));
        assert!(!ctx.validate_pairing_token("wrong_token"));
    }

    #[test]
    fn test_session_creation() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(&dir);

        let session = ctx.create_session().unwrap();
        assert!(ctx.validate_session(&session.token));
    }

    #[test]
    fn sessions_survive_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let session = context(&dir).create_session().unwrap();

        let reloaded = context(&dir);
        assert!(reloaded.validate_session(&session.token));
    }

    #[test]
    fn a_session_file_from_a_newer_daemon_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.json");
        let content = r#"{"version": 99, "sessions": {}}"#;
        fs::write(&path, content).unwrap();

        let ctx = context(&dir);
        // Pairing still works in memory; the file it could not read is not rewritten.
        let session = ctx.create_session().unwrap();
        assert!(ctx.validate_session(&session.token));
        assert_eq!(fs::read_to_string(&path).unwrap(), content);
    }
}
