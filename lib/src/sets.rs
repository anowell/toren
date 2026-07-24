//! The observable sets of one workspace.
//!
//! A workspace is a container of independent sets — agent sessions, changes, remote branches,
//! PRs, CI, tasks. There are deliberately no edges between them and no rolled-up status: which
//! session produced which commit is not load-bearing, and an aggregate "state" would have to
//! invent judgments the sets already show plainly.
//!
//! `breq get` renders every set in full; `breq list` renders a compact slice of each. Both read
//! the same join, at two zoom levels.
//!
//! Freshness rules differ per set on purpose:
//! - VCS-derived sets (changes, branches) are computed on the spot — local and fast.
//! - Task fields are pass-through: asked of the tracker, never cached, so breq can never show a
//!   status that disagrees with the tracker.
//! - Delivery (PRs, CI) is cached with a timestamp, because it costs a network round trip. The
//!   cache is what `list` renders; refreshing is an explicit act.

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::annotations::Cache;
use crate::config::Config;
use crate::place::Place;
use crate::plugins::{PluginContext, PluginManager};
use crate::rmux::{self, PaneStatus};
use crate::workspace::{CommitInfo, WorkspaceManager};

/// Cache key delivery resolvers write into `<ws>/.toren/cache.json`.
pub const DELIVERY_CACHE_KEY: &str = "delivery.prs";

/// One pane in the workspace's rmux session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    /// rmux window name, e.g. "agent" or "shell".
    pub window: String,
    /// `idle` | `running` | `exited`.
    pub status: String,
    /// Foreground command, e.g. "claude" or "zsh".
    pub command: String,
    /// The agent's own view of itself, when the window runs an agent breq knows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_activity: Option<String>,
}

/// A pull request, as last reported by a delivery resolver.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrInfo {
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub url: String,
    /// The forge's verdict — `open`, `merged`, `closed`. Never inferred locally, which is how
    /// squash merges stay a non-problem.
    #[serde(default)]
    pub state: String,
    /// CI rollup for the PR: `passing`, `failing`, `pending`, or empty.
    #[serde(default)]
    pub ci: String,
}

/// A linked task, with whatever its source reports right now.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskView {
    /// `source:id` as annotated.
    pub link: String,
    pub source: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Native status from the source. Breq applies no vocabulary of its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Why the source couldn't be reached, if it couldn't.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Everything observable about one workspace.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Sets {
    pub sessions: Vec<SessionInfo>,
    pub changes: Vec<CommitInfo>,
    pub branches: Vec<String>,
    pub prs: Vec<PrInfo>,
    /// How stale the delivery cache is, e.g. "3h". `None` when nothing is cached.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prs_age: Option<String>,
    pub tasks: Vec<TaskView>,
}

/// What to spend time on while collecting.
#[derive(Debug, Clone, Copy)]
pub struct CollectOptions {
    /// Ask each task source for its current fields. Pass-through, so it costs a resolver call.
    pub tasks: bool,
    /// Re-run the delivery resolver instead of reading the cache. Never set on `list`'s
    /// default path.
    pub refresh_delivery: bool,
}

impl Default for CollectOptions {
    fn default() -> Self {
        Self {
            tasks: true,
            refresh_delivery: false,
        }
    }
}

impl CollectOptions {
    /// Local signals only — no resolver calls at all.
    pub fn local() -> Self {
        Self {
            tasks: false,
            refresh_delivery: false,
        }
    }
}

impl Sets {
    /// Read every set for a place.
    pub fn collect(
        place: &Place,
        ws_mgr: &WorkspaceManager,
        plugins: &PluginManager,
        config: &Config,
        opts: CollectOptions,
    ) -> Self {
        let sessions = collect_sessions(place, plugins);
        let changes = collect_changes(place, ws_mgr);
        let branches = if place.exists() {
            ws_mgr.remote_branches(&place.segment_path, &place.path)
        } else {
            Vec::new()
        };

        let (prs, prs_age) = if opts.refresh_delivery {
            (
                refresh_delivery(place, plugins, config, &branches),
                Some("now".to_string()),
            )
        } else {
            cached_delivery(place)
        };

        let tasks = if opts.tasks {
            collect_tasks(place, plugins)
        } else {
            place
                .tasks()
                .iter()
                .filter_map(|link| task_view_stub(link))
                .collect()
        };

        Self {
            sessions,
            changes,
            branches,
            prs,
            prs_age,
            tasks,
        }
    }

    /// Whether the workspace has work its base doesn't.
    pub fn has_changes(&self) -> bool {
        !self.changes.is_empty()
    }

    /// Whether any pane is doing something.
    pub fn is_busy(&self) -> bool {
        self.sessions.iter().any(|s| s.status == "running")
    }

    /// Compact session summary for `list`, e.g. "agent:running".
    pub fn session_summary(&self) -> String {
        if self.sessions.is_empty() {
            return "-".to_string();
        }
        self.sessions
            .iter()
            .filter(|s| s.window == rmux::AGENT_WINDOW || s.status != "idle")
            .map(|s| {
                let status = s.agent_activity.as_deref().unwrap_or(&s.status);
                format!("{}:{}", s.window, status)
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Compact delivery summary for `list`, e.g. "#12 open ci:passing (3h)".
    pub fn delivery_summary(&self) -> String {
        if self.prs.is_empty() {
            return "-".to_string();
        }
        let mut parts: Vec<String> = self
            .prs
            .iter()
            .map(|pr| {
                let mut s = format!("{} {}", pr.id, pr.state);
                if !pr.ci.is_empty() {
                    s.push_str(&format!(" ci:{}", pr.ci));
                }
                s
            })
            .collect();
        if let Some(age) = &self.prs_age {
            parts.push(format!("({})", age));
        }
        parts.join(" ")
    }

    /// Compact task summary for `list`, e.g. "runes:tor-bau(in-progress)".
    pub fn task_summary(&self) -> String {
        if self.tasks.is_empty() {
            return "-".to_string();
        }
        self.tasks
            .iter()
            .map(|t| match &t.status {
                Some(status) => format!("{}({})", t.link, status),
                None => t.link.clone(),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The row title, in falling order of authority: what the task says, what the human typed,
    /// what the agent called the session, what the work itself says.
    ///
    /// Without this chain, a task-less `breq do -p "..."` workspace is an unreadable row.
    pub fn title(&self, place: &Place, plugins: &PluginManager) -> Option<String> {
        if let Some(title) = self.tasks.iter().find_map(|t| t.title.clone()) {
            return Some(title);
        }
        if let Some(title) = place.annotations.get_str("title") {
            return Some(title);
        }
        if let Some(agent) = place.annotations.get_str("agent") {
            if let Some(title) = plugins.agent_title(&agent, &place.path) {
                return Some(title);
            }
        }
        self.changes
            .iter()
            .map(|c| c.summary.clone())
            .find(|s| !s.trim().is_empty())
    }
}

fn collect_sessions(place: &Place, plugins: &PluginManager) -> Vec<SessionInfo> {
    if !rmux::is_available() {
        return Vec::new();
    }
    let session = place.session_name();
    if !rmux::session_exists(&session) {
        return Vec::new();
    }

    let agent = place.annotations.get_str("agent");

    rmux::list_panes(&session)
        .unwrap_or_default()
        .into_iter()
        .map(|pane| {
            let status = pane.status();
            // Only the agent's own logs can tell "sitting at its prompt" from "working", and
            // only while the pane is actually alive.
            let agent_activity = match (&agent, status) {
                (Some(agent), PaneStatus::Running) if pane.window == rmux::AGENT_WINDOW => {
                    plugins.agent_activity(agent, &place.path)
                }
                _ => None,
            };
            SessionInfo {
                window: pane.window,
                status: status.to_string(),
                command: pane.command,
                agent_activity,
            }
        })
        .collect()
}

fn collect_changes(place: &Place, ws_mgr: &WorkspaceManager) -> Vec<CommitInfo> {
    if !place.exists() {
        return Vec::new();
    }
    let Some(base) = place.base() else {
        // Undecorated or pre-base workspaces still deserve a change list; fall back to the
        // backend's own notion of "exclusive to this workspace".
        return ws_mgr
            .workspace_info(&place.segment_path, &place.path, None)
            .unwrap_or_default();
    };
    ws_mgr.changes_since(&place.segment_path, &place.path, &base)
}

fn task_view_stub(link: &str) -> Option<TaskView> {
    let (source, id) = crate::tasks::split_link(link)?;
    Some(TaskView {
        link: link.to_string(),
        source,
        id,
        title: None,
        status: None,
        assignee: None,
        url: None,
        error: None,
    })
}

fn collect_tasks(place: &Place, plugins: &PluginManager) -> Vec<TaskView> {
    let links = place.tasks();
    if links.is_empty() {
        return Vec::new();
    }

    // One resolver call per link, in parallel: a row with three tasks shouldn't cost three
    // sequential round trips.
    std::thread::scope(|scope| {
        let handles: Vec<_> = links
            .iter()
            .map(|link| {
                scope.spawn(move || {
                    let mut view = task_view_stub(link)?;
                    let ctx = PluginContext::new(
                        Some(place.segment_path.clone()),
                        Some(place.segment.clone()),
                    );
                    match plugins.resolve_info(&view.source, &view.id, ctx) {
                        Ok(task) => {
                            view.title = Some(task.title).filter(|t| !t.is_empty());
                            view.status = task.status;
                            view.assignee = task.assignee;
                            view.url = task.url;
                        }
                        Err(e) => view.error = Some(format!("{}", e)),
                    }
                    Some(view)
                })
            })
            .collect();

        handles
            .into_iter()
            .filter_map(|h| h.join().ok().flatten())
            .collect()
    })
}

/// Read the delivery cache as-is. Never touches the network — this is `list`'s path.
fn cached_delivery(place: &Place) -> (Vec<PrInfo>, Option<String>) {
    if !place.exists() {
        return (Vec::new(), None);
    }
    let cache = Cache::load(&place.path);
    let Some(entry) = cache.get(DELIVERY_CACHE_KEY) else {
        return (Vec::new(), None);
    };
    let age = entry.age_label();
    let prs: Vec<PrInfo> = serde_json::from_value(entry.value).unwrap_or_default();
    (prs, Some(age))
}

/// Ask the delivery resolver and write the answer into the workspace's cache.
fn refresh_delivery(
    place: &Place,
    plugins: &PluginManager,
    config: &Config,
    branches: &[String],
) -> Vec<PrInfo> {
    let Some(resolver) = delivery_resolver(place, plugins, config) else {
        return Vec::new();
    };
    if branches.is_empty() {
        return Vec::new();
    }

    let values = match plugins.delivery_prs(&resolver, &place.path, branches) {
        Ok(values) => values,
        Err(e) => {
            tracing::warn!("delivery resolver '{}' failed: {:#}", resolver, e);
            return Vec::new();
        }
    };

    let prs: Vec<PrInfo> = values
        .iter()
        .filter_map(|v| serde_json::from_value(v.clone()).ok())
        .collect();

    let mut cache = Cache::load(&place.path);
    cache.set(
        DELIVERY_CACHE_KEY,
        serde_json::to_value(&prs).unwrap_or(serde_json::Value::Null),
    );
    if let Err(e) = cache.save(&place.path) {
        tracing::warn!("failed to write delivery cache: {:#}", e);
    }

    prs
}

/// Which delivery resolver applies to a workspace.
///
/// In falling order of authority: the workspace says so, config says so, exactly one
/// user-installed resolver exists (a user file beats the vendored defaults), a resolver's name
/// appears in a remote URL, or there is exactly one resolver at all.
pub fn delivery_resolver(
    place: &Place,
    plugins: &PluginManager,
    config: &Config,
) -> Option<String> {
    if let Some(name) = place.annotations.get_str("delivery") {
        return Some(name);
    }
    if let Some(name) = config.delivery.source.clone() {
        return Some(name);
    }

    let installed = plugins.list_delivery();

    let user_installed: Vec<&str> = installed
        .iter()
        .copied()
        .filter(|name| {
            plugins
                .get_meta(crate::plugins::Family::Delivery, name)
                .is_some_and(|m| !m.is_builtin())
        })
        .collect();
    if user_installed.len() == 1 {
        return Some(user_installed[0].to_string());
    }

    let remotes = remote_urls(&place.path);
    if let Some(name) = installed
        .iter()
        .find(|name| remotes.iter().any(|url| url.contains(*name)))
    {
        return Some(name.to_string());
    }

    if installed.len() == 1 {
        return Some(installed[0].to_string());
    }
    None
}

/// Remote URLs configured for a working copy, for forge detection.
fn remote_urls(ws_path: &Path) -> Vec<String> {
    let output = std::process::Command::new("git")
        .args(["remote", "-v"])
        .current_dir(ws_path)
        .output();

    let Ok(output) = output else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .map(|s| s.to_string())
        .collect()
}

/// Refresh delivery state for a workspace, returning the PRs now in cache.
pub fn refresh(
    place: &Place,
    ws_mgr: &WorkspaceManager,
    plugins: &PluginManager,
    config: &Config,
) -> Vec<PrInfo> {
    let branches = if place.exists() {
        ws_mgr.remote_branches(&place.segment_path, &place.path)
    } else {
        Vec::new()
    };
    refresh_delivery(place, plugins, config, &branches)
}

/// Whether a path looks like a workspace breq can read sets from.
pub fn is_workspace_dir(path: &Path) -> bool {
    path.join(".jj").exists() || path.join(".git").exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place_with_prs(prs: Vec<PrInfo>) -> Sets {
        Sets {
            prs,
            prs_age: Some("3h".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn delivery_summary_marks_staleness() {
        let sets = place_with_prs(vec![PrInfo {
            id: "#12".into(),
            state: "open".into(),
            ci: "passing".into(),
            ..Default::default()
        }]);
        assert_eq!(sets.delivery_summary(), "#12 open ci:passing (3h)");
    }

    #[test]
    fn empty_sets_render_as_dashes() {
        let sets = Sets::default();
        assert_eq!(sets.delivery_summary(), "-");
        assert_eq!(sets.task_summary(), "-");
        assert_eq!(sets.session_summary(), "-");
        assert!(!sets.has_changes());
    }

    #[test]
    fn task_summary_shows_native_status() {
        let sets = Sets {
            tasks: vec![TaskView {
                link: "runes:tor-bau".into(),
                source: "runes".into(),
                id: "tor-bau".into(),
                title: Some("workflow revamp".into()),
                status: Some("in-progress".into()),
                assignee: None,
                url: None,
                error: None,
            }],
            ..Default::default()
        };
        assert_eq!(sets.task_summary(), "runes:tor-bau(in-progress)");
    }

    #[test]
    fn session_summary_prefers_agent_activity_over_pane_liveness() {
        let sets = Sets {
            sessions: vec![SessionInfo {
                window: "agent".into(),
                status: "running".into(),
                command: "claude".into(),
                agent_activity: Some("idle".into()),
            }],
            ..Default::default()
        };
        assert_eq!(sets.session_summary(), "agent:idle");
        // The pane is alive, so the workspace still counts as busy for teardown purposes.
        assert!(sets.is_busy());
    }

    #[test]
    fn session_summary_hides_an_idle_shell() {
        let sets = Sets {
            sessions: vec![
                SessionInfo {
                    window: "shell".into(),
                    status: "idle".into(),
                    command: "zsh".into(),
                    agent_activity: None,
                },
                SessionInfo {
                    window: "agent".into(),
                    status: "exited".into(),
                    command: "claude".into(),
                    agent_activity: None,
                },
            ],
            ..Default::default()
        };
        assert_eq!(sets.session_summary(), "agent:exited");
    }
}
