//! Per-workspace annotation and cache storage.
//!
//! State that dies with the workspace lives *in* the workspace, under `<ws>/.toren/`:
//!
//! - `annotations.json` — small, stable, hand-editable key/values (`breq get`/`set`).
//! - `cache.json` — machine-churned, timestamped reads (PR/CI status). Disposable.
//!
//! Keys are flat strings; dots are literal characters, not a nesting path. That keeps the
//! file readable and `breq set one delivery.pr ...` unsurprising.
//!
//! There is no global registry — the VCS enumerates workspaces, and each working copy
//! carries its own annotations. See [`crate::place`].

use anyhow::{Context, Result};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

/// Directory inside a workspace holding breq's per-workspace state.
pub const TOREN_DIR: &str = ".toren";
/// Hand-editable annotations, relative to [`TOREN_DIR`].
pub const ANNOTATIONS_FILE: &str = "annotations.json";
/// Timestamped caches, relative to [`TOREN_DIR`].
pub const CACHE_FILE: &str = "cache.json";

/// Core annotation keys written by breq itself. Everything else belongs to a plugin
/// and must be namespaced (`<plugin>.<key>`), so a plugin can never collide with core.
pub const CORE_KEYS: &[&str] = &[
    "uid",
    "name",
    "segment",
    "created_at",
    "base",
    "parent",
    "title",
    "agent",
    "task",
];

/// `<ws>/.toren`
pub fn toren_dir(ws_path: &Path) -> PathBuf {
    ws_path.join(TOREN_DIR)
}

/// Whether a working copy carries breq annotations (i.e. is a *decorated* workspace).
pub fn is_decorated(ws_path: &Path) -> bool {
    toren_dir(ws_path).join(ANNOTATIONS_FILE).exists()
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
        std::fs::write(&ignore, "*\n")
            .with_context(|| format!("Failed to write {}", ignore.display()))?;
    }
    Ok(dir)
}

/// A short, unique id for one incarnation of a workspace.
///
/// Names ("two") are reusable slots; the uid names *this* incarnation, so a session or
/// transcript from a deleted-and-recreated workspace is provably stale.
pub fn mint_uid() -> String {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    bytes[..6]
        .iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
}

/// Flat key/value annotations for one workspace.
#[derive(Debug, Clone, Default)]
pub struct Annotations {
    map: Map<String, Value>,
}

impl Annotations {
    /// Read `<ws>/.toren/annotations.json`. A missing file is an empty set, not an error —
    /// undecorated working copies are legal and adoptable.
    pub fn load(ws_path: &Path) -> Result<Self> {
        let path = Self::path(ws_path);
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        if content.trim().is_empty() {
            return Ok(Self::default());
        }
        let map: Map<String, Value> = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        Ok(Self { map })
    }

    /// As [`Annotations::load`], but never fails — a corrupt file reads as empty.
    pub fn load_lossy(ws_path: &Path) -> Self {
        Self::load(ws_path).unwrap_or_default()
    }

    pub fn path(ws_path: &Path) -> PathBuf {
        toren_dir(ws_path).join(ANNOTATIONS_FILE)
    }

    pub fn save(&self, ws_path: &Path) -> Result<()> {
        ensure_toren_dir(ws_path)?;
        let path = Self::path(ws_path);
        let mut content = serde_json::to_string_pretty(&self.map)?;
        content.push('\n');
        std::fs::write(&path, content)
            .with_context(|| format!("Failed to write {}", path.display()))
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.map.get(key)
    }

    /// Scalar read, rendered as a plain string (numbers and bools included).
    pub fn get_str(&self, key: &str) -> Option<String> {
        self.map.get(key).and_then(value_to_string)
    }

    /// List read. A scalar reads as a one-element list, so `task` works whether it was
    /// written by breq or hand-edited to a bare string.
    pub fn get_list(&self, key: &str) -> Vec<String> {
        match self.map.get(key) {
            Some(Value::Array(items)) => items.iter().filter_map(value_to_string).collect(),
            Some(other) => value_to_string(other).into_iter().collect(),
            None => Vec::new(),
        }
    }

    pub fn set(&mut self, key: &str, value: Value) {
        self.map.insert(key.to_string(), value);
    }

    pub fn set_str(&mut self, key: &str, value: impl Into<String>) {
        self.map
            .insert(key.to_string(), Value::String(value.into()));
    }

    /// Set only if absent — used for values that describe the incarnation's birth.
    pub fn set_default(&mut self, key: &str, value: impl Into<String>) {
        if !self.map.contains_key(key) {
            self.set_str(key, value);
        }
    }

    pub fn remove(&mut self, key: &str) -> bool {
        self.map.remove(key).is_some()
    }

    /// Append to a list-valued key, ignoring duplicates. Returns whether anything changed.
    pub fn add_to_list(&mut self, key: &str, value: &str) -> bool {
        let mut items = self.get_list(key);
        if items.iter().any(|i| i == value) {
            return false;
        }
        items.push(value.to_string());
        self.set_list(key, items);
        true
    }

    /// Drop from a list-valued key. Returns whether anything changed.
    pub fn remove_from_list(&mut self, key: &str, value: &str) -> bool {
        let mut items = self.get_list(key);
        let before = items.len();
        items.retain(|i| i != value);
        if items.len() == before {
            return false;
        }
        if items.is_empty() {
            self.map.remove(key);
        } else {
            self.set_list(key, items);
        }
        true
    }

    fn set_list(&mut self, key: &str, items: Vec<String>) {
        self.map.insert(
            key.to_string(),
            Value::Array(items.into_iter().map(Value::String).collect()),
        );
    }

    /// All keys, sorted, for full rendering.
    pub fn keys(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = self.map.keys().map(|k| k.as_str()).collect();
        keys.sort();
        keys
    }

    /// Keys a plugin wrote — everything that isn't core.
    pub fn plugin_keys(&self) -> Vec<&str> {
        self.keys()
            .into_iter()
            .filter(|k| !CORE_KEYS.contains(k))
            .collect()
    }

    pub fn as_map(&self) -> &Map<String, Value> {
        &self.map
    }
}

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

/// Timestamped, disposable per-workspace cache (`<ws>/.toren/cache.json`).
///
/// Deliberately separate from annotations: delivery state is slow to fetch and safe to
/// serve stale, while task-source-owned fields are never cached at all.
#[derive(Debug, Clone, Default)]
pub struct Cache {
    map: Map<String, Value>,
}

impl Cache {
    pub fn path(ws_path: &Path) -> PathBuf {
        toren_dir(ws_path).join(CACHE_FILE)
    }

    pub fn load(ws_path: &Path) -> Self {
        let path = Self::path(ws_path);
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&content)
            .map(|map| Self { map })
            .unwrap_or_default()
    }

    pub fn save(&self, ws_path: &Path) -> Result<()> {
        ensure_toren_dir(ws_path)?;
        let path = Self::path(ws_path);
        let mut content = serde_json::to_string_pretty(&self.map)?;
        content.push('\n');
        std::fs::write(&path, content)
            .with_context(|| format!("Failed to write {}", path.display()))
    }

    pub fn get(&self, key: &str) -> Option<CacheEntry> {
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

        let mut ann = Annotations::default();
        ann.set_str("uid", "abc123");
        ann.add_to_list("task", "runes:tor-1");
        ann.save(ws).unwrap();

        let loaded = Annotations::load(ws).unwrap();
        assert_eq!(loaded.get_str("uid").as_deref(), Some("abc123"));
        assert_eq!(loaded.get_list("task"), vec!["runes:tor-1"]);
        assert!(is_decorated(ws));
        // The directory hides itself from the VCS.
        assert_eq!(
            std::fs::read_to_string(ws.join(".toren/.gitignore")).unwrap(),
            "*\n"
        );
    }

    #[test]
    fn missing_annotations_read_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let ann = Annotations::load(dir.path()).unwrap();
        assert!(ann.is_empty());
        assert!(!is_decorated(dir.path()));
    }

    #[test]
    fn list_keys_dedupe_and_detach() {
        let mut ann = Annotations::default();
        assert!(ann.add_to_list("task", "runes:a"));
        assert!(!ann.add_to_list("task", "runes:a"));
        assert!(ann.add_to_list("task", "gh:2"));
        assert_eq!(ann.get_list("task"), vec!["runes:a", "gh:2"]);

        assert!(ann.remove_from_list("task", "runes:a"));
        assert_eq!(ann.get_list("task"), vec!["gh:2"]);
        assert!(!ann.remove_from_list("task", "nope"));

        // Emptying a list drops the key rather than leaving `[]` behind.
        assert!(ann.remove_from_list("task", "gh:2"));
        assert!(ann.get("task").is_none());
    }

    #[test]
    fn scalar_reads_as_single_item_list() {
        let mut ann = Annotations::default();
        ann.set_str("task", "runes:a");
        assert_eq!(ann.get_list("task"), vec!["runes:a"]);
    }

    #[test]
    fn plugin_keys_exclude_core() {
        let mut ann = Annotations::default();
        ann.set_str("uid", "x");
        ann.set_str("delivery.pr", "12");
        assert_eq!(ann.plugin_keys(), vec!["delivery.pr"]);
    }

    #[test]
    fn parse_value_keeps_prose_as_string() {
        assert_eq!(parse_value("done"), Value::String("done".into()));
        assert_eq!(parse_value("3"), Value::Number(3.into()));
        assert_eq!(parse_value("true"), Value::Bool(true));
        assert_eq!(parse_value("3 things"), Value::String("3 things".into()));
    }

    #[test]
    fn cache_entries_carry_a_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = Cache::default();
        cache.set("delivery.prs", serde_json::json!([{"id": 12}]));
        cache.save(dir.path()).unwrap();

        let loaded = Cache::load(dir.path());
        let entry = loaded.get("delivery.prs").unwrap();
        assert_eq!(entry.value[0]["id"], 12);
        assert!(!entry.fetched_at.is_empty());
        assert_eq!(entry.age_label(), "now");
    }
}
