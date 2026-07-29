//! Per-workspace state storage.
//!
//! State that dies with the workspace lives *in* the workspace, under `<ws>/.toren/`, split by
//! durability class rather than by domain:
//!
//! - `state.json` — durable, structured, hand-editable (`breq get`/`set`).
//! - `cache.json` — machine-churned, timestamped reads (PR/CI status). Disposable.
//!
//! Both carry a schema `version` as their first key and are written atomically. Splitting them
//! is what keeps a routine cache write from truncating the `uid` that names a live rmux session.
//!
//! There is no global registry — the VCS enumerates workspaces, and each working copy carries
//! its own state. See [`crate::place`].

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

use crate::fsutil::write_atomic;

/// Directory inside a workspace holding breq's per-workspace state.
pub const TOREN_DIR: &str = ".toren";
/// Durable workspace state, relative to [`TOREN_DIR`].
pub const STATE_FILE: &str = "state.json";
/// The flat key/value store `state.json` replaced. Migrated away on first load.
pub const LEGACY_ANNOTATIONS_FILE: &str = "annotations.json";
/// Timestamped caches, relative to [`TOREN_DIR`].
pub const CACHE_FILE: &str = "cache.json";

/// Schema version of every file breq persists.
pub const SCHEMA_VERSION: u32 = 1;

fn current_version() -> u32 {
    SCHEMA_VERSION
}

/// Refuse a file written by a newer breq rather than misparsing it into a silent data loss.
fn check_version(version: u32, path: &Path) -> Result<()> {
    if version > SCHEMA_VERSION {
        anyhow::bail!(
            "{} is schema version {}, newer than this breq understands ({}). Upgrade breq.",
            path.display(),
            version,
            SCHEMA_VERSION
        );
    }
    Ok(())
}

/// `<ws>/.toren`
pub fn toren_dir(ws_path: &Path) -> PathBuf {
    ws_path.join(TOREN_DIR)
}

/// Whether a working copy carries breq state (i.e. is a *decorated* workspace).
pub fn is_decorated(ws_path: &Path) -> bool {
    let dir = toren_dir(ws_path);
    dir.join(STATE_FILE).exists() || dir.join(LEGACY_ANNOTATIONS_FILE).exists()
}

/// Create `<ws>/.toren/` and make it invisible to the VCS.
///
/// The directory ignores itself (`.gitignore` containing `*`), which both git worktrees and
/// jj working copies honour without touching any repo-level config. An agent can still
/// force-track it, but nothing does so by accident.
pub fn ensure_toren_dir(ws_path: &Path) -> Result<PathBuf> {
    let dir = toren_dir(ws_path);
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;

    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        write_atomic(&ignore, "*\n")?;
    }
    Ok(dir)
}

/// A short, unique id for one incarnation of a workspace.
///
/// Names ("two") are reusable slots; the uid names *this* incarnation, so a session left over
/// from a deleted-and-recreated workspace is provably stale.
pub fn mint_uid() -> String {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    bytes[..6]
        .iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
}

/// The revision a workspace forked from, and the backend that can resolve it.
///
/// A bare revision is ambiguous — a git sha and a jj change id are both opaque strings — so
/// whoever hands it to a backend has to know which one it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseRevision {
    pub vcs: String,
    pub revision: String,
}

/// One task linked to a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskLink {
    pub source: String,
    pub id: String,
    /// RFC 3339. Makes "most recently added" answerable without a tracker call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_at: Option<String>,
    /// The task this workspace is *for*, when several are linked.
    #[serde(default)]
    pub primary: bool,
}

impl TaskLink {
    /// The flat `source:id` form task plugins and shell scripts speak.
    pub fn link(&self) -> String {
        crate::tasks::format_link(&self.source, &self.id)
    }
}

/// One agent session that ran in this workspace, and how to get back to it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSession {
    /// The agent's own session id — what `--resume` takes.
    ///
    /// Absent while the session is still opening: a fresh run has no id until the agent has
    /// written its own session file, so [`crate::sessions`] fills it in on the way out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Which agent kept it, since a workspace can be worked by more than one.
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    /// The agent's own summary, snapshotted so the title chain never re-reads agent files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Task link the session was started against, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// True when breq found this session in the agent's own files rather than starting it —
    /// someone ran the agent in the workspace directly. Breq has no pane for one of these, so it
    /// can neither report its liveness nor stop it; the record only makes it resumable.
    #[serde(default, skip_serializing_if = "is_false")]
    pub external: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// The agent that works this workspace, and the sessions it has kept here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentState {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub sessions: Vec<AgentSession>,
}

/// Which delivery resolver reads this workspace's PRs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryState {
    pub resolver: String,
}

/// Durable per-workspace state (`<ws>/.toren/state.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceState {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// What the human asked for, kept apart from `title`: the title is mutable and the prompt
    /// is the last rung of the chain that resolves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<BaseRevision>,
    /// Stack parent workspace name, for `setup --from` children.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<TaskLink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<DeliveryState>,
    /// Keys a human typed via `breq set` that the schema knows nothing about.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl Default for WorkspaceState {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            uid: None,
            created_at: None,
            title: None,
            prompt: None,
            base: None,
            parent: None,
            tasks: Vec::new(),
            agent: None,
            delivery: None,
            extra: Map::new(),
        }
    }
}

impl WorkspaceState {
    pub fn path(ws_path: &Path) -> PathBuf {
        toren_dir(ws_path).join(STATE_FILE)
    }

    /// Read `<ws>/.toren/state.json`, converting a legacy `annotations.json` if that is all
    /// there is. A missing file is empty state, not an error — undecorated working copies are
    /// legal and adoptable.
    ///
    /// `vcs` names the segment's backend; the flat store never recorded which one its `base`
    /// belonged to, so migration is the only chance to learn it.
    pub fn load(ws_path: &Path, vcs: Option<&str>) -> Result<Self> {
        let path = Self::path(ws_path);
        if path.exists() {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("Failed to read {}", path.display()))?;
            if content.trim().is_empty() {
                return Ok(Self::default());
            }
            let version = serde_json::from_str::<Value>(&content)
                .ok()
                .and_then(|v| v.get("version").and_then(Value::as_u64))
                .unwrap_or(SCHEMA_VERSION as u64);
            check_version(version as u32, &path)?;
            return serde_json::from_str(&content)
                .with_context(|| format!("Failed to parse {}", path.display()));
        }

        let legacy = toren_dir(ws_path).join(LEGACY_ANNOTATIONS_FILE);
        if legacy.exists() {
            return Self::migrate(ws_path, &legacy, vcs);
        }
        Ok(Self::default())
    }

    /// Convert `annotations.json` in place: write the new file, then drop the old one.
    fn migrate(ws_path: &Path, legacy: &Path, vcs: Option<&str>) -> Result<Self> {
        let content = std::fs::read_to_string(legacy)
            .with_context(|| format!("Failed to read {}", legacy.display()))?;
        let map: Map<String, Value> = if content.trim().is_empty() {
            Map::new()
        } else {
            serde_json::from_str(&content)
                .with_context(|| format!("Failed to parse {}", legacy.display()))?
        };

        let state = Self::from_annotations(&map, vcs);
        // A workspace breq cannot write to still has a readable uid — losing it here would
        // orphan a live rmux session over a permissions problem.
        match state.save(ws_path) {
            Ok(()) => {
                if let Err(e) = std::fs::remove_file(legacy) {
                    tracing::warn!("Failed to remove {}: {}", legacy.display(), e);
                }
                tracing::info!(
                    "Migrated {} to {}",
                    legacy.display(),
                    Self::path(ws_path).display()
                );
            }
            Err(e) => tracing::warn!(
                "Read {} but could not write {}: {:#}",
                legacy.display(),
                Self::path(ws_path).display(),
                e
            ),
        }
        Ok(state)
    }

    /// Unpack the flat annotation store: `agent = "claude:opus"`, `task = ["runes:tor-1"]`,
    /// an opaque `base`, and a `name`/`segment` pair the file's own path already implied.
    pub fn from_annotations(map: &Map<String, Value>, vcs: Option<&str>) -> Self {
        let string = |key: &str| map.get(key).and_then(value_to_string);
        let created_at = string("created_at");

        let tasks: Vec<TaskLink> = list_of(map, "task")
            .iter()
            .filter_map(|link| crate::tasks::split_link(link))
            .enumerate()
            .map(|(i, (source, id))| TaskLink {
                source,
                id,
                added_at: created_at.clone(),
                primary: i == 0,
            })
            .collect();

        let agent = string("agent").map(|packed| {
            let spec = crate::agents::AgentSpec::parse(&packed);
            AgentState {
                name: spec.name,
                model: spec.model,
                sessions: Vec::new(),
            }
        });

        let known = [
            "uid",
            "name",
            "segment",
            "created_at",
            "title",
            "prompt",
            "base",
            "parent",
            "task",
            "agent",
            "delivery",
        ];
        let extra: Map<String, Value> = map
            .iter()
            .filter(|(k, _)| !known.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        Self {
            version: SCHEMA_VERSION,
            uid: string("uid"),
            created_at,
            title: string("title"),
            prompt: string("prompt"),
            base: string("base").map(|revision| BaseRevision {
                vcs: vcs.unwrap_or(DEFAULT_VCS).to_string(),
                revision,
            }),
            parent: string("parent"),
            tasks,
            agent,
            delivery: string("delivery").map(|resolver| DeliveryState { resolver }),
            extra,
        }
    }

    pub fn save(&self, ws_path: &Path) -> Result<()> {
        ensure_toren_dir(ws_path)?;
        let mut content = serde_json::to_string_pretty(self)?;
        content.push('\n');
        write_atomic(&Self::path(ws_path), content)
    }

    /// Whether anything has been recorded here at all.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Task links, as `source:id` strings, most recently added last.
    pub fn task_links(&self) -> Vec<String> {
        self.tasks.iter().map(TaskLink::link).collect()
    }

    /// The task this workspace is for: the one marked primary, else the first linked.
    pub fn primary_task(&self) -> Option<&TaskLink> {
        self.tasks
            .iter()
            .find(|t| t.primary)
            .or_else(|| self.tasks.first())
    }

    /// Link a task. The first one linked is the primary. Returns whether anything changed.
    pub fn add_task(&mut self, source: &str, id: &str) -> bool {
        if self.tasks.iter().any(|t| t.source == source && t.id == id) {
            return false;
        }
        let primary = self.tasks.is_empty();
        self.tasks.push(TaskLink {
            source: source.to_string(),
            id: id.to_string(),
            added_at: Some(chrono::Utc::now().to_rfc3339()),
            primary,
        });
        true
    }

    /// Unlink a task by `source:id`. Returns whether anything changed.
    pub fn remove_task(&mut self, link: &str) -> bool {
        let before = self.tasks.len();
        self.tasks.retain(|t| t.link() != link);
        if self.tasks.len() == before {
            return false;
        }
        // Losing the primary promotes the oldest survivor rather than leaving none.
        if !self.tasks.is_empty() && !self.tasks.iter().any(|t| t.primary) {
            self.tasks[0].primary = true;
        }
        true
    }

    /// Record which agent works here, keeping the sessions it already ran.
    pub fn set_agent(&mut self, name: &str, model: Option<&str>) {
        let sessions = self.agent.take().map(|a| a.sessions).unwrap_or_default();
        self.agent = Some(AgentState {
            name: name.to_string(),
            model: model.map(|m| m.to_string()),
            sessions,
        });
    }

    /// Agent sessions kept here, oldest first.
    pub fn sessions(&self) -> &[AgentSession] {
        self.agent
            .as_ref()
            .map(|a| a.sessions.as_slice())
            .unwrap_or_default()
    }

    /// The session most recently started here.
    pub fn latest_session(&self) -> Option<&AgentSession> {
        self.sessions().last()
    }

    /// A recorded session by its agent-side id.
    pub fn session(&self, id: &str) -> Option<&AgentSession> {
        self.sessions().iter().find(|s| s.id.as_deref() == Some(id))
    }

    /// Append a session record, minting the agent entry if this is the first one.
    pub fn push_session(&mut self, session: AgentSession) {
        if self.agent.is_none() {
            self.set_agent(&session.agent, None);
        }
        if let Some(agent) = &mut self.agent {
            agent.sessions.push(session);
        }
    }

    /// The session nothing has closed out yet — the one a running agent belongs to.
    pub fn open_session_mut(&mut self) -> Option<&mut AgentSession> {
        self.agent
            .as_mut()?
            .sessions
            .iter_mut()
            .rev()
            .find(|s| s.ended_at.is_none())
    }

    /// Detach a recorded session by id, so a resume can re-record it as the most recent.
    pub fn take_session(&mut self, id: &str) -> Option<AgentSession> {
        let sessions = &mut self.agent.as_mut()?.sessions;
        let pos = sessions.iter().position(|s| s.id.as_deref() == Some(id))?;
        Some(sessions.remove(pos))
    }

    /// Set the base revision, tagging it with the backend that can resolve it.
    pub fn set_base(&mut self, vcs: Option<&str>, revision: impl Into<String>) {
        self.base = Some(BaseRevision {
            vcs: vcs.unwrap_or(DEFAULT_VCS).to_string(),
            revision: revision.into(),
        });
    }

    /// Read a field by the name `breq get` uses. A list-valued field yields one entry per line.
    pub fn get_field(&self, key: &str) -> Option<Vec<String>> {
        let one = |v: Option<String>| v.map(|v| vec![v]);
        match key {
            "uid" => one(self.uid.clone()),
            "created_at" => one(self.created_at.clone()),
            "title" => one(self.title.clone()),
            "prompt" => one(self.prompt.clone()),
            "parent" => one(self.parent.clone()),
            "base" => one(self.base.as_ref().map(|b| b.revision.clone())),
            "base.vcs" => one(self.base.as_ref().map(|b| b.vcs.clone())),
            "task" | "tasks" => Some(self.task_links()).filter(|l| !l.is_empty()),
            "agent" => one(self.agent.as_ref().map(agent_label)),
            "agent.name" => one(self.agent.as_ref().map(|a| a.name.clone())),
            "agent.model" => one(self.agent.as_ref().and_then(|a| a.model.clone())),
            "agent.session" => one(self
                .latest_session()
                .and_then(|s| s.id.clone())
                .filter(|id| !id.is_empty())),
            "agent.sessions" => {
                let ids: Vec<String> = self
                    .sessions()
                    .iter()
                    .rev()
                    .filter_map(|s| s.id.clone())
                    .collect();
                Some(ids).filter(|ids| !ids.is_empty())
            }
            "delivery" => one(self.delivery.as_ref().map(|d| d.resolver.clone())),
            other => match self.extra.get(other) {
                Some(Value::Array(items)) => {
                    Some(items.iter().filter_map(value_to_string).collect())
                }
                Some(value) => one(value_to_string(value)),
                None => None,
            },
        }
    }

    /// Write a field by the name `breq set` uses. Unknown keys land in `extra`.
    pub fn set_field(&mut self, key: &str, value: Value) -> Result<()> {
        let text = || value_to_string(&value).unwrap_or_default();
        match key {
            "uid" => self.uid = Some(text()),
            "created_at" => self.created_at = Some(text()),
            "title" => self.title = Some(text()),
            "prompt" => self.prompt = Some(text()),
            "parent" => self.parent = Some(text()),
            "base" => {
                let vcs = self.base.as_ref().map(|b| b.vcs.clone());
                self.set_base(vcs.as_deref(), text());
            }
            "base.vcs" => match &mut self.base {
                Some(base) => base.vcs = text(),
                None => anyhow::bail!("No base revision to set a vcs on"),
            },
            "task" | "tasks" => anyhow::bail!(
                "Task links are a list — use `breq set <ws> +task <source>:<id>` to add one"
            ),
            "agent" => {
                let spec = crate::agents::AgentSpec::parse(&text());
                self.set_agent(&spec.name, spec.model.as_deref());
            }
            "agent.model" => match &mut self.agent {
                Some(agent) => agent.model = Some(text()).filter(|m| !m.is_empty()),
                None => anyhow::bail!("No agent recorded — set `agent` first"),
            },
            "delivery" => {
                self.delivery = Some(DeliveryState { resolver: text() });
            }
            other => {
                check_writable(other)?;
                self.extra.insert(other.to_string(), value);
            }
        }
        Ok(())
    }

    /// Append to a list-valued field, ignoring duplicates. Returns whether anything changed.
    pub fn add_to_field(&mut self, key: &str, value: &str) -> Result<bool> {
        if key == "task" || key == "tasks" {
            let (source, id) = crate::tasks::split_link(value)
                .with_context(|| format!("Malformed task link '{}' — expected source:id", value))?;
            return Ok(self.add_task(&source, &id));
        }
        check_writable(key)?;
        let mut items = self.extra_list(key);
        if items.iter().any(|i| i == value) {
            return Ok(false);
        }
        items.push(value.to_string());
        self.set_extra_list(key, items);
        Ok(true)
    }

    /// Drop from a list-valued field. Returns whether anything changed.
    pub fn remove_from_field(&mut self, key: &str, value: &str) -> Result<bool> {
        if key == "task" || key == "tasks" {
            return Ok(self.remove_task(value));
        }
        let mut items = self.extra_list(key);
        let before = items.len();
        items.retain(|i| i != value);
        if items.len() == before {
            return Ok(false);
        }
        if items.is_empty() {
            self.extra.remove(key);
        } else {
            self.set_extra_list(key, items);
        }
        Ok(true)
    }

    fn extra_list(&self, key: &str) -> Vec<String> {
        match self.extra.get(key) {
            Some(Value::Array(items)) => items.iter().filter_map(value_to_string).collect(),
            Some(other) => value_to_string(other).into_iter().collect(),
            None => Vec::new(),
        }
    }

    fn set_extra_list(&mut self, key: &str, items: Vec<String>) {
        self.extra.insert(
            key.to_string(),
            Value::Array(items.into_iter().map(Value::String).collect()),
        );
    }

    /// `extra` keys, sorted, for full rendering.
    pub fn extra_keys(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = self.extra.keys().map(|k| k.as_str()).collect();
        keys.sort();
        keys
    }
}

/// Keys `breq get` answers from somewhere other than stored state: derived from the place, or
/// from a live plugin call. Storing one is accepting a write nothing can ever read back.
pub const RESERVED_KEYS: &[&str] = &[
    "path",
    "workspace.path",
    "session",
    "changes",
    "branches",
    "prs",
];

/// Namespace owned by the task plugins' pass-through RPC (`breq get <ws> task.status`).
pub const TASK_NAMESPACE: &str = "task.";

/// Namespace `breq get` answers from `cache.json` — disposable by definition, so a durable value
/// stored under it would be shadowed by the next fetch.
pub const CACHE_NAMESPACE: &str = "cache.";

/// Refuse a free-form key that `breq get` could never hand back.
fn check_writable(key: &str) -> Result<()> {
    if let Some(field) = key.strip_prefix(TASK_NAMESPACE) {
        anyhow::bail!(
            "'{}' belongs to the task source, not the workspace — `breq set <ws> {}{} <value>` \
             writes it to the tracker",
            key,
            TASK_NAMESPACE,
            field
        );
    }
    if key.starts_with(CACHE_NAMESPACE) {
        anyhow::bail!(
            "'{}' names a cached read, which is disposable — durable state goes under any other \
             key",
            key
        );
    }
    if RESERVED_KEYS.contains(&key) {
        anyhow::bail!(
            "'{}' is derived from the workspace, so a stored value would never be read back",
            key
        );
    }
    Ok(())
}

/// The backend assumed when detection cannot say — the same default `backend_for` picks.
const DEFAULT_VCS: &str = "jj";

/// `claude` or `claude:opus` — the packed form `-a` accepts and `breq get agent` prints.
pub fn agent_label(agent: &AgentState) -> String {
    match &agent.model {
        Some(model) => format!("{}:{}", agent.name, model),
        None => agent.name.clone(),
    }
}

/// Read a flat annotation list value, tolerating the scalar form a hand-edit leaves behind.
fn list_of(map: &Map<String, Value>, key: &str) -> Vec<String> {
    match map.get(key) {
        Some(Value::Array(items)) => items.iter().filter_map(value_to_string).collect(),
        Some(other) => value_to_string(other).into_iter().collect(),
        None => Vec::new(),
    }
}

/// How old a cached read may get before it stops being served at all.
pub const CACHE_MAX_AGE_DAYS: i64 = 30;

/// One cached read, with the time it was taken.
#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub value: Value,
    pub fetched_at: String,
}

impl CacheEntry {
    /// How long ago this was fetched, if the timestamp parses.
    pub fn age(&self) -> Option<chrono::Duration> {
        let then = chrono::DateTime::parse_from_rfc3339(&self.fetched_at).ok()?;
        Some(chrono::Utc::now().signed_duration_since(then.with_timezone(&chrono::Utc)))
    }

    /// Whether this entry is past the point where showing its age would still be an answer.
    ///
    /// Write-through (D17) keeps the entries you touch current, so the ceiling only catches
    /// values nothing has looked at in a month — and entries with no readable timestamp, which
    /// cannot be rendered as stale and so must not be rendered as fresh.
    pub fn is_expired(&self) -> bool {
        match self.age() {
            Some(age) => age > chrono::Duration::days(CACHE_MAX_AGE_DAYS),
            None => true,
        }
    }

    /// Compact staleness marker for list output: `2m`, `3h`, `4d`.
    pub fn age_label(&self) -> String {
        match self.age() {
            Some(d) if d.num_days() > 0 => format!("{}d", d.num_days()),
            Some(d) if d.num_hours() > 0 => format!("{}h", d.num_hours()),
            Some(d) if d.num_minutes() > 0 => format!("{}m", d.num_minutes()),
            Some(_) => "now".to_string(),
            None => "?".to_string(),
        }
    }
}

/// On-disk shape of `cache.json`: a schema version wrapped around the entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheFile {
    #[serde(default = "current_version")]
    version: u32,
    #[serde(default)]
    entries: Map<String, Value>,
}

/// Timestamped, disposable per-workspace cache (`<ws>/.toren/cache.json`).
///
/// Deliberately separate from [`WorkspaceState`]: delivery state is slow to fetch and safe to
/// serve stale, while nothing here is worth keeping if it is lost.
#[derive(Debug, Clone, Default)]
pub struct Cache {
    map: Map<String, Value>,
}

impl Cache {
    pub fn path(ws_path: &Path) -> PathBuf {
        toren_dir(ws_path).join(CACHE_FILE)
    }

    /// Never fails: a cache that will not read is a cache that is simply cold.
    pub fn load(ws_path: &Path) -> Self {
        let path = Self::path(ws_path);
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(value) = serde_json::from_str::<Value>(&content) else {
            return Self::default();
        };
        // Pre-version caches were the bare entry map.
        match value.get("version").and_then(Value::as_u64) {
            Some(version) => {
                if check_version(version as u32, &path).is_err() {
                    return Self::default();
                }
                serde_json::from_value::<CacheFile>(value)
                    .map(|file| Self { map: file.entries })
                    .unwrap_or_default()
            }
            None => serde_json::from_value::<Map<String, Value>>(value)
                .map(|map| Self { map })
                .unwrap_or_default(),
        }
    }

    pub fn save(&self, ws_path: &Path) -> Result<()> {
        ensure_toren_dir(ws_path)?;
        let file = CacheFile {
            version: SCHEMA_VERSION,
            entries: self.map.clone(),
        };
        let mut content = serde_json::to_string_pretty(&file)?;
        content.push('\n');
        write_atomic(&Self::path(ws_path), content)
    }

    /// A cached read, if one is recent enough to still mean something.
    pub fn get(&self, key: &str) -> Option<CacheEntry> {
        self.entry(key).filter(|entry| !entry.is_expired())
    }

    /// A cached read whatever its age, for callers reporting on the cache itself.
    pub fn entry(&self, key: &str) -> Option<CacheEntry> {
        let entry = self.map.get(key)?.as_object()?;
        Some(CacheEntry {
            value: entry.get("value").cloned().unwrap_or(Value::Null),
            fetched_at: entry
                .get("fetched_at")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        })
    }

    pub fn set(&mut self, key: &str, value: Value) {
        let mut entry = Map::new();
        entry.insert("value".to_string(), value);
        entry.insert(
            "fetched_at".to_string(),
            Value::String(chrono::Utc::now().to_rfc3339()),
        );
        self.map.insert(key.to_string(), Value::Object(entry));
    }

    pub fn keys(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = self.map.keys().map(|k| k.as_str()).collect();
        keys.sort();
        keys
    }
}

/// Render a JSON scalar as the string a shell script would expect. Objects and arrays
/// stay JSON so `breq get` output is always parseable.
pub fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        other => Some(other.to_string()),
    }
}

/// Parse a CLI-supplied value: JSON if it parses as a container or literal, else a string.
///
/// Keeps `breq set one count 3` numeric and `breq set one title "3 things"` a string.
pub fn parse_value(raw: &str) -> Value {
    match serde_json::from_str::<Value>(raw) {
        Ok(v @ (Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_))) => v,
        _ => Value::String(raw.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn uid_is_short_and_varies() {
        let a = mint_uid();
        let b = mint_uid();
        assert_eq!(a.len(), 6);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(a, b);
    }

    #[test]
    fn roundtrips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();

        let mut state = WorkspaceState {
            uid: Some("abc123".into()),
            ..Default::default()
        };
        state.add_task("runes", "tor-1");
        state.set_agent("claude", Some("opus"));
        state.save(ws).unwrap();

        let loaded = WorkspaceState::load(ws, Some("jj")).unwrap();
        assert_eq!(loaded.uid.as_deref(), Some("abc123"));
        assert_eq!(loaded.task_links(), vec!["runes:tor-1"]);
        assert_eq!(
            loaded.agent.as_ref().unwrap().model.as_deref(),
            Some("opus")
        );
        assert!(is_decorated(ws));
        // The directory hides itself from the VCS.
        assert_eq!(
            std::fs::read_to_string(ws.join(".toren/.gitignore")).unwrap(),
            "*\n"
        );
    }

    #[test]
    fn version_leads_the_file() {
        let dir = tempfile::tempdir().unwrap();
        WorkspaceState::default().save(dir.path()).unwrap();
        let content = std::fs::read_to_string(dir.path().join(".toren/state.json")).unwrap();
        assert!(
            content.lines().nth(1).unwrap().contains("\"version\": 1"),
            "{}",
            content
        );
    }

    #[test]
    fn a_newer_schema_refuses_to_load() {
        let dir = tempfile::tempdir().unwrap();
        ensure_toren_dir(dir.path()).unwrap();
        std::fs::write(
            WorkspaceState::path(dir.path()),
            r#"{"version": 99, "uid": "abc123"}"#,
        )
        .unwrap();

        let err = WorkspaceState::load(dir.path(), None).unwrap_err();
        assert!(err.to_string().contains("newer than this breq"), "{}", err);
        // And the lossy read does not pretend the workspace is undecorated on disk.
        assert!(is_decorated(dir.path()));
    }

    /// The file is read by humans, so its shape is part of the contract.
    #[test]
    fn the_serialized_shape_is_flat_and_ordered() {
        let mut state = WorkspaceState {
            uid: Some("k3m9xz".into()),
            created_at: Some("2026-07-24T22:07:02Z".into()),
            title: Some("Mirror rmux panes".into()),
            parent: Some("one".into()),
            ..Default::default()
        };
        state.set_base(Some("jj"), "qpvuntsm");
        state.add_task("runes", "tor-fe7");
        state.tasks[0].added_at = Some("2026-07-24T22:07:02Z".into());
        state.set_agent("claude", Some("opus"));
        state.set_field("delivery", json!("github")).unwrap();

        assert_eq!(
            serde_json::to_string_pretty(&state).unwrap(),
            r#"{
  "version": 1,
  "uid": "k3m9xz",
  "created_at": "2026-07-24T22:07:02Z",
  "title": "Mirror rmux panes",
  "base": {
    "vcs": "jj",
    "revision": "qpvuntsm"
  },
  "parent": "one",
  "tasks": [
    {
      "source": "runes",
      "id": "tor-fe7",
      "added_at": "2026-07-24T22:07:02Z",
      "primary": true
    }
  ],
  "agent": {
    "name": "claude",
    "model": "opus",
    "sessions": []
  },
  "delivery": {
    "resolver": "github"
  }
}"#
        );
    }

    #[test]
    fn missing_state_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let state = WorkspaceState::load(dir.path(), None).unwrap();
        assert!(state.is_empty());
        assert!(!is_decorated(dir.path()));
    }

    /// The whole point of the migration: packed strings become structure.
    #[test]
    fn migrates_annotations_and_removes_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        ensure_toren_dir(ws).unwrap();
        std::fs::write(
            toren_dir(ws).join(LEGACY_ANNOTATIONS_FILE),
            serde_json::to_string_pretty(&json!({
                "uid": "k3m9xz",
                "name": "one",
                "segment": "toren",
                "created_at": "2026-07-24T22:07:02Z",
                "title": "Mirror rmux panes",
                "base": "qpvuntsm",
                "parent": "one",
                "task": ["runes:tor-fe7", "gh:12"],
                "agent": "claude:opus",
                "delivery": "github",
                "notes.scratch": "keep me"
            }))
            .unwrap(),
        )
        .unwrap();

        let state = WorkspaceState::load(ws, Some("jj")).unwrap();

        assert_eq!(state.uid.as_deref(), Some("k3m9xz"));
        assert_eq!(state.parent.as_deref(), Some("one"));
        assert_eq!(
            state.base,
            Some(BaseRevision {
                vcs: "jj".into(),
                revision: "qpvuntsm".into()
            })
        );
        assert_eq!(state.tasks.len(), 2);
        assert!(state.tasks[0].primary);
        assert!(!state.tasks[1].primary);
        assert_eq!(
            state.tasks[0].added_at.as_deref(),
            state.created_at.as_deref()
        );
        let agent = state.agent.as_ref().unwrap();
        assert_eq!(agent.name, "claude");
        assert_eq!(agent.model.as_deref(), Some("opus"));
        assert!(agent.sessions.is_empty());
        assert_eq!(
            state.delivery,
            Some(DeliveryState {
                resolver: "github".into()
            })
        );
        // The dead keys go; anything unrecognised lands in extra.
        assert_eq!(state.extra.len(), 1);
        assert_eq!(state.extra["notes.scratch"], json!("keep me"));

        // Converted on disk, once.
        assert!(!toren_dir(ws).join(LEGACY_ANNOTATIONS_FILE).exists());
        assert!(WorkspaceState::path(ws).exists());
        let reloaded = WorkspaceState::load(ws, Some("jj")).unwrap();
        assert_eq!(reloaded, state);
    }

    #[test]
    fn migration_infers_the_vcs_of_the_base_revision() {
        let dir = tempfile::tempdir().unwrap();
        ensure_toren_dir(dir.path()).unwrap();
        std::fs::write(
            toren_dir(dir.path()).join(LEGACY_ANNOTATIONS_FILE),
            r#"{"base": "9f1c2d0"}"#,
        )
        .unwrap();

        let state = WorkspaceState::load(dir.path(), Some("git")).unwrap();
        assert_eq!(state.base.unwrap().vcs, "git");
    }

    #[test]
    fn a_state_file_wins_over_a_stray_annotations_file() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let state = WorkspaceState {
            uid: Some("current".into()),
            ..Default::default()
        };
        state.save(ws).unwrap();
        std::fs::write(
            toren_dir(ws).join(LEGACY_ANNOTATIONS_FILE),
            r#"{"uid": "stale"}"#,
        )
        .unwrap();

        assert_eq!(
            WorkspaceState::load(ws, None).unwrap().uid.as_deref(),
            Some("current")
        );
    }

    #[test]
    fn task_links_dedupe_and_detach() {
        let mut state = WorkspaceState::default();
        assert!(state.add_task("runes", "a"));
        assert!(!state.add_task("runes", "a"));
        assert!(state.add_task("gh", "2"));
        assert_eq!(state.task_links(), vec!["runes:a", "gh:2"]);
        assert_eq!(state.primary_task().unwrap().id, "a");

        // Dropping the primary promotes the survivor rather than leaving none.
        assert!(state.remove_task("runes:a"));
        assert_eq!(state.primary_task().unwrap().id, "2");
        assert!(!state.remove_from_field("task", "nope").unwrap());
        assert!(state.remove_from_field("task", "gh:2").unwrap());
        assert!(state.tasks.is_empty());
    }

    #[test]
    fn fields_read_and_write_by_name() {
        let mut state = WorkspaceState::default();
        state.set_field("title", json!("a title")).unwrap();
        state.set_field("agent", json!("claude:opus")).unwrap();
        state.set_field("delivery", json!("github")).unwrap();
        state.set_field("scratch", json!(3)).unwrap();

        assert_eq!(state.get_field("title").unwrap(), vec!["a title"]);
        assert_eq!(state.get_field("agent").unwrap(), vec!["claude:opus"]);
        assert_eq!(state.get_field("agent.name").unwrap(), vec!["claude"]);
        assert_eq!(state.get_field("delivery").unwrap(), vec!["github"]);
        assert_eq!(state.get_field("scratch").unwrap(), vec!["3"]);
        assert!(state.get_field("prompt").is_none());
        assert_eq!(state.extra_keys(), vec!["scratch"]);
    }

    #[test]
    fn setting_the_agent_keeps_its_sessions() {
        let mut state = WorkspaceState::default();
        state.set_agent("claude", None);
        state.push_session(AgentSession {
            id: Some("3f2a".into()),
            agent: "claude".into(),
            ..Default::default()
        });
        state.set_agent("claude", Some("opus"));
        assert_eq!(state.agent.unwrap().sessions.len(), 1);
    }

    /// Both halves of D13's namespace collision: a key the tracker owns, and a key derived from
    /// the place. Either would store fine and then be unreadable, so neither is accepted.
    #[test]
    fn keys_that_could_never_be_read_back_are_refused() {
        let mut state = WorkspaceState::default();

        let err = state
            .set_field("task.status", json!("done"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("belongs to the task source"), "{}", err);

        let err = state.set_field("path", json!("/tmp/x")).unwrap_err();
        assert!(err.to_string().contains("derived"), "{}", err);
        assert!(state.add_to_field("prs", "#12").is_err());

        // And the third: durable state under the namespace that reads the disposable file.
        let err = state
            .set_field("cache.delivery.prs", json!([]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("disposable"), "{}", err);

        // Anything else is still free-form.
        state.set_field("notes", json!("keep me")).unwrap();
        assert_eq!(state.get_field("notes").unwrap(), vec!["keep me"]);
    }

    #[test]
    fn sessions_read_back_newest_first() {
        let mut state = WorkspaceState::default();
        state.set_agent("claude", None);
        state.push_session(AgentSession {
            id: Some("3f2a".into()),
            agent: "claude".into(),
            ended_at: Some("2026-07-24T23:07:02Z".into()),
            ..Default::default()
        });
        state.push_session(AgentSession {
            agent: "claude".into(),
            ..Default::default()
        });

        assert_eq!(state.sessions().len(), 2);
        assert!(state.latest_session().unwrap().id.is_none());
        assert!(state.session("3f2a").is_some());
        // A pending session has no id to print, so only recorded ones are listed.
        assert_eq!(state.get_field("agent.sessions").unwrap(), vec!["3f2a"]);
        assert!(state.get_field("agent.session").is_none());

        // The open one is the last unclosed record, and it can be detached by id.
        assert!(state.open_session_mut().is_some());
        assert!(state.take_session("3f2a").is_some());
        assert_eq!(state.sessions().len(), 1);
    }

    #[test]
    fn a_task_list_cannot_be_clobbered_by_a_scalar_set() {
        let mut state = WorkspaceState::default();
        assert!(state.set_field("task", json!("runes:a")).is_err());
    }

    #[test]
    fn extra_lists_still_work() {
        let mut state = WorkspaceState::default();
        assert!(state.add_to_field("reviewers", "ana").unwrap());
        assert!(!state.add_to_field("reviewers", "ana").unwrap());
        assert!(state.add_to_field("reviewers", "bo").unwrap());
        assert_eq!(state.get_field("reviewers").unwrap(), vec!["ana", "bo"]);
        assert!(state.remove_from_field("reviewers", "ana").unwrap());
        assert_eq!(state.get_field("reviewers").unwrap(), vec!["bo"]);
    }

    #[test]
    fn parse_value_keeps_prose_as_string() {
        assert_eq!(parse_value("done"), Value::String("done".into()));
        assert_eq!(parse_value("3"), Value::Number(3.into()));
        assert_eq!(parse_value("true"), Value::Bool(true));
        assert_eq!(parse_value("3 things"), Value::String("3 things".into()));
    }

    #[test]
    fn cache_entries_carry_a_timestamp_and_a_version() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = Cache::default();
        cache.set("delivery.prs", json!([{"id": 12}]));
        cache.save(dir.path()).unwrap();

        let content = std::fs::read_to_string(Cache::path(dir.path())).unwrap();
        assert!(content.contains("\"version\": 1"), "{}", content);

        let loaded = Cache::load(dir.path());
        let entry = loaded.get("delivery.prs").unwrap();
        assert_eq!(entry.value[0]["id"], 12);
        assert!(!entry.fetched_at.is_empty());
        assert_eq!(entry.age_label(), "now");
    }

    #[test]
    fn a_pre_version_cache_still_reads() {
        let dir = tempfile::tempdir().unwrap();
        ensure_toren_dir(dir.path()).unwrap();
        std::fs::write(
            Cache::path(dir.path()),
            r#"{"delivery.prs": {"value": [], "fetched_at": "2026-07-24T22:07:02Z"}}"#,
        )
        .unwrap();

        let cache = Cache::load(dir.path());
        assert_eq!(cache.keys(), vec!["delivery.prs"]);
    }

    /// The bug write-through replaces a TTL for: a year-old value used to be served as
    /// confidently as a fresh one.
    #[test]
    fn an_ancient_cache_entry_is_not_served() {
        let dir = tempfile::tempdir().unwrap();
        ensure_toren_dir(dir.path()).unwrap();
        let stamp =
            (chrono::Utc::now() - chrono::Duration::days(CACHE_MAX_AGE_DAYS + 1)).to_rfc3339();
        std::fs::write(
            Cache::path(dir.path()),
            serde_json::to_string(&json!({
                "version": 1,
                "entries": {
                    "delivery.prs": { "value": [{"id": "#12"}], "fetched_at": stamp },
                    "tasks.runes:tor-1": { "value": {"title": "t"} }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let cache = Cache::load(dir.path());
        assert!(cache.get("delivery.prs").is_none(), "too old to answer");
        // Nor is one with no timestamp, since its staleness could not be shown either.
        assert!(cache.get("tasks.runes:tor-1").is_none());
        // The entries are still there to report on.
        assert!(cache.entry("delivery.prs").is_some());
        assert_eq!(cache.keys().len(), 2);
    }

    #[test]
    fn a_newer_cache_schema_reads_as_cold() {
        let dir = tempfile::tempdir().unwrap();
        ensure_toren_dir(dir.path()).unwrap();
        std::fs::write(
            Cache::path(dir.path()),
            r#"{"version": 99, "entries": {"delivery.prs": {"value": []}}}"#,
        )
        .unwrap();
        assert!(Cache::load(dir.path()).keys().is_empty());
    }
}
