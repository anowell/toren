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
//! - Everything derived from a remote — task status and title, PR state and CI — is
//!   **write-through**: a command that already pays for the call stamps the answer into
//!   `<ws>/.toren/cache.json` on its way past, and a command that would otherwise have to make
//!   the call renders the stamped copy with its age. Freshness then tracks attention: the
//!   workspaces you work in are current in `breq list`, and the ones you have not touched read
//!   as visibly stale rather than costing a round trip each.
//! - `breq list` is the one command that must never write, and never calls out at all.

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::config::Config;
use crate::place::Place;
use crate::plugins::{PluginContext, PluginManager};
use crate::rmux::{self, PaneStatus};
use crate::state::Cache;
use crate::workspace::{CommitInfo, WorkspaceManager};

/// Cache key delivery resolvers write into `<ws>/.toren/cache.json`.
pub const DELIVERY_CACHE_KEY: &str = "delivery.prs";

/// Cache key one task's last live read lands under.
///
/// Deliberately not `task.<link>`: that namespace belongs to the task plugins' pass-through RPC,
/// so a key inside it could never be read back by name.
pub fn task_cache_key(link: &str) -> String {
    format!("tasks.{}", link)
}

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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// How stale this read is, e.g. "3h". `None` when it was just made.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age: Option<String>,
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
    /// Include the task set at all.
    pub tasks: bool,
    /// Ask the tracker and the delivery resolver rather than reading the cache, and write what
    /// comes back through to it. Off is `breq list`'s path: cache only, no calls, no writes.
    pub refresh: bool,
    /// Whether a refreshed value is written back to the workspace cache. `breq list` is the one
    /// command that must not write, even when `--refresh` makes it pay for the calls.
    pub write_through: bool,
}

impl Default for CollectOptions {
    fn default() -> Self {
        Self::cached()
    }
}

impl CollectOptions {
    /// Render what the cache already holds. Makes no remote call and writes nothing.
    pub fn cached() -> Self {
        Self {
            tasks: true,
            refresh: false,
            write_through: true,
        }
    }

    /// Ask every remote and refresh the cache on the way past.
    pub fn live() -> Self {
        Self {
            tasks: true,
            refresh: true,
            write_through: true,
        }
    }

    /// Local signals only — no resolver calls and no task set at all.
    pub fn local() -> Self {
        Self {
            tasks: false,
            refresh: false,
            write_through: true,
        }
    }

    /// Same sets, with the remote calls turned on or off.
    pub fn with_refresh(self, refresh: bool) -> Self {
        Self { refresh, ..self }
    }

    /// Same sets, without the write-through: whatever comes back is rendered and dropped.
    pub fn read_only(self) -> Self {
        Self {
            write_through: false,
            ..self
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

        // One load and at most one write per collect: read commands refresh the cache as a side
        // effect, and a cache failure must never fail the command that triggered it. `breq list`
        // opts out of the write entirely — it is the one command that reads every workspace.
        let mut cache = Cache::load(&place.path);
        let mut written = false;

        let (prs, prs_age) = match opts
            .refresh
            .then(|| fetch_delivery(place, plugins, config, &branches))
            .flatten()
        {
            Some(prs) => {
                cache.set(
                    DELIVERY_CACHE_KEY,
                    serde_json::to_value(&prs).unwrap_or(serde_json::Value::Null),
                );
                written = true;
                (prs, Some("now".to_string()))
            }
            None => cached_delivery(&cache),
        };

        let tasks = match (opts.tasks, opts.refresh) {
            // The links themselves are local knowledge; only their fields cost anything.
            (false, _) => place
                .tasks()
                .iter()
                .filter_map(|link| task_view_stub(link))
                .collect(),
            (true, true) => {
                let tasks = fetch_tasks(place, plugins);
                for task in &tasks {
                    if task.error.is_none() {
                        cache.set(&task_cache_key(&task.link), task_cache_value(task));
                        written = true;
                    }
                }
                tasks
            }
            (true, false) => cached_tasks(place, &cache),
        };

        if written && opts.write_through && place.exists() {
            if let Err(e) = cache.save(&place.path) {
                tracing::warn!(
                    "failed to write {}: {:#}",
                    Cache::path(&place.path).display(),
                    e
                );
            }
        }

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
            .map(|t| match (&t.status, &t.age) {
                (Some(status), Some(age)) => format!("{}({} {})", t.link, status, age),
                (Some(status), None) => format!("{}({})", t.link, status),
                (None, _) => t.link.clone(),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// What to call this workspace, in falling order of authority.
    ///
    /// 1. the most recently linked task, while it is still open — the tracker owns that title
    ///    and keeps editing it, so a stored copy would go stale
    /// 2. the most recent agent session's own summary, snapshotted at session end (and read
    ///    live for a session still in flight)
    /// 3. the stored `title`, which `breq do` writes at start and `breq set` can override
    /// 4. the original prompt, kept apart from the title precisely so it can be this rung
    /// 5. the newest commit's summary, for a working copy that was never `breq do`'d
    /// 6. `"<shell> shell"` — a workspace that has only ever been a place to stand
    pub fn title(&self, place: &Place, plugins: &PluginManager) -> String {
        if let Some(title) = self.open_task_title(place) {
            return title;
        }
        if let Some(title) = self.session_title(place, plugins) {
            return title;
        }
        if let Some(title) = place.state.title.clone().filter(|t| !t.trim().is_empty()) {
            return title;
        }
        if let Some(prompt) = place.state.prompt.as_deref() {
            let first = prompt.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
            if !first.is_empty() {
                return first.to_string();
            }
        }
        if let Some(summary) = self
            .changes
            .iter()
            .map(|c| c.summary.clone())
            .find(|s| !s.trim().is_empty())
        {
            return summary;
        }
        format!("{} shell", shell_name())
    }

    /// The title of the most recently linked task, unless the tracker says it is finished.
    fn open_task_title(&self, place: &Place) -> Option<String> {
        let mut links: Vec<&crate::state::TaskLink> = place.state.tasks.iter().collect();
        links.sort_by(|a, b| a.added_at.cmp(&b.added_at));
        for link in links.iter().rev() {
            let flat = link.link();
            let Some(view) = self.tasks.iter().find(|t| t.link == flat) else {
                continue;
            };
            if view.status.as_deref().is_some_and(is_closed_status) {
                continue;
            }
            if let Some(title) = view.title.clone().filter(|t| !t.trim().is_empty()) {
                return Some(title);
            }
        }
        None
    }

    /// What the agent called its most recent session here.
    fn session_title(&self, place: &Place, plugins: &PluginManager) -> Option<String> {
        let session = place.state.latest_session()?;
        if let Some(title) = session.title.clone().filter(|t| !t.trim().is_empty()) {
            return Some(title);
        }
        // A session still in flight has no snapshot yet, but the agent can be asked directly.
        if session.ended_at.is_none() {
            return plugins.agent_title(&session.agent, &place.path);
        }
        None
    }
}

/// Whether a tracker's own word for a status means the task is finished with.
///
/// Breq applies no vocabulary of its own anywhere else; this is the one place it has to guess,
/// because "is the task still open" is what decides whether its title still describes the work.
pub fn is_closed_status(status: &str) -> bool {
    const CLOSED: &[&str] = &[
        "closed",
        "done",
        "complete",
        "completed",
        "resolved",
        "merged",
        "shipped",
        "cancelled",
        "canceled",
        "abandoned",
        "wontfix",
        "duplicate",
    ];
    let normalized: String = status
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    CLOSED.contains(&normalized.as_str())
}

/// The user's login shell, for the last rung of the title chain.
fn shell_name() -> String {
    std::env::var("SHELL")
        .ok()
        .and_then(|s| {
            Path::new(&s)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "sh".to_string())
}

fn collect_sessions(place: &Place, plugins: &PluginManager) -> Vec<SessionInfo> {
    if !rmux::is_available() {
        return Vec::new();
    }
    let session = place.session_name();
    if !rmux::session_exists(&session) {
        return Vec::new();
    }

    let agent = place.agent();

    rmux::list_panes(&session)
        .unwrap_or_default()
        .into_iter()
        .map(|pane| {
            let status = pane.status();
            // Only the agent's own logs can tell "sitting at its prompt" from "working", and
            // only while the pane is actually alive.
            let agent_activity = match (&agent, status) {
                (Some(agent), PaneStatus::Running) if pane.window == rmux::AGENT_WINDOW => {
                    plugins.agent_activity(&agent.name, &place.path)
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
        age: None,
        error: None,
    })
}

/// What a task read keeps in the cache: the tracker-owned fields, and nothing derived.
fn task_cache_value(task: &TaskView) -> serde_json::Value {
    serde_json::json!({
        "title": task.title,
        "status": task.status,
        "assignee": task.assignee,
        "url": task.url,
    })
}

/// Render the workspace's tasks from cache, stamped with how old each read is.
fn cached_tasks(place: &Place, cache: &Cache) -> Vec<TaskView> {
    place
        .tasks()
        .iter()
        .filter_map(|link| {
            let mut view = task_view_stub(link)?;
            if let Some(entry) = cache.get(&task_cache_key(link)) {
                let field = |name: &str| {
                    entry
                        .value
                        .get(name)
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                };
                view.title = field("title");
                view.status = field("status");
                view.assignee = field("assignee");
                view.url = field("url");
                view.age = Some(entry.age_label());
            }
            Some(view)
        })
        .collect()
}

/// Write one freshly-read task through to the workspace cache. Best-effort by design: a cache
/// that will not write is a command that still succeeds.
pub fn cache_task(place: &Place, task: &crate::tasks::ResolvedTask) {
    if !place.exists() {
        return;
    }
    let link = crate::tasks::format_link(&task.source, &task.id);
    let mut cache = Cache::load(&place.path);
    cache.set(
        &task_cache_key(&link),
        serde_json::json!({
            "title": Some(task.title.clone()).filter(|t| !t.is_empty()),
            "status": task.status,
            "assignee": task.assignee,
            "url": task.url,
        }),
    );
    if let Err(e) = cache.save(&place.path) {
        tracing::warn!("failed to cache {}: {:#}", link, e);
    }
}

fn fetch_tasks(place: &Place, plugins: &PluginManager) -> Vec<TaskView> {
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
fn cached_delivery(cache: &Cache) -> (Vec<PrInfo>, Option<String>) {
    let Some(entry) = cache.get(DELIVERY_CACHE_KEY) else {
        return (Vec::new(), None);
    };
    let age = entry.age_label();
    let prs: Vec<PrInfo> = serde_json::from_value(entry.value).unwrap_or_default();
    (prs, Some(age))
}

/// Ask the delivery resolver. `None` means nobody was asked, which is different from an answer
/// of "no pull requests" — only an answer is worth writing through to the cache.
fn fetch_delivery(
    place: &Place,
    plugins: &PluginManager,
    config: &Config,
    branches: &[String],
) -> Option<Vec<PrInfo>> {
    let resolver = delivery_resolver(place, plugins, config)?;
    if branches.is_empty() {
        return None;
    }

    let values = match plugins.delivery_prs(&resolver, &place.path, branches) {
        Ok(values) => values,
        Err(e) => {
            tracing::warn!("delivery resolver '{}' failed: {:#}", resolver, e);
            return None;
        }
    };

    Some(
        values
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect(),
    )
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
    if let Some(delivery) = &place.state.delivery {
        return Some(delivery.resolver.clone());
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
    Sets::collect(
        place,
        ws_mgr,
        plugins,
        config,
        CollectOptions {
            tasks: false,
            ..CollectOptions::live()
        },
    )
    .prs
}

/// Whether a path looks like a workspace breq can read sets from.
pub fn is_workspace_dir(path: &Path) -> bool {
    path.join(".jj").exists() || path.join(".git").exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segments::Segment;
    use crate::state::{AgentSession, Cache};

    /// A place with no rmux session and no plugins, so only stored state answers anything.
    fn place(dir: &Path) -> Place {
        let segment = Segment {
            name: "demo".to_string(),
            path: dir.join("demo"),
        };
        let ws = dir.join("workspaces/demo/one");
        std::fs::create_dir_all(&ws).unwrap();
        Place::load(&segment, "one", ws, true)
    }

    fn plugins() -> PluginManager {
        PluginManager::new(Path::new("/nonexistent")).unwrap()
    }

    fn task(link: &str, status: Option<&str>, title: &str) -> TaskView {
        let (source, id) = crate::tasks::split_link(link).unwrap();
        TaskView {
            link: link.to_string(),
            source,
            id,
            title: Some(title.to_string()),
            status: status.map(|s| s.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn the_title_chain_prefers_an_open_task() {
        let dir = tempfile::tempdir().unwrap();
        let mut place = place(dir.path());
        place.state.add_task("runes", "tor-bau");
        place.state.add_task("runes", "tor-fe7");
        place.state.title = Some("stored".into());

        let sets = Sets {
            tasks: vec![
                task("runes:tor-bau", Some("in-progress"), "workflow revamp"),
                task("runes:tor-fe7", Some("todo"), "mirror rmux panes"),
            ],
            ..Default::default()
        };
        // The most recently linked one, not the first.
        assert_eq!(sets.title(&place, &plugins()), "mirror rmux panes");
    }

    #[test]
    fn a_closed_task_stops_speaking_for_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let mut place = place(dir.path());
        place.state.add_task("runes", "tor-bau");
        place.state.add_task("runes", "tor-fe7");
        place.state.prompt = Some("mirror the panes\nand more".into());

        let sets = Sets {
            tasks: vec![
                task("runes:tor-bau", Some("in-progress"), "workflow revamp"),
                task("runes:tor-fe7", Some("Done"), "mirror rmux panes"),
            ],
            ..Default::default()
        };
        assert_eq!(sets.title(&place, &plugins()), "workflow revamp");

        // With every task closed the chain keeps falling.
        let sets = Sets {
            tasks: vec![
                task("runes:tor-bau", Some("closed"), "workflow revamp"),
                task("runes:tor-fe7", Some("done"), "mirror rmux panes"),
            ],
            ..Default::default()
        };
        assert_eq!(sets.title(&place, &plugins()), "mirror the panes");
    }

    #[test]
    fn a_session_title_outranks_the_stored_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut place = place(dir.path());
        place.state.set_agent("claude", None);
        place.state.title = Some("do the thing".into());
        place.state.prompt = Some("do the thing".into());
        place.state.push_session(AgentSession {
            id: Some("3f2a".into()),
            agent: "claude".into(),
            ended_at: Some("2026-07-24T23:07:02Z".into()),
            title: Some("spike the pane mirror".into()),
            ..Default::default()
        });

        assert_eq!(sets_for(&place), "spike the pane mirror");

        // An unsettled session has no snapshot, so the stored title carries the row.
        place.state.agent.as_mut().unwrap().sessions[0].title = None;
        place.state.agent.as_mut().unwrap().sessions[0].ended_at = None;
        assert_eq!(sets_for(&place), "do the thing");
    }

    fn sets_for(place: &Place) -> String {
        Sets::default().title(place, &plugins())
    }

    #[test]
    fn a_workspace_that_was_only_ever_a_shell_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let place = place(dir.path());
        let title = Sets::default().title(&place, &plugins());
        assert!(title.ends_with(" shell"), "{}", title);
    }

    #[test]
    fn closed_statuses_ignore_the_trackers_spelling() {
        assert!(is_closed_status("Done"));
        assert!(is_closed_status("won't fix"));
        assert!(!is_closed_status("in_review"));
        assert!(!is_closed_status("in-progress"));
        assert!(!is_closed_status(""));
    }

    /// D17's rule, end to end: a live read stamps the cache, and the cached render says how old
    /// the stamp is without calling anything.
    #[test]
    fn task_reads_are_written_through_and_read_back_with_their_age() {
        let dir = tempfile::tempdir().unwrap();
        let mut place = place(dir.path());
        place.state.add_task("runes", "tor-fe7");

        crate::sets::cache_task(
            &place,
            &crate::tasks::ResolvedTask {
                id: "tor-fe7".into(),
                source: "runes".into(),
                kind: None,
                title: "mirror rmux panes".into(),
                status: Some("in-progress".into()),
                assignee: None,
                description: None,
                url: None,
                created_at: None,
                updated_at: None,
            },
        );

        let cache = Cache::load(&place.path);
        let views = cached_tasks(&place, &cache);
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].title.as_deref(), Some("mirror rmux panes"));
        assert_eq!(views[0].status.as_deref(), Some("in-progress"));
        assert_eq!(views[0].age.as_deref(), Some("now"));

        // And it is the cached read the title chain resolves against.
        let sets = Sets {
            tasks: views,
            ..Default::default()
        };
        assert_eq!(sets.title(&place, &plugins()), "mirror rmux panes");
    }

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
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(sets.task_summary(), "runes:tor-bau(in-progress)");

        // A cached read says how old it is, so nothing reads as fresher than it is.
        let stale = Sets {
            tasks: vec![TaskView {
                link: "runes:tor-bau".into(),
                status: Some("in-progress".into()),
                age: Some("3h".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(stale.task_summary(), "runes:tor-bau(in-progress 3h)");
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
