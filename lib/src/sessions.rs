//! Agent session provenance: which agent session ran in which workspace incarnation.
//!
//! Toren already asks an agent for its session id to build `--resume` argv and then throws the
//! answer away, so nothing links a workspace to the sessions worked in it. This module keeps
//! that link in `state.json` — a handful of records per workspace lifetime, enough to resume a
//! *specific* session and enough to title the workspace after one.
//!
//! Both front-ends record through here: `breq do` and the daemon's start endpoint.
//!
//! **When the id is knowable.** A resume already names its session, so its record carries the id
//! from the start. A fresh run does not: the plugin's `session_id` reads the agent's newest
//! session file, which at spawn time is still the *previous* run's. So a fresh record opens
//! without an id and [`settle`] fills it in once the pane is gone and the agent's own file is the
//! answer — which is also when its title and exit status can be snapshotted.
//!
//! Nothing watches a pane for its death, so settling happens whenever something next looks at the
//! workspace closely enough to have asked anyway: stopping an agent, rendering one workspace, or
//! starting the next session.

use anyhow::Result;

use crate::place::Place;
use crate::plugins::PluginManager;
use crate::rmux;
use crate::state::{AgentSession, TaskLink};

/// Record the session an agent start is about to open, closing out whatever preceded it.
///
/// `session_id` is the session being resumed, when one is named; a fresh run passes `None`.
/// Resuming a session breq already recorded reopens that record rather than duplicating it, so
/// the list stays one entry per session rather than one per attach.
pub fn record_start(
    place: &mut Place,
    plugins: &PluginManager,
    agent: &str,
    session_id: Option<&str>,
) -> Result<()> {
    settle(place, plugins);

    // A new agent supersedes anything settle could not close: whatever was open is over.
    let now = chrono::Utc::now().to_rfc3339();
    if let Some(agent_state) = place.state.agent.as_mut() {
        for session in agent_state.sessions.iter_mut() {
            if session.ended_at.is_none() {
                session.ended_at = Some(now.clone());
            }
        }
    }

    let task = place.state.primary_task().map(TaskLink::link);
    let resumed = session_id.and_then(|id| place.state.take_session(id));

    let session = match resumed {
        Some(previous) => AgentSession {
            ended_at: None,
            exit: None,
            task: previous.task.clone().or(task),
            ..previous
        },
        None => AgentSession {
            id: session_id.map(|id| id.to_string()),
            agent: agent.to_string(),
            started_at: Some(now),
            task,
            ..Default::default()
        },
    };
    tracing::info!(
        event = if session_id.is_some() {
            "agent.resume"
        } else {
            "agent.start"
        },
        segment = %place.segment,
        workspace = %place.name,
        uid = place.uid(),
        agent,
        session_id,
        task = session.task.as_deref(),
        "{} {} in '{}'",
        if session_id.is_some() {
            "Resuming"
        } else {
            "Starting"
        },
        agent,
        place.name
    );

    place.state.push_session(session);
    place.save()
}

/// What the workspace's rmux session says about the agent working in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Liveness {
    /// A live pane: the agent is still working.
    Running,
    /// The pane is dead, or its window is gone. Nothing is running there.
    Ended(Option<i32>),
    /// Nothing to read. rmux is unavailable, or the agent was never hosted in it — a direct
    /// child (`--no-rmux`) leaves no session behind to ask.
    Unknown,
}

fn liveness(session: &str) -> Liveness {
    if !rmux::is_available() {
        return Liveness::Unknown;
    }
    match rmux::agent_pane(session) {
        Some(pane) if !pane.dead => Liveness::Running,
        Some(pane) => Liveness::Ended(pane.exit),
        // No agent window in a session that exists is a pane already dismissed; no session at
        // all is a workspace rmux never hosted, and nothing to conclude from.
        None if rmux::session_exists(session) => Liveness::Ended(None),
        None => Liveness::Unknown,
    }
}

/// Snapshot what an ended session left behind: its id, the agent's title for it, its exit status.
///
/// Best-effort and idempotent — a live agent settles nothing, and neither does one nothing can
/// answer for: closing a session out mid-run would stamp a half-written title on it. Returns
/// whether anything changed.
pub fn settle(place: &mut Place, plugins: &PluginManager) -> bool {
    if place.state.open_session_mut().is_none() {
        return false;
    }
    let liveness = liveness(&place.session_name());
    settle_with(place, plugins, liveness)
}

fn settle_with(place: &mut Place, plugins: &PluginManager, liveness: Liveness) -> bool {
    let Liveness::Ended(exit) = liveness else {
        return false;
    };
    let Some(open) = place.state.open_session_mut() else {
        return false;
    };
    let agent = open.agent.clone();
    let known_id = open.id.clone();

    // The newest session file is this session's only once the agent has stopped writing it.
    let id = known_id.clone().or_else(|| {
        plugins
            .agent_session_id(&agent, &place.path)
            .filter(|id| place.state.session(id).is_none())
    });
    let title = plugins.agent_title(&agent, &place.path);

    let Some(open) = place.state.open_session_mut() else {
        return false;
    };
    // Only ever add to what a previous settle knew: a resumed session keeps the title and status
    // it earned last time when the agent has nothing new to say.
    open.id = id;
    open.exit = exit.or(open.exit);
    open.title = title.or_else(|| open.title.take());
    open.ended_at = Some(chrono::Utc::now().to_rfc3339());

    let (id, exit) = (open.id.clone(), open.exit);
    tracing::info!(
        event = "agent.exit",
        segment = %place.segment,
        workspace = %place.name,
        uid = place.uid(),
        agent = %agent,
        session_id = id,
        exit,
        "{} finished in '{}'",
        agent,
        place.name
    );
    true
}

/// [`settle`], persisted. Best-effort in both halves: a workspace that will not take the write
/// keeps its record in memory and settles again next time something looks.
pub fn settle_saved(place: &mut Place, plugins: &PluginManager) {
    if !settle(place, plugins) {
        return;
    }
    if let Err(e) = place.save() {
        tracing::warn!(
            "failed to record the end of the agent session in '{}': {:#}",
            place.name,
            e
        );
    }
}

/// Which session `breq do --resume [<id>]` should continue.
///
/// A named id is taken at its word — the agent knows sessions toren never recorded, so an
/// unrecorded one is a warning rather than a refusal. A bare `--resume` takes the workspace's
/// most recent recorded session, and falls back to whatever the agent itself calls current.
pub fn resume_target(
    place: &Place,
    plugins: &PluginManager,
    agent: &str,
    requested: Option<&str>,
) -> Option<String> {
    if let Some(id) = requested {
        if place.state.session(id).is_none() {
            tracing::warn!(
                "Session '{}' is not recorded in workspace '{}'; asking {} for it anyway",
                id,
                place.name,
                agent
            );
        }
        return Some(id.to_string());
    }
    place
        .state
        .latest_session()
        .and_then(|s| s.id.clone())
        .or_else(|| plugins.agent_session_id(agent, &place.path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::place::PlaceRegistry;
    use crate::segments::Segment;
    use std::path::Path;

    /// A place in a tempdir. Its uid is fresh, so no rmux session can exist for it and every
    /// pane lookup below reads as [`Liveness::Unknown`] — the tests that need a finished agent
    /// hand [`settle_with`] the liveness they mean.
    fn place(root: &Path) -> Place {
        let ws_root = root.join("workspaces");
        std::fs::create_dir_all(ws_root.join("demo/one")).unwrap();

        let mut config = Config::default();
        config.ancillaries.workspace_root = ws_root;
        config.ancillaries.segments = vec![];
        let registry = PlaceRegistry::new(&config).unwrap();
        let segment = Segment {
            name: "demo".to_string(),
            path: root.join("demo"),
        };
        let mut place = registry.get(&segment, "one");
        place.initialize(None, None).unwrap();
        place
    }

    fn plugins() -> PluginManager {
        PluginManager::new(Path::new("/nonexistent")).unwrap()
    }

    #[test]
    fn a_fresh_start_records_a_session_with_no_id_yet() {
        let dir = tempfile::tempdir().unwrap();
        let mut place = place(dir.path());
        place.state.set_agent("claude", Some("opus"));
        place.state.add_task("runes", "tor-fe7");

        record_start(&mut place, &plugins(), "claude", None).unwrap();

        let session = place.state.latest_session().unwrap();
        assert!(session.id.is_none(), "no id until the agent writes one");
        assert_eq!(session.agent, "claude");
        assert_eq!(session.task.as_deref(), Some("runes:tor-fe7"));
        assert!(session.started_at.is_some());
        assert!(session.ended_at.is_none());

        // And it survives the round trip through disk.
        let reloaded = crate::state::WorkspaceState::load(&place.path, None).unwrap();
        assert_eq!(reloaded.sessions().len(), 1);
    }

    #[test]
    fn settling_closes_the_open_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut place = place(dir.path());
        place.state.set_agent("claude", None);
        record_start(&mut place, &plugins(), "claude", None).unwrap();

        assert!(settle_with(
            &mut place,
            &plugins(),
            Liveness::Ended(Some(0))
        ));
        let session = place.state.latest_session().unwrap();
        assert!(session.ended_at.is_some());
        assert_eq!(session.exit, Some(0));
        // Idempotent: a closed session settles no further.
        assert!(!settle_with(
            &mut place,
            &plugins(),
            Liveness::Ended(Some(0))
        ));
    }

    #[test]
    fn an_agent_nothing_can_answer_for_stays_open() {
        let dir = tempfile::tempdir().unwrap();
        let mut place = place(dir.path());
        place.state.set_agent("claude", None);
        record_start(&mut place, &plugins(), "claude", None).unwrap();

        // A live pane declines, and so does a workspace with no pane to read — an agent run
        // outside rmux is still running for all this knows.
        assert!(!settle_with(&mut place, &plugins(), Liveness::Running));
        assert!(!settle_with(&mut place, &plugins(), Liveness::Unknown));
        assert!(!settle(&mut place, &plugins()));
        assert!(place.state.latest_session().unwrap().ended_at.is_none());
    }

    #[test]
    fn settling_a_resumed_session_keeps_what_the_first_run_left() {
        let dir = tempfile::tempdir().unwrap();
        let mut place = place(dir.path());
        place.state.set_agent("claude", None);
        place.state.push_session(AgentSession {
            id: Some("3f2a".into()),
            agent: "claude".into(),
            ended_at: Some("2026-07-24T23:07:02Z".into()),
            exit: Some(0),
            title: Some("spike the pane mirror".into()),
            ..Default::default()
        });

        record_start(&mut place, &plugins(), "claude", Some("3f2a")).unwrap();
        // These agents answer nothing, so a second settle has only the snapshot to go on.
        assert!(settle_with(&mut place, &plugins(), Liveness::Ended(None)));
        place.save().unwrap();

        let session = place.state.latest_session().unwrap();
        assert_eq!(session.title.as_deref(), Some("spike the pane mirror"));
        // The exit status is the *run's*, not the session's, so resuming clears it and this
        // run's pane is long gone.
        assert!(session.exit.is_none());

        let reloaded = crate::state::WorkspaceState::load(&place.path, None).unwrap();
        assert!(reloaded.latest_session().unwrap().ended_at.is_some());
    }

    #[test]
    fn a_resume_reopens_the_recorded_session_rather_than_duplicating_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut place = place(dir.path());
        place.state.set_agent("claude", None);
        place.state.push_session(AgentSession {
            id: Some("3f2a".into()),
            agent: "claude".into(),
            started_at: Some("2026-07-24T22:07:02Z".into()),
            ended_at: Some("2026-07-24T23:07:02Z".into()),
            exit: Some(0),
            title: Some("spike the pane mirror".into()),
            task: None,
        });
        place.state.push_session(AgentSession {
            id: Some("aa11".into()),
            agent: "claude".into(),
            ended_at: Some("2026-07-25T23:07:02Z".into()),
            ..Default::default()
        });

        record_start(&mut place, &plugins(), "claude", Some("3f2a")).unwrap();

        let sessions = place.state.sessions();
        assert_eq!(sessions.len(), 2, "reopened, not duplicated");
        let latest = place.state.latest_session().unwrap();
        assert_eq!(latest.id.as_deref(), Some("3f2a"));
        assert!(latest.ended_at.is_none(), "live again");
        assert!(latest.exit.is_none());
        // The snapshot from last time is what the title chain still reads.
        assert_eq!(latest.title.as_deref(), Some("spike the pane mirror"));
        assert_eq!(
            latest.started_at.as_deref(),
            Some("2026-07-24T22:07:02Z"),
            "the session began when it began"
        );
    }

    #[test]
    fn a_new_start_closes_a_session_left_open() {
        let dir = tempfile::tempdir().unwrap();
        let mut place = place(dir.path());
        place.state.set_agent("claude", None);
        record_start(&mut place, &plugins(), "claude", None).unwrap();
        record_start(&mut place, &plugins(), "claude", None).unwrap();

        let sessions = place.state.sessions();
        assert_eq!(sessions.len(), 2);
        assert!(sessions[0].ended_at.is_some());
        assert!(sessions[1].ended_at.is_none());
    }

    #[test]
    fn resume_targets_the_most_recent_recorded_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut place = place(dir.path());
        place.state.set_agent("claude", None);
        place.state.push_session(AgentSession {
            id: Some("3f2a".into()),
            agent: "claude".into(),
            ..Default::default()
        });
        place.state.push_session(AgentSession {
            id: Some("aa11".into()),
            agent: "claude".into(),
            ..Default::default()
        });

        let plugins = plugins();
        assert_eq!(
            resume_target(&place, &plugins, "claude", None).as_deref(),
            Some("aa11")
        );
        assert_eq!(
            resume_target(&place, &plugins, "claude", Some("3f2a")).as_deref(),
            Some("3f2a")
        );
        // An id breq never recorded is still the user's to ask for.
        assert_eq!(
            resume_target(&place, &plugins, "claude", Some("unknown")).as_deref(),
            Some("unknown")
        );
    }
}
