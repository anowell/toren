use serde::{Deserialize, Serialize};

/// Inferred task fields from various input formats.
pub struct InferredTaskFields {
    pub task_id: Option<String>,
    pub task_title: Option<String>,
    pub task_url: Option<String>,
    pub task_source: Option<String>,
}

/// Infer task fields from an ID, URL, or prompt.
///
/// Supports:
/// - `source:id` prefix splitting on first `:` (e.g., "runes:task-abc")
/// - URL → task_id extraction (last path segment)
/// - prompt → task_title (first 120 chars of first line)
///
/// The `source` field is only set when explicitly given via `source:id` prefix.
/// When only a bare ID is provided, source is `None` — callers should search
/// across available task plugins to discover the source.
pub fn infer_task_fields(
    task_id: Option<&str>,
    task_title: Option<&str>,
    task_url: Option<&str>,
    prompt: Option<&str>,
) -> InferredTaskFields {
    let mut id = task_id.map(|s| s.to_string());
    let mut title = task_title.map(|s| s.to_string());
    let url = task_url.map(|s| s.to_string());
    let mut source: Option<String> = None;

    // Split source:id prefix
    if let Some(ref raw_id) = id {
        if let Some(colon_pos) = raw_id.find(':') {
            let prefix = &raw_id[..colon_pos];
            let rest = &raw_id[colon_pos + 1..];
            // Only treat as source:id if prefix looks like a source name (no slashes, not a URL scheme)
            if !prefix.contains('/')
                && !rest.starts_with("//")
                && !prefix.is_empty()
                && !rest.is_empty()
            {
                source = Some(prefix.to_string());
                id = Some(rest.to_string());
            }
        }
    }

    // URL → task_id extraction (last path segment)
    if id.is_none() {
        if let Some(ref u) = url {
            if let Some(last_seg) = u.trim_end_matches('/').rsplit('/').next() {
                if !last_seg.is_empty() {
                    id = Some(last_seg.to_string());
                }
            }
        }
    }

    // prompt → task_title (first 120 chars of first line)
    if title.is_none() {
        if let Some(p) = prompt {
            let first_line = p.lines().next().unwrap_or(p);
            title = Some(first_line.chars().take(120).collect());
        }
    }

    InferredTaskFields {
        task_id: id,
        task_title: title,
        task_url: url,
        task_source: source,
    }
}

/// Unified task data returned by a task resolver's `info(id)` function.
///
/// All fields except `id` and `source` are optional — different task plugins
/// may return different subsets of fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedTask {
    pub id: String,
    /// Source name (e.g., "runes", "linear").
    pub source: String,
    /// Task kind (e.g., "bug", "task", "feature").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// Split a `source:id` task link into its parts.
///
/// Task links round-trip through shell scripts and plugin calls unchanged, which is why the
/// flat `source:id` form survives alongside the structured [`crate::state::TaskLink`].
pub fn split_link(link: &str) -> Option<(String, String)> {
    let (source, id) = link.split_once(':')?;
    if source.is_empty() || id.is_empty() {
        return None;
    }
    Some((source.to_string(), id.to_string()))
}

/// Join a source and id into a task link.
pub fn format_link(source: &str, id: &str) -> String {
    format!("{}:{}", source, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_infer_source_id_splitting() {
        let result = infer_task_fields(Some("beads:breq-abc"), None, None, None);
        assert_eq!(result.task_source.as_deref(), Some("beads"));
        assert_eq!(result.task_id.as_deref(), Some("breq-abc"));
    }

    #[test]
    fn test_infer_source_id_splitting_custom_source() {
        let result = infer_task_fields(Some("linear:ENG-123"), None, None, None);
        assert_eq!(result.task_source.as_deref(), Some("linear"));
        assert_eq!(result.task_id.as_deref(), Some("ENG-123"));
    }

    #[test]
    fn test_infer_plain_id_no_default_source() {
        let result = infer_task_fields(Some("breq-abc"), None, None, None);
        // Bare ID without source: prefix → source is None (search across plugins)
        assert_eq!(result.task_source.as_deref(), None);
        assert_eq!(result.task_id.as_deref(), Some("breq-abc"));
    }

    #[test]
    fn test_infer_url_to_id_extraction() {
        let result = infer_task_fields(None, None, Some("https://linear.app/team/ENG-123"), None);
        assert_eq!(result.task_id.as_deref(), Some("ENG-123"));
        assert_eq!(
            result.task_url.as_deref(),
            Some("https://linear.app/team/ENG-123")
        );
    }

    #[test]
    fn test_infer_url_trailing_slash() {
        let result = infer_task_fields(None, None, Some("https://example.com/issues/42/"), None);
        assert_eq!(result.task_id.as_deref(), Some("42"));
    }

    #[test]
    fn test_infer_prompt_to_title() {
        let result = infer_task_fields(
            None,
            None,
            None,
            Some("Fix the login bug\nMore details here"),
        );
        assert_eq!(result.task_title.as_deref(), Some("Fix the login bug"));
        assert!(result.task_id.is_none());
        assert!(result.task_source.is_none());
    }

    #[test]
    fn test_infer_prompt_title_truncated_to_120() {
        let long_prompt = "a".repeat(200);
        let result = infer_task_fields(None, None, None, Some(&long_prompt));
        assert_eq!(result.task_title.as_ref().map(|t| t.len()), Some(120));
    }

    #[test]
    fn test_infer_explicit_title_not_overridden_by_prompt() {
        let result = infer_task_fields(
            Some("breq-abc"),
            Some("Explicit Title"),
            None,
            Some("prompt text"),
        );
        assert_eq!(result.task_title.as_deref(), Some("Explicit Title"));
    }

    #[test]
    fn test_infer_no_source_without_id() {
        let result = infer_task_fields(None, None, None, None);
        assert!(result.task_id.is_none());
        assert!(result.task_source.is_none());
    }

    #[test]
    fn test_links_round_trip() {
        assert_eq!(
            split_link("runes:tor-bau"),
            Some(("runes".to_string(), "tor-bau".to_string()))
        );
        assert_eq!(format_link("runes", "tor-bau"), "runes:tor-bau");
        assert!(split_link("no-source").is_none());
        assert!(split_link("runes:").is_none());
    }

    #[test]
    fn test_infer_url_scheme_not_split_as_source() {
        // "https://..." should not be split as source=https, id=//...
        let result = infer_task_fields(Some("https://example.com/issue/42"), None, None, None);
        // The colon in https: has rest starting with //, so it should NOT be treated as source:id
        assert_eq!(
            result.task_id.as_deref(),
            Some("https://example.com/issue/42")
        );
        assert_eq!(result.task_source.as_deref(), None);
    }
}
