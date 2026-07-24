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

use crate::annotations::{self, Annotations};
use crate::config::{toren_root, Config};
use crate::place::PlaceRegistry;
use crate::plugins::{PluginContext, PluginManager};
use crate::rmux;
use crate::scripts;

/// What one check found and, if fixing, what it did about it.
#[derive(Debug, Default)]
pub struct CheckReport {
    pub name: &'static str,
    /// One line per problem found.
    pub findings: Vec<String>,
    /// One line per repair applied.
    pub fixed: Vec<String>,
    /// Something the user has to do themselves.
    pub advice: Option<String>,
}

impl CheckReport {
    fn new(name: &'static str) -> Self {
        Self {
            name,
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
        check_legacy_assignments(config, plugins, fix)?,
        check_shipped_scripts(fix)?,
        check_stale_sessions(config, fix)?,
        check_toren_excluded(config, fix)?,
    ])
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

/// `~/.toren/assignments.json` — the global registry that the annotation store replaced.
///
/// Each record becomes annotations on the workspace it described: an instance uid, the task it
/// was working, its title, and a base commit read from the working copy as it stands. Records
/// whose workspace is already gone carry no state worth keeping.
fn check_legacy_assignments(
    config: &Config,
    plugins: &PluginManager,
    fix: bool,
) -> Result<CheckReport> {
    let mut report = CheckReport::new("legacy assignments");
    let path = legacy_assignments_path();
    if !path.exists() {
        return Ok(report);
    }

    let content = std::fs::read_to_string(&path)?;
    let records: Vec<Value> = serde_json::from_str(&content).unwrap_or_default();
    report.findings.push(format!(
        "{} holds {} assignment(s) from the pre-annotation state model",
        crate::config::tilde_shorten(&path),
        records.len()
    ));

    if !fix {
        report.advice = Some("run `breq doctor --fix` to migrate them into each workspace".into());
        return Ok(report);
    }

    let registry = PlaceRegistry::new(config)?;

    for record in &records {
        let segment_name = record["segment"].as_str().unwrap_or_default().to_string();
        let ws_path = PathBuf::from(record["workspace_path"].as_str().unwrap_or_default());
        let ws_name = ws_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();

        if ws_name.is_empty() || !ws_path.exists() {
            report.fixed.push(format!(
                "dropped record for missing workspace '{}'",
                ws_name
            ));
            continue;
        }

        let Some(segment) = registry.segments.find_by_name(&segment_name) else {
            report.fixed.push(format!(
                "dropped record for unknown segment '{}'",
                segment_name
            ));
            continue;
        };

        let mut place = registry.get(&segment, &ws_name);
        let mut annotations = Annotations::load(&place.path).unwrap_or_default();

        if annotations.get_str("uid").is_none() {
            annotations.set_str("uid", annotations::mint_uid());
        }
        annotations.set_str("name", place.name.clone());
        annotations.set_str("segment", segment.name.clone());
        if let Some(created) = record["created_at"].as_str() {
            annotations.set_default("created_at", created);
        }
        if let Some(title) = record["task_title"].as_str().filter(|t| !t.is_empty()) {
            annotations.set_default("title", title);
        }

        // The base was a branch name before; re-read it as a commit from the working copy so
        // the change set means the same thing it will mean for new workspaces.
        if annotations.get_str("base").is_none() {
            if let Some(base) = registry
                .workspaces
                .base_revision(&segment.path, &place.path)
            {
                annotations.set_str("base", base);
            }
        }

        if let Some(task_id) = record["task_id"].as_str().filter(|t| !t.is_empty()) {
            let source = record["task_source"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .or_else(|| {
                    let sources = plugins.effective_sources(&config.tasks.sources);
                    let ctx =
                        PluginContext::new(Some(segment.path.clone()), Some(segment.name.clone()));
                    plugins
                        .resolve_info_multi(&sources, task_id, ctx)
                        .ok()
                        .map(|t| t.source)
                });

            match source {
                Some(source) => {
                    annotations.add_to_list("task", &crate::tasks::format_link(&source, task_id));
                }
                None => report.fixed.push(format!(
                    "{}: kept title but dropped task '{}' — no resolver claims it",
                    ws_name, task_id
                )),
            }
        }

        place.annotations = annotations;
        place.save()?;
        report
            .fixed
            .push(format!("migrated '{}' into {}/.toren", ws_name, ws_name));
    }

    std::fs::remove_file(&path)?;
    report
        .fixed
        .push(format!("removed {}", crate::config::tilde_shorten(&path)));

    Ok(report)
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
}
