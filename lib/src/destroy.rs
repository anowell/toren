//! Taking a place down.
//!
//! Pure workspace deletion: destroy hooks, session, proxy routes, working copy.
//! It touches no tracker and pushes nothing. Shipping is a different axis — that's what the
//! `breq-complete` / `breq-submit` scripts are for — and keeping them separate is what lets a
//! workspace outlive the work it shipped.

use anyhow::Result;
use tracing::{info, warn};

use crate::place::Place;
use crate::plugins::PluginManager;
use crate::process;
use crate::rmux;
use crate::workspace::{CleanupMode, WorkspaceManager};

/// How to tear a place down.
#[derive(Debug, Clone, Copy, Default)]
pub struct DestroyOptions {
    /// Kill processes and live panes instead of refusing.
    pub kill: bool,
    /// Keep the working copy and its VCS registration; drop only breq's own state.
    /// The exact inverse of adopting a working copy with an in-place `setup`.
    pub no_delete: bool,
}

/// What destroy did.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DestroyOutcome {
    pub workspace: String,
    pub segment: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// False when `--no-delete` kept the working copy.
    pub deleted: bool,
}

/// Tear down a place.
///
/// Refuses while anything is running in it unless `kill` is set — a live agent is work in
/// progress, and this is the only thing standing between it and `rm -rf`.
pub fn destroy(
    place: &Place,
    ws_mgr: &WorkspaceManager,
    plugins: &PluginManager,
    opts: DestroyOptions,
) -> Result<DestroyOutcome> {
    guard_session(place, opts.kill)?;

    // Asked while the working copy is still there, because that path is how an agent finds its
    // own session. Afterwards nothing can answer the question.
    let (agent, session_id) = agent_provenance(place, plugins);

    // The session always holds an idle shell sitting in the workspace, which would otherwise
    // trip the process check forever — so it has to come down, but only after the guard above
    // has had its say. Stale incarnations go too: nothing is left holding this directory.
    kill_sessions(place);

    if place.exists() {
        let processes = process::find_workspace_processes(&place.path);
        if !processes.is_empty() {
            if opts.kill {
                info!("Terminating {} process(es) in workspace", processes.len());
                process::terminate_processes(&processes, std::time::Duration::from_secs(5))?;
            } else {
                return Err(process::WorkspaceProcessesRunning { processes }.into());
            }
        }
    }

    let revision = if place.exists() {
        ws_mgr.capture_revision(&place.segment_path, &place.path)
    } else {
        None
    };

    ws_mgr.destroy_workspace(
        &place.segment_path,
        &place.segment,
        &place.name,
        CleanupMode::Abort,
        !opts.no_delete,
    )?;

    if opts.no_delete {
        place.undecorate()?;
    }

    // The last thing said about this incarnation. The agent's own session file is the record of
    // what was done here; this is the line that ties the two together once the workspace is gone.
    info!(
        event = "workspace.destroy",
        segment = %place.segment,
        workspace = %place.name,
        uid = place.uid(),
        tasks = ?place.tasks(),
        revision = revision.as_deref(),
        agent = agent.as_deref(),
        session_id = session_id.as_deref(),
        deleted = !opts.no_delete,
        "Tore down '{}'",
        place.name
    );

    Ok(DestroyOutcome {
        workspace: place.name.clone(),
        segment: place.segment.clone(),
        uid: place.uid(),
        revision,
        deleted: !opts.no_delete,
    })
}

/// The agent that worked here and the id of the session it kept its own record under.
///
/// Nothing else links an incarnation to the agent's transcript of it, and both sides of that link
/// are gone the moment the workspace is: the workspace's state with `<ws>/.toren/`, and the
/// agent's answer with the path it keys its sessions by.
fn agent_provenance(place: &Place, plugins: &PluginManager) -> (Option<String>, Option<String>) {
    let Some(agent) = place.agent() else {
        return (None, None);
    };
    // The workspace's own record is the cheaper and more specific answer; the agent is asked
    // only for a workspace that predates the list, or whose last session never got an id.
    let session_id = place
        .state
        .latest_session()
        .and_then(|s| s.id.clone())
        .or_else(|| plugins.agent_session_id(&agent.name, &place.path));
    (Some(agent.name), session_id)
}

/// Refuse to proceed if the place's session holds live work.
pub fn guard_session(place: &Place, kill: bool) -> Result<()> {
    if kill || !rmux::is_available() {
        return Ok(());
    }
    let session = place.session_name();
    if !rmux::session_exists(&session) {
        return Ok(());
    }

    let busy = rmux::busy_panes(&session);
    if busy.is_empty() {
        return Ok(());
    }

    let processes = busy
        .into_iter()
        .map(|pane| process::ProcessInfo {
            pid: pane.pid,
            name: format!("{} in rmux {}:{}", pane.command, session, pane.window),
        })
        .collect();

    Err(process::WorkspaceProcessesRunning { processes }.into())
}

/// Kill this incarnation's session, and any left over from earlier ones.
fn kill_sessions(place: &Place) {
    if !rmux::is_available() {
        return;
    }
    if let Err(e) = rmux::kill_session(&place.session_name()) {
        warn!("Failed to kill rmux session: {:#}", e);
    }
    rmux::reconcile(&place.segment, &place.name, place.uid().as_deref());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentSpec;
    use crate::segments::Segment;
    use std::path::Path;

    fn place_with(agent: Option<&str>) -> Place {
        let dir = tempfile::tempdir().unwrap();
        let segment = Segment {
            name: "toren".into(),
            path: dir.path().to_path_buf(),
        };
        let mut place = Place::load(&segment, "one", dir.path().join("one"), true);
        if let Some(agent) = agent {
            let spec = AgentSpec::parse(agent);
            place.state.set_agent(&spec.name, spec.model.as_deref());
        }
        place
    }

    /// No plugins installed, so the session id is unanswerable — the agent name still is.
    fn plugins() -> PluginManager {
        PluginManager::new(Path::new("/nonexistent")).unwrap()
    }

    #[test]
    fn provenance_drops_the_model_from_the_recorded_agent() {
        let (agent, _) = agent_provenance(&place_with(Some("claude:opus")), &plugins());
        assert_eq!(agent.as_deref(), Some("claude"));
    }

    #[test]
    fn a_place_that_never_ran_an_agent_has_no_provenance() {
        let (agent, session_id) = agent_provenance(&place_with(None), &plugins());
        assert!(agent.is_none());
        assert!(session_id.is_none());
    }
}
