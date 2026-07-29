//! Rendering the workspace join, at two zoom levels.
//!
//! `list` is a compact slice across every workspace; `get` is the same data in full for one.
//! Both are views over [`Sets`] — there is no second read model, and no derived judgment about
//! whether a workspace is "done". The drift you care about (task closed but workspace alive,
//! changes never pushed, agent long since idle) is meant to be *readable* here, not computed.

use anyhow::Result;
use colored::Colorize;
use serde_json::json;
use toren_lib::sets::{task_state, TaskState};
use toren_lib::{Place, PluginManager, Sets};

/// One row per workspace.
pub fn list(rows: &[(Place, Sets)], plugins: &PluginManager, show_segment: bool) {
    let term_width = terminal_size::terminal_size()
        .map(|(w, _)| w.0 as usize)
        .unwrap_or(100);

    let cells: Vec<Row> = rows
        .iter()
        .map(|(place, sets)| Row {
            name: if show_segment {
                format!("{}/{}", place.segment, place.name)
            } else {
                place.name.clone()
            },
            dirty: sets.has_changes(),
            adoptable: !place.is_decorated(),
            age: place.age_label(),
            agents: sets.agents(),
            changes: if sets.changes.is_empty() {
                "-".to_string()
            } else {
                sets.changes.len().to_string()
            },
            delivery: sets.delivery_summary(),
            tasks: sets.task_cells(),
            title: if place.is_decorated() {
                sets.title(place, plugins)
            } else {
                "(undecorated — `breq setup <name>` adopts it)".to_string()
            },
        })
        .collect();

    let w_name = width(cells.iter().map(|c| c.name.len() + 2), 9);
    let w_age = width(cells.iter().map(|c| c.age.len()), 3);
    let w_agents = width(cells.iter().map(|c| agent_cell(c.agents).1), 6);
    let w_changes = width(cells.iter().map(|c| c.changes.len()), 3);
    let w_delivery = width(cells.iter().map(|c| c.delivery.len()), 8);
    let w_tasks = width(cells.iter().map(|c| task_cell(&c.tasks).1), 5);

    // Two spaces between columns, one inside a cell: a task's glyph belongs to its id, and the
    // gap has to say so.
    let fixed = w_name + w_age + w_agents + w_changes + w_delivery + w_tasks + 12;
    let w_title = term_width.saturating_sub(fixed).max(10);

    println!(
        "{}",
        format!(
            "{:<w_name$}  {:<w_age$}  {:<w_agents$}  {:<w_changes$}  {:<w_delivery$}  {:<w_tasks$}  {}",
            "WORKSPACE",
            "AGE",
            "AGENTS",
            "CHG",
            "DELIVERY",
            "TASKS",
            "TITLE",
            w_name = w_name,
            w_age = w_age,
            w_agents = w_agents,
            w_changes = w_changes,
            w_delivery = w_delivery,
            w_tasks = w_tasks,
        )
        .dimmed()
    );

    for cell in &cells {
        let name = if cell.dirty {
            format!("{} *", cell.name)
        } else {
            cell.name.clone()
        };
        let name = format!("{:<w_name$}", name, w_name = w_name);
        let name = if cell.adoptable {
            name.dimmed()
        } else if cell.dirty {
            name.yellow()
        } else {
            name.normal()
        };

        let pad = |(text, width): (String, usize), to: usize| {
            format!("{}{}", text, " ".repeat(to.saturating_sub(width)))
        };
        let agents = pad(agent_cell(cell.agents), w_agents);
        let tasks = pad(task_cell(&cell.tasks), w_tasks);

        println!(
            "{}  {:<w_age$}  {}  {:<w_changes$}  {:<w_delivery$}  {}  {}",
            name,
            cell.age,
            agents,
            cell.changes,
            cell.delivery,
            tasks,
            truncate(&cell.title, w_title),
            w_age = w_age,
            w_changes = w_changes,
            w_delivery = w_delivery,
        );
    }
}

struct Row {
    name: String,
    dirty: bool,
    adoptable: bool,
    age: String,
    agents: Option<(usize, bool)>,
    changes: String,
    delivery: String,
    tasks: Vec<(Option<TaskState>, String)>,
    title: String,
}

/// A task as a row shows it: how far along, then which one, both carrying the colour so the
/// state reads without having to resolve the glyph.
///
/// A link whose status has never been read gets no glyph rather than a guessed one.
fn task_token(state: Option<TaskState>, id: &str) -> String {
    match state {
        Some(TaskState::Closed) => format!("{} {}", TaskState::Closed.glyph(), id)
            .green()
            .to_string(),
        Some(TaskState::Wip) => format!("{} {}", TaskState::Wip.glyph(), id)
            .yellow()
            .to_string(),
        Some(TaskState::Todo) => format!("{} {}", TaskState::Todo.glyph(), id),
        None => id.dimmed().to_string(),
    }
}

/// What [`task_token`] takes up on screen: the glyph and the space after it, or neither.
fn task_token_width(state: Option<TaskState>, id: &str) -> usize {
    id.chars().count() + if state.is_some() { 2 } else { 0 }
}

/// How many tasks a row spells out before falling back to `+N`.
const TASKS_SHOWN: usize = 2;

/// The agent column: how many are live in there, and whether any is mid-turn.
fn agent_cell(agents: Option<(usize, bool)>) -> (String, usize) {
    match agents {
        None => ("-".to_string(), 1),
        Some((count, busy)) => {
            let mark = if busy {
                "◐".yellow().to_string()
            } else {
                "○".to_string()
            };
            (format!("{}{}", mark, count), 1 + count.to_string().len())
        }
    }
}

/// The task column, with the width it *looks* — colour codes and the multi-byte glyphs both
/// make `len()` the wrong number to pad by.
fn task_cell(tasks: &[(Option<TaskState>, String)]) -> (String, usize) {
    if tasks.is_empty() {
        return ("-".to_string(), 1);
    }
    let shown = &tasks[..tasks.len().min(TASKS_SHOWN)];
    let overflow = tasks.len() - shown.len();

    let mut text: Vec<String> = shown
        .iter()
        .map(|(state, id)| task_token(*state, id))
        .collect();
    let mut width = shown
        .iter()
        .map(|(state, id)| task_token_width(*state, id))
        .sum::<usize>()
        + shown.len()
        - 1;

    if overflow > 0 {
        let more = format!("+{}", overflow);
        width += 1 + more.chars().count();
        text.push(more.dimmed().to_string());
    }
    (text.join(" "), width)
}

fn width(lens: impl Iterator<Item = usize>, min: usize) -> usize {
    lens.max().unwrap_or(min).max(min)
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    if max <= 3 {
        return text.chars().take(max).collect();
    }
    let head: String = text.chars().take(max - 1).collect();
    format!("{}…", head)
}

/// How long ago the agent last wrote for this session, if it can be attributed to it.
///
/// An agent reports activity per *directory*, not per session, so the answer only belongs to the
/// session it currently considers current. An older record with no ending was left that way by
/// something that could not observe it, and nothing here can date it.
fn last_wrote(
    place: &Place,
    plugins: &PluginManager,
    session: &toren_lib::state::AgentSession,
) -> Option<String> {
    let id = session.id.as_deref()?;
    let current = plugins.agent_session_id(&session.agent, &place.path)?;
    if current != id {
        return None;
    }
    let secs = plugins.agent_last_activity(&session.agent, &place.path)?;
    Some(format!("{} ago", brief_duration(secs)))
}

/// `4d`, `3h`, `5m`, `20s` — the coarsest unit that is not zero.
fn brief_duration(secs: i64) -> String {
    match secs {
        s if s >= 86_400 => format!("{}d", s / 86_400),
        s if s >= 3_600 => format!("{}h", s / 3_600),
        s if s >= 60 => format!("{}m", s / 60),
        s => format!("{}s", s.max(0)),
    }
}

/// Every set, in full, for one workspace.
pub fn detail(place: &Place, sets: &Sets, plugins: &PluginManager) {
    let header = match place.uid() {
        Some(uid) => format!("{}  {}  {}", place.name, place.segment, uid),
        None => format!("{}  {}  (undecorated)", place.name, place.segment),
    };
    println!("{}", header.bold());

    field("path", &place.path.display().to_string());
    field("title", &sets.title(place, plugins));
    if let Some(base) = place.base() {
        field("base", &base);
    }
    if let Some(parent) = place.parent() {
        field("parent", &parent);
    }
    field(
        "created",
        &place.created_label().unwrap_or_else(|| "-".to_string()),
    );
    // "session" alone read as any of three things, and this is the rmux one — the session that
    // holds the shell and agent windows below, not a window itself.
    field("rmux session", &place.session_name());
    if !place.vcs_tracked {
        field("vcs", "untracked (prunable with `breq cleanup`)");
    }

    // Every pane is a session, which said nothing about which ones you can talk to and which
    // ones are working. Split by that instead.
    let (agents, shells): (Vec<_>, Vec<_>) = sets
        .sessions
        .iter()
        .partition(|s| s.window == toren_lib::rmux::AGENT_WINDOW);

    // Both are rmux panes, which is what makes a pane id meaningful here and nowhere below.
    // Padded before it is dimmed: the escape codes are not columns.
    let pane_of = |session: &toren_lib::sets::SessionInfo| {
        format!("{:<5}", session.pane).dimmed().to_string()
    };

    section("shells", shells.len());
    for session in &shells {
        println!(
            "  {:<8} {} {:<8} {}",
            session.window,
            pane_of(session),
            session.status,
            session.command
        );
    }

    section("agents", agents.len());
    for session in &agents {
        let activity = match &session.agent_activity {
            Some(activity) => format!("  {}", activity),
            None => String::new(),
        };
        println!(
            "  {:<8} {} {:<8} {}{}",
            session.window,
            pane_of(session),
            session.status,
            session.command,
            activity
        );
    }

    section("changes", sets.changes.len());
    for commit in &sets.changes {
        println!("  {} {}", commit.id, commit.summary);
    }

    section("branches", sets.branches.len());
    for branch in &sets.branches {
        println!("  {}", branch);
    }

    let pr_label = match &sets.prs_age {
        Some(age) => format!("pull requests (cached {})", age),
        None => "pull requests".to_string(),
    };
    section(&pr_label, sets.prs.len());
    for pr in &sets.prs {
        let ci = if pr.ci.is_empty() {
            String::new()
        } else {
            format!("  ci:{}", pr.ci)
        };
        println!("  {:<6} {:<8}{}  {}", pr.id, pr.state, ci, pr.url);
    }

    section("tasks", sets.tasks.len());
    for task in &sets.tasks {
        match &task.error {
            Some(error) => println!(
                "  {}  {}",
                task.link,
                format!("unreadable: {}", error).red()
            ),
            None => {
                let age = match task.age.as_deref() {
                    Some("now") => "  (just read)".to_string(),
                    Some(age) => format!("  (read {} ago)", age),
                    None => String::new(),
                };
                println!(
                    "  {}  {:<12} {}{}",
                    task_token(task.status.as_deref().map(task_state), &task.link),
                    task.status.as_deref().unwrap_or("-"),
                    task.title.as_deref().unwrap_or(""),
                    age.dimmed()
                );
            }
        }
    }

    // The provenance list `breq do --resume <id>` reads from.
    let sessions = place.state.sessions();
    section("agent sessions", sessions.len());
    for session in sessions.iter().rev() {
        let state = match (&session.ended_at, session.exit) {
            (Some(_), Some(code)) => format!("exited {}", code),
            (Some(_), None) => "ended".to_string(),
            // Nothing closed this one out. Whether it is still open is unknowable without a pane
            // to watch, so say the one true thing instead: when the agent last wrote.
            (None, _) => last_wrote(place, plugins, session).unwrap_or_else(|| "-".to_string()),
        };
        let origin = if session.external {
            "  (not breq's)".dimmed().to_string()
        } else {
            String::new()
        };
        println!(
            "  {:<38} {:<8} {:<10} {}{}",
            session.id.as_deref().unwrap_or("(pending)"),
            session.agent,
            state,
            session.title.as_deref().unwrap_or(""),
            origin
        );
    }

    let extra_keys = place.state.extra_keys();
    if !extra_keys.is_empty() {
        section("extra", extra_keys.len());
        for key in extra_keys {
            let value = place.state.get_field(key).unwrap_or_default().join(", ");
            println!("  {:<20} {}", key, value);
        }
    }
}

/// The same detail as JSON, for scripts that want the whole join at once.
pub fn detail_json(place: &Place, sets: &Sets, plugins: &PluginManager) -> Result<String> {
    let value = json!({
        "name": place.name,
        "segment": place.segment,
        "uid": place.uid(),
        "path": place.path,
        "title": sets.title(place, plugins),
        "base": place.base(),
        "parent": place.parent(),
        "decorated": place.is_decorated(),
        "vcs_tracked": place.vcs_tracked,
        "state": place.state,
        "sets": sets,
    });
    Ok(serde_json::to_string_pretty(&value)?)
}

fn field(name: &str, value: &str) {
    println!("  {:<10} {}", name.dimmed(), value);
}

fn section(name: &str, count: usize) {
    println!();
    if count == 0 {
        println!("{}", format!("{} (none)", name).dimmed());
    } else {
        println!("{}", format!("{} ({})", name, count).dimmed());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_marks_elision() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a much longer title", 8), "a much …");
        assert_eq!(truncate("abcdef", 2), "ab");
    }

    #[test]
    fn width_respects_a_minimum() {
        assert_eq!(width([1, 2, 3].into_iter(), 5), 5);
        assert_eq!(width([9, 2].into_iter(), 5), 9);
        assert_eq!(width(std::iter::empty(), 4), 4);
    }

    /// The reported width is what pads the column, and it has to match what the cell *looks*
    /// like — never the byte length, which colour codes and the glyphs both inflate.
    #[test]
    fn a_cells_width_is_what_it_looks_not_what_it_weighs() {
        let task = |state, id: &str| (Some(state), id.to_string());

        assert_eq!(task_cell(&[]), ("-".to_string(), 1));

        // glyph, space, id
        let (_, w) = task_cell(&[task(TaskState::Todo, "tor-1")]);
        assert_eq!(w, 7);

        // two of them, one space between
        let (_, w) = task_cell(&[
            task(TaskState::Wip, "tor-1"),
            task(TaskState::Todo, "tor-2"),
        ]);
        assert_eq!(w, 15);

        // past the cap the rest collapse into "+N"
        let (text, w) = task_cell(&[
            task(TaskState::Wip, "tor-1"),
            task(TaskState::Todo, "tor-2"),
            task(TaskState::Closed, "tor-3"),
            task(TaskState::Closed, "tor-4"),
        ]);
        assert!(text.contains("+2"), "{}", text);
        assert!(!text.contains("tor-3"), "{}", text);
        assert_eq!(w, 18);

        // a link never read gets no glyph, so it is a character narrower
        let (_, w) = task_cell(&[(None, "tor-1".to_string())]);
        assert_eq!(w, 5);

        assert_eq!(agent_cell(None), ("-".to_string(), 1));
        assert_eq!(agent_cell(Some((1, true))).1, 2);
        assert_eq!(agent_cell(Some((12, false))).1, 3);
    }
}
