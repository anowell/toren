//! Taking a place down.
//!
//! Teardown is pure workspace deletion: destroy hooks, session, proxy routes, working copy.
//! It touches no tracker and pushes nothing. Shipping is a different axis — that's what the
//! `breq-complete` / `breq-submit` scripts are for — and keeping them separate is what lets a
//! workspace outlive the work it shipped.

use anyhow::Result;
use tracing::{info, warn};

use crate::history::{record_teardown, TeardownRecord};
use crate::place::Place;
use crate::process;
use crate::rmux;
use crate::workspace::{CleanupMode, WorkspaceManager};

/// How to tear a place down.
#[derive(Debug, Clone, Copy, Default)]
pub struct TeardownOptions {
    /// Kill processes and live panes instead of refusing.
    pub kill: bool,
    /// Keep the working copy and its VCS registration; drop only breq's own state.
    /// The exact inverse of adopting a working copy with an in-place `setup`.
    pub no_delete: bool,
}

/// What teardown did.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TeardownOutcome {
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
pub fn teardown(
    place: &Place,
    ws_mgr: &WorkspaceManager,
    opts: TeardownOptions,
) -> Result<TeardownOutcome> {
    guard_session(place, opts.kill)?;

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

    ws_mgr.teardown_workspace(
        &place.segment_path,
        &place.segment,
        &place.name,
        CleanupMode::Abort,
        !opts.no_delete,
    )?;

    if opts.no_delete {
        place.undecorate()?;
    }

    let record = TeardownRecord {
        uid: place.uid(),
        workspace: place.name.clone(),
        segment: place.segment.clone(),
        tasks: place.tasks(),
        revision: revision.clone(),
        torn_down_at: chrono::Utc::now().to_rfc3339(),
    };
    if let Err(e) = record_teardown(&record) {
        warn!("Failed to record teardown history: {:#}", e);
    }

    Ok(TeardownOutcome {
        workspace: place.name.clone(),
        segment: place.segment.clone(),
        uid: place.uid(),
        revision,
        deleted: !opts.no_delete,
    })
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
