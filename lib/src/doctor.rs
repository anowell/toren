//! `breq doctor`: detect known-bad state, apply the known fix.
//!
//! Every check pairs a detection with a repair, and nothing here ever runs implicitly — normal
//! commands never migrate anything behind your back. That's the whole reason this verb exists:
//! migrations and repairs happen when you ask, once, visibly.
//!
//! Individual checks come and go as old state stops existing. `doctor` itself stays as the
//! standing home for them.

use anyhow::Result;
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::config::{toren_root, Config};
use crate::mux as rmux;
use crate::place::PlaceRegistry;
use crate::plugins::{PluginContext, PluginManager};
use crate::scripts;
use crate::state;

/// What one check found and, if fixing, what it did about it.
#[derive(Debug, Default)]
pub struct CheckReport {
    pub name: String,
    /// One line per problem found.
    pub findings: Vec<String>,
    /// One line per repair applied.
    pub fixed: Vec<String>,
    /// Something the user has to do themselves.
    pub advice: Option<String>,
}

impl CheckReport {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    pub fn is_clean(&self) -> bool {
        self.findings.is_empty() && self.advice.is_none()
    }
}

/// Run every check. With `fix`, apply repairs; without, only report.
pub fn run(config: &Config, plugins: &PluginManager, fix: bool) -> Result<Vec<CheckReport>> {
    Ok(vec![
        check_mux(config)?,
        check_legacy_assignments(config, plugins, fix)?,
        check_shipped_scripts(fix)?,
        check_stale_sessions(config, fix)?,
        check_toren_excluded(config, fix)?,
        check_retired_history()?,
    ])
}

fn check_mux(config: &Config) -> Result<CheckReport> {
    let status = crate::mux::status(config.mux.as_ref(), crate::mux::MuxOverride::default())?;
    let args = if status.args.is_empty() {
        String::new()
    } else {
        format!(" {}", status.args.join(" "))
    };
    let mut report = CheckReport::new(format!("mux: {} ({})", status.name, status.source));

    if status.name != crate::mux::Mux::None && !status.available {
        let command = status.command.as_deref().unwrap_or(status.name.name());
        report
            .findings
            .push(format!("{}{} is not available on PATH", command, args));
    }
    if status.name != crate::mux::Mux::None && !status.held_panes {
        report
            .findings
            .push("this mux cannot report held-pane exit status".into());
    }

    Ok(report)
}

/// `~/.toren/completion_history.jsonl` — the destroy record the rolling log replaced.
///
/// Reported, never read: it holds two incompatible schemas with no discriminator, and nothing
/// ever consumed it. `--fix` deliberately leaves it alone — whatever it says about workspaces
/// that no longer exist is not toren's to delete.
fn check_retired_history() -> Result<CheckReport> {
    let mut report = CheckReport::new("retired completion history");
    let path = retired_history_path();
    if !path.exists() {
        return Ok(report);
    }

    let shown = crate::config::tilde_shorten(&path);
    report
        .findings
        .push(format!("{} is no longer written or read", shown));
    report.advice = Some(format!(
        "destroys now land in {}; delete {} once you are done with it",
        crate::config::tilde_shorten(&crate::logging::log_dir()),
        shown
    ));

    Ok(report)
}

/// The line this check keeps in each segment's local exclude.
const TOREN_EXCLUDE: &str = ".toren/";

/// `.toren/` is machine-local state, so it must never be committed.
///
/// Nothing structurally prevents that — a daemon that once created `.toren/commands/` relative to
/// its cwd got six YAML files committed to this repo before anyone noticed. The guard is the
/// repo's local exclude, which keeps the pattern out of everyone else's clone.
///
/// Note this only stops *new* commits: an exclude never untracks a path that is already tracked,
/// so a repo that already carries a committed `.toren/` needs a deletion commit as well.
fn check_toren_excluded(config: &Config, fix: bool) -> Result<CheckReport> {
    let mut report = CheckReport::new("toren excluded from segments");
    let registry = PlaceRegistry::new(config)?;

    for segment in registry.segments.list_all() {
        let exclude = segment.path.join(".git/info/exclude");
        // No `.git` means nothing to exclude into — a jj repo without git colocation would need
        // its own ignore file, which is rare enough to leave to the person who hits it.
        let Some(parent) = exclude.parent() else {
            continue;
        };
        if !segment.path.join(".git").exists() {
            continue;
        }

        if exclude_covers_toren(&exclude) {
            continue;
        }

        report
            .findings
            .push(format!("{} does not exclude .toren/", segment.name));

        if fix {
            std::fs::create_dir_all(parent)?;
            let mut content = std::fs::read_to_string(&exclude).unwrap_or_default();
            if !content.is_empty() && !content.ends_with('\n') {
                content.push('\n');
            }
            content.push_str(TOREN_EXCLUDE);
            content.push('\n');
            std::fs::write(&exclude, content)?;
            report
                .fixed
                .push(format!("excluded .toren/ in {}", segment.name));
        }
    }

    if !report.findings.is_empty() && !fix {
        report.advice = Some("run `breq doctor --fix` to add the exclude".into());
    }

    Ok(report)
}

/// Whether an exclude file already has the entry this check writes.
fn exclude_covers_toren(exclude: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(exclude) else {
        return false;
    };
    content.lines().any(|line| line.trim() == TOREN_EXCLUDE)
}

/// `~/.toren/assignments.json` — the global registry that per-workspace state replaced.
///
/// Each record becomes state on the workspace it described: an instance uid, the task it was
/// working, its title, and a base commit read from the working copy as it stands. Records whose
/// workspace is already gone carry no state worth keeping, and neither do the ones that are no
/// longer active — a finished assignment describes work the new model does not track at all.
fn check_legacy_assignments(
    config: &Config,
    plugins: &PluginManager,
    fix: bool,
) -> Result<CheckReport> {
    migrate_assignments(&legacy_assignments_path(), config, plugins, fix)
}

/// The check itself, against a given registry file so tests never touch the real one.
fn migrate_assignments(
    path: &Path,
    config: &Config,
    plugins: &PluginManager,
    fix: bool,
) -> Result<CheckReport> {
    let mut report = CheckReport::new("legacy assignments");
    if !path.exists() {
        return Ok(report);
    }

    let content = std::fs::read_to_string(path)?;
    let records: Vec<Value> = serde_json::from_str(&content).unwrap_or_default();
    report.findings.push(format!(
        "{} holds {} assignment(s) from the pre-annotation state model",
        crate::config::tilde_shorten(path),
        records.len()
    ));

    let registry = PlaceRegistry::new(config)?;
    let mut kept = 0;
    let mut inactive = 0;

    for value in &records {
        let record = LegacyRecord::read(value);
        if !record.is_active() {
            inactive += 1;
            continue;
        }

        let label = record.label();
        let migration = migrate_record(&record, &registry, plugins, config, fix);

        if let Some(reason) = &migration.skipped {
            report
                .findings
                .push(format!("{}: nothing to migrate — {}", label, reason));
            continue;
        }
        if let Some(reason) = &migration.blocked {
            report
                .findings
                .push(format!("could not migrate '{}': {}", label, reason));
            kept += 1;
            continue;
        }
        for warning in &migration.warnings {
            report.findings.push(format!("{}: {}", label, warning));
        }
        if migration.actions.is_empty() {
            continue;
        }

        let actions = migration.actions.join(", ");
        if fix {
            report.fixed.push(format!("{}: {}", label, actions));
        } else {
            report
                .findings
                .push(format!("{}: would {}", label, actions));
        }
    }

    if inactive > 0 {
        report.findings.push(format!(
            "{} record(s) are not active — nothing to carry over",
            inactive
        ));
    }

    if !fix {
        report.advice = Some("run `breq doctor --fix` to migrate them into each workspace".into());
        return Ok(report);
    }

    // Dropping the legacy file is only safe once every record it holds has somewhere else to
    // live, so a single refused write keeps all of them.
    if kept > 0 {
        report.advice = Some(format!(
            "{} left in place: {} record(s) could not be migrated",
            crate::config::tilde_shorten(path),
            kept
        ));
        return Ok(report);
    }

    std::fs::remove_file(path)?;
    report
        .fixed
        .push(format!("removed {}", crate::config::tilde_shorten(path)));

    Ok(report)
}

/// One record of the legacy registry, in the terms per-workspace state needs.
struct LegacyRecord {
    segment: String,
    name: String,
    path: PathBuf,
    status: String,
    created_at: Option<String>,
    title: Option<String>,
    task_id: Option<String>,
    task_source: Option<String>,
}

impl LegacyRecord {
    fn read(value: &Value) -> Self {
        let string = |key: &str| {
            value[key]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        };
        let path = PathBuf::from(value["workspace_path"].as_str().unwrap_or_default());
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();

        Self {
            segment: string("segment").unwrap_or_default(),
            name,
            path,
            status: string("status").unwrap_or_default(),
            created_at: string("created_at"),
            title: string("task_title"),
            task_id: string("task_id"),
            task_source: string("task_source"),
        }
    }

    /// A record with no status at all is as live as the workspace it names.
    fn is_active(&self) -> bool {
        self.status.is_empty() || self.status == "active"
    }

    /// `<segment>/<workspace>`, for report lines.
    fn label(&self) -> String {
        format!("{}/{}", self.segment, self.name)
    }
}

/// What migrating one record did, or would do with `--fix`.
#[derive(Default)]
struct Migration {
    /// The writes this record implies, as verbs.
    actions: Vec<String>,
    /// Why the record was passed over, when it was.
    skipped: Option<String>,
    /// What went wrong without costing the migration.
    warnings: Vec<String>,
    /// Why the record has to stay in the legacy file.
    blocked: Option<String>,
}

/// Move one record onto the workspace it describes: `breq setup`, then `breq set +task`.
///
/// Without `fix` nothing is written — the same walk decides what the report would say.
fn migrate_record(
    record: &LegacyRecord,
    registry: &PlaceRegistry,
    plugins: &PluginManager,
    config: &Config,
    fix: bool,
) -> Migration {
    let mut migration = Migration::default();

    if record.name.is_empty() || !record.path.exists() {
        migration.skipped = Some(format!(
            "{} is gone",
            crate::config::tilde_shorten(&record.path)
        ));
        return migration;
    }
    let Some(segment) = registry.segments.find_by_name(&record.segment) else {
        migration.skipped = Some(format!("segment '{}' is not configured", record.segment));
        return migration;
    };

    let mut place = registry.get(&segment, &record.name);
    let decorate = !place.is_decorated();
    if decorate {
        migration.actions.push("decorate".to_string());
    }

    // Decoration is adoption — the same path `breq setup` takes over an existing working copy,
    // hooks and all. The legacy creation time is prefilled so the workspace does not come out
    // of the migration reading as minted today.
    if fix && decorate {
        place.state.created_at = record.created_at.clone();
        if let Err(e) = registry.adopt(&mut place) {
            migration.blocked = Some(format!("{:#}", e));
            return migration;
        }
    }

    if !decorate {
        if place.state.uid.is_none() {
            place.state.uid = Some(state::mint_uid());
            migration.actions.push("mint a uid".to_string());
        }
        if place.state.created_at.is_none() {
            place.state.created_at = record.created_at.clone();
        }
        // The base was a branch name before; re-read it as a commit from the working copy so
        // the change set means the same thing it will mean for new workspaces.
        if place.state.base.is_none() {
            if let Some(base) = registry
                .workspaces
                .base_revision(&segment.path, &place.path)
            {
                let vcs = place.vcs_label();
                place.state.set_base(vcs, base);
                migration.actions.push("record a base".to_string());
            }
        }
    }

    // D13's write rule: the stored title is the rung below the tracker's own, so it is written
    // once and never over a title the workspace already carries.
    if place.state.title.is_none() {
        if let Some(title) = &record.title {
            place.state.title = Some(title.clone());
            migration.actions.push(format!("set title '{}'", title));
        }
    }

    let mut linked = Vec::new();
    if let Some(id) = &record.task_id {
        match task_source(record, &segment, plugins, config) {
            Some(source) => {
                if place.state.add_task(&source, id) {
                    let link = crate::tasks::format_link(&source, id);
                    migration.actions.push(format!("link {}", link));
                    linked.push(link);
                }
            }
            None => migration
                .warnings
                .push(format!("no resolver claims task '{}'", id)),
        }
    }

    if !fix || migration.actions.is_empty() {
        return migration;
    }

    if let Err(e) = place.save() {
        migration.blocked = Some(format!("{:#}", e));
        return migration;
    }

    // Linking is a write-through point (D17): the link is local knowledge and its title and
    // status are not. A tracker that cannot be reached leaves the link — a workspace that knows
    // what it is for beats one that knows nothing.
    for link in &linked {
        if let Err(e) = crate::sets::refresh_task(&place, plugins, link) {
            migration
                .warnings
                .push(format!("linked {} but could not read it: {:#}", link, e));
        }
    }

    migration
}

/// Which source a record's task belongs to.
///
/// The registry recorded it explicitly for most records; for the ones it did not, the resolvers
/// are asked in turn, which is the only thing that can still answer.
fn task_source(
    record: &LegacyRecord,
    segment: &crate::segments::Segment,
    plugins: &PluginManager,
    config: &Config,
) -> Option<String> {
    if let Some(source) = record.task_source.clone() {
        return Some(source);
    }
    let id = record.task_id.as_ref()?;
    let sources = plugins.effective_sources(&config.tasks.sources);
    let ctx = PluginContext::new(Some(segment.path.clone()), Some(segment.name.clone()));
    plugins
        .resolve_info_multi(&sources, id, ctx)
        .ok()
        .map(|t| t.source)
}

/// The shipped workflow scripts, and whether they can be typed directly.
fn check_shipped_scripts(fix: bool) -> Result<CheckReport> {
    let mut report = CheckReport::new("workflow scripts");

    let missing = scripts::missing(true);
    if !missing.is_empty() {
        report
            .findings
            .push(format!("missing shipped scripts: {}", missing.join(", ")));
        if fix {
            for name in &missing {
                if let Some(path) = scripts::install(name)? {
                    report
                        .fixed
                        .push(format!("installed {}", crate::config::tilde_shorten(&path)));
                }
            }
        } else {
            report.advice = Some("run `breq doctor --fix` to install them".into());
        }
    }

    if !scripts::bin_dir_on_path() {
        report.findings.push(format!(
            "{} is not on PATH",
            crate::config::tilde_shorten(&scripts::bin_dir())
        ));
        report.advice = Some(format!(
            "`breq <name>` finds these regardless; add {} to PATH to run them directly",
            crate::config::tilde_shorten(&scripts::bin_dir())
        ));
    }

    Ok(report)
}

/// rmux sessions belonging to workspaces that no longer exist, or to a dead incarnation of one
/// that does. Both attach to a directory that isn't there any more.
fn check_stale_sessions(config: &Config, fix: bool) -> Result<CheckReport> {
    let mut report = CheckReport::new("rmux sessions");
    if !rmux::is_available() {
        return Ok(report);
    }

    let registry = PlaceRegistry::new(config)?;
    let live: Vec<String> = registry
        .list_all()
        .iter()
        .filter(|p| p.exists())
        .map(|p| p.session_name())
        .collect();

    let stale: Vec<String> = rmux::list_sessions()
        .into_iter()
        .filter(|s| s.starts_with("toren-"))
        .filter(|s| !live.contains(s))
        .collect();

    if stale.is_empty() {
        return Ok(report);
    }

    report.findings.push(format!(
        "sessions with no live workspace: {}",
        stale.join(", ")
    ));

    if fix {
        for session in &stale {
            rmux::kill_session(session)?;
            report.fixed.push(format!("killed {}", session));
        }
    } else {
        report.advice = Some("run `breq doctor --fix` to kill them".into());
    }

    Ok(report)
}

fn legacy_assignments_path() -> PathBuf {
    toren_root().join("assignments.json")
}

fn retired_history_path() -> PathBuf {
    toren_root().join("completion_history.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_report_has_nothing_to_say() {
        let report = CheckReport::new("x");
        assert!(report.is_clean());
    }

    #[test]
    fn findings_without_advice_still_count_as_dirty() {
        let mut report = CheckReport::new("x");
        report.findings.push("something".into());
        assert!(!report.is_clean());
    }

    #[test]
    fn the_exclude_entry_is_found_once_written() {
        let dir = tempfile::tempdir().unwrap();
        let exclude = dir.path().join("exclude");

        assert!(!exclude_covers_toren(&exclude), "missing file");

        std::fs::write(&exclude, "/target/\n").unwrap();
        assert!(!exclude_covers_toren(&exclude), "unrelated entries");

        std::fs::write(&exclude, format!("/target/\n{}\n", TOREN_EXCLUDE)).unwrap();
        assert!(exclude_covers_toren(&exclude));
    }

    // ── The legacy assignment registry ──────────────────────────────────────
    //
    // Every one of these runs against a toren root of its own: a segment, a workspace root,
    // an `assignments.json`, and a task resolver that answers without leaving the tempdir.

    use crate::state::{Cache, WorkspaceState};
    use serde_json::json;

    /// A resolver that answers for any id, standing in for the `runes` CLI.
    const RESOLVER: &str =
        r#"fn info(id) { #{ id: id, title: "breq list not accurate", status: "todo" } }"#;

    /// A resolver that cannot reach its store, the way `runes` fails outside one.
    const UNREACHABLE: &str = r#"fn info(id) { throw "no runes store here" }"#;

    struct World {
        _dir: tempfile::TempDir,
        root: PathBuf,
        config: Config,
        plugins: PluginManager,
    }

    impl World {
        fn new(resolver: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().to_path_buf();
            std::fs::create_dir_all(root.join("segments/demo")).unwrap();
            std::fs::create_dir_all(root.join("workspaces/demo")).unwrap();

            let tasks = root.join("plugins/tasks");
            std::fs::create_dir_all(&tasks).unwrap();
            std::fs::write(tasks.join("runes.rhai"), resolver).unwrap();

            let mut config = Config::default();
            config.ancillaries.workspace_root = root.join("workspaces");
            config.ancillaries.segments = vec![root.join("segments/demo").display().to_string()];
            config.segment_paths = config.compute_segment_paths();

            let plugins = PluginManager::new(&root.join("plugins")).unwrap();
            Self {
                _dir: dir,
                root,
                config,
                plugins,
            }
        }

        fn workspace(&self, name: &str) -> PathBuf {
            self.root.join("workspaces/demo").join(name)
        }

        fn make_workspace(&self, name: &str) -> PathBuf {
            let path = self.workspace(name);
            std::fs::create_dir_all(&path).unwrap();
            path
        }

        /// Write the registry file and return where it landed.
        fn assignments(&self, records: Vec<Value>) -> PathBuf {
            let path = self.root.join("assignments.json");
            std::fs::write(&path, serde_json::to_string_pretty(&records).unwrap()).unwrap();
            path
        }

        fn migrate(&self, path: &Path, fix: bool) -> CheckReport {
            migrate_assignments(path, &self.config, &self.plugins, fix).unwrap()
        }
    }

    /// A record shaped like the ones the real file holds.
    fn record(world: &World, name: &str, task: Option<(&str, &str)>) -> Value {
        let mut value = json!({
            "id": "6f1c0d2e-0000-4000-8000-000000000000",
            "ancillary_id": "Demo One",
            "segment": "demo",
            "workspace_path": world.workspace(name),
            "source": { "type": "Reference" },
            "status": "active",
            "created_at": "2026-05-06T22:29:32.306212+00:00",
            "updated_at": "2026-05-06T22:29:32.306212+00:00",
            "task_title": "breq list not accurate",
            "ancillary_num": 1,
        });
        if let Some((source, id)) = task {
            value["task_id"] = json!(id);
            if !source.is_empty() {
                value["task_source"] = json!(source);
            }
        }
        value
    }

    fn line_with<'a>(lines: &'a [String], needle: &str) -> &'a str {
        lines
            .iter()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no line mentioning '{}' in {:?}", needle, lines))
    }

    /// The whole migration, on the shape the real file holds: decorate, link, title, cache.
    #[test]
    fn a_record_becomes_state_on_the_workspace_it_described() {
        let world = World::new(RESOLVER);
        let ws = world.make_workspace("one");
        let path = world.assignments(vec![record(&world, "one", Some(("runes", "tor-mt4")))]);

        let report = world.migrate(&path, true);

        let applied = line_with(&report.fixed, "demo/one");
        assert!(applied.contains("decorate"), "{}", applied);
        assert!(applied.contains("link runes:tor-mt4"), "{}", applied);
        assert!(applied.contains("set title"), "{}", applied);

        let state = WorkspaceState::load(&ws, None).unwrap();
        assert!(state.uid.is_some());
        assert_eq!(
            state.created_at.as_deref(),
            Some("2026-05-06T22:29:32.306212+00:00")
        );
        assert_eq!(state.title.as_deref(), Some("breq list not accurate"));
        assert_eq!(state.task_links(), vec!["runes:tor-mt4"]);
        assert!(state.tasks[0].primary);
        assert!(state.tasks[0].added_at.is_some());

        // The link was paid for on the way past (D17), so `breq list` has a title to show.
        let cached = Cache::load(&ws)
            .get(&crate::sets::task_cache_key("runes:tor-mt4"))
            .expect("task cached");
        assert_eq!(cached.value["title"], json!("breq list not accurate"));

        // Nothing is left to migrate, so the registry goes.
        assert!(!path.exists());
        assert!(report.fixed.iter().any(|l| l.contains("removed")));
    }

    /// The second run has to be a no-op, whether or not the file survived the first.
    #[test]
    fn migrating_twice_writes_nothing_the_second_time() {
        let world = World::new(RESOLVER);
        let ws = world.make_workspace("one");
        let records = vec![record(&world, "one", Some(("runes", "tor-mt4")))];

        let path = world.assignments(records.clone());
        world.migrate(&path, true);
        let after_first = std::fs::read_to_string(WorkspaceState::path(&ws)).unwrap();

        let path = world.assignments(records);
        let report = world.migrate(&path, true);

        assert_eq!(
            std::fs::read_to_string(WorkspaceState::path(&ws)).unwrap(),
            after_first
        );
        assert!(
            !report.fixed.iter().any(|l| l.contains("demo/one")),
            "{:?}",
            report.fixed
        );
    }

    /// A record whose workspace was deleted describes nothing that still exists.
    #[test]
    fn a_workspace_that_is_gone_is_skipped() {
        let world = World::new(RESOLVER);
        let path = world.assignments(vec![record(&world, "gone", Some(("runes", "tor-mt4")))]);

        let report = world.migrate(&path, true);

        let skipped = line_with(&report.findings, "demo/gone");
        assert!(skipped.contains("nothing to migrate"), "{}", skipped);
        assert!(!crate::state::is_decorated(&world.workspace("gone")));
        // It blocks nothing: there is no state to lose by dropping the file.
        assert!(!path.exists());
    }

    /// Prompt-sourced records carry no task at all, and pre-`task_source` ones carry only an id.
    #[test]
    fn a_record_with_no_task_source_links_what_it_can() {
        let world = World::new(RESOLVER);
        let bare = world.make_workspace("one");
        let sourceless = world.make_workspace("two");
        let path = world.assignments(vec![
            record(&world, "one", None),
            record(&world, "two", Some(("", "how-q2"))),
        ]);

        world.migrate(&path, true);

        let state = WorkspaceState::load(&bare, None).unwrap();
        assert!(state.uid.is_some(), "decorated anyway");
        assert_eq!(state.title.as_deref(), Some("breq list not accurate"));
        assert!(state.tasks.is_empty());

        // With no recorded source, whichever resolver claims the id owns it.
        let state = WorkspaceState::load(&sourceless, None).unwrap();
        assert_eq!(state.task_links(), vec!["runes:how-q2"]);
    }

    /// Same rule as `breq set +task`: the link is local knowledge and survives the tracker.
    #[test]
    fn a_tracker_that_cannot_be_reached_keeps_the_link() {
        let world = World::new(UNREACHABLE);
        let ws = world.make_workspace("one");
        let path = world.assignments(vec![record(&world, "one", Some(("runes", "tor-mt4")))]);

        let report = world.migrate(&path, true);

        let warned = line_with(&report.findings, "could not read");
        assert!(warned.contains("no runes store here"), "{}", warned);

        let state = WorkspaceState::load(&ws, None).unwrap();
        assert_eq!(state.task_links(), vec!["runes:tor-mt4"]);
        assert!(Cache::load(&ws)
            .get(&crate::sets::task_cache_key("runes:tor-mt4"))
            .is_none());
        // The record still found a home, so the registry is still done with.
        assert!(!path.exists());
    }

    /// Without `--fix` the check says what it would do and touches nothing.
    #[test]
    fn a_report_without_fix_writes_nothing() {
        let world = World::new(RESOLVER);
        let ws = world.make_workspace("one");
        let path = world.assignments(vec![record(&world, "one", Some(("runes", "tor-mt4")))]);

        let report = world.migrate(&path, false);

        let planned = line_with(&report.findings, "demo/one");
        assert!(
            planned.starts_with("demo/one: would decorate"),
            "{}",
            planned
        );
        assert!(planned.contains("link runes:tor-mt4"), "{}", planned);
        assert!(report.fixed.is_empty());
        assert!(report.advice.is_some());

        assert!(!crate::state::is_decorated(&ws));
        assert!(path.exists());
    }

    /// A finished assignment describes work the new model does not track.
    #[test]
    fn records_that_are_not_active_are_left_alone() {
        let world = World::new(RESOLVER);
        let ws = world.make_workspace("one");
        let mut done = record(&world, "one", Some(("runes", "tor-mt4")));
        done["status"] = json!("completed");
        let path = world.assignments(vec![done]);

        let report = world.migrate(&path, true);

        assert!(line_with(&report.findings, "not active").contains('1'));
        assert!(!crate::state::is_decorated(&ws));
        assert!(!path.exists());
    }
}
