//! Workspaces as *places*: a directory, its VCS state, and its annotations.
//!
//! Breq keeps no database of workspaces. The VCS is the registry: segments come from
//! config, each segment's working copies come from `jj workspace list` / `git worktree
//! list`, and anything sitting in the workspace root is listed too — so a manually deleted
//! workspace shows up as prunable and a hand-made working copy shows up as adoptable.
//!
//! Workspaces are duck-typed. A *decorated* one carries `<ws>/.toren/annotations.json`;
//! an undecorated one is still a place breq can list, enter, and adopt.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::annotations::{self, Annotations, Cache};
use crate::config::Config;
use crate::segments::{Segment, SegmentManager};
use crate::workspace::WorkspaceManager;

/// One workspace, as breq sees it.
#[derive(Debug, Clone)]
pub struct Place {
    /// Slot name, e.g. "one". Reusable across incarnations.
    pub name: String,
    /// Segment (repo) name, e.g. "toren".
    pub segment: String,
    /// Segment root on disk.
    pub segment_path: PathBuf,
    /// The working copy.
    pub path: PathBuf,
    /// Whether the VCS still tracks this working copy.
    pub vcs_tracked: bool,
    pub annotations: Annotations,
}

impl Place {
    /// Load a place from disk. Cheap: two small file reads at most.
    pub fn load(segment: &Segment, name: &str, path: PathBuf, vcs_tracked: bool) -> Self {
        Self {
            name: name.to_string(),
            segment: segment.name.clone(),
            segment_path: segment.path.clone(),
            annotations: Annotations::load_lossy(&path),
            path,
            vcs_tracked,
        }
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// Whether breq state lives here, as opposed to a working copy it merely found.
    pub fn is_decorated(&self) -> bool {
        !self.annotations.is_empty()
    }

    /// This incarnation's id. Undecorated places have none.
    pub fn uid(&self) -> Option<String> {
        self.annotations.get_str("uid")
    }

    /// The rmux session this incarnation owns. Sessions matching the workspace but not the
    /// uid belong to a dead incarnation — see [`crate::rmux::stale_sessions`].
    pub fn session_name(&self) -> String {
        crate::rmux::session_name(&self.segment, &self.name, self.uid().as_deref())
    }

    /// The commit the workspace was forked from, if setup recorded one.
    pub fn base(&self) -> Option<String> {
        self.annotations.get_str("base")
    }

    /// Stack parent workspace name, for `setup --from` children.
    pub fn parent(&self) -> Option<String> {
        self.annotations.get_str("parent")
    }

    /// Task links, as `source:id` strings.
    pub fn tasks(&self) -> Vec<String> {
        self.annotations.get_list("task")
    }

    pub fn cache(&self) -> Cache {
        Cache::load(&self.path)
    }

    /// Age of the workspace, from the `created_at` annotation.
    pub fn age(&self) -> Option<chrono::Duration> {
        let created = self.annotations.get_str("created_at")?;
        let then = chrono::DateTime::parse_from_rfc3339(&created).ok()?;
        Some(chrono::Utc::now().signed_duration_since(then.with_timezone(&chrono::Utc)))
    }

    /// Compact age for list output: `2m`, `3h`, `4d`.
    pub fn age_label(&self) -> String {
        match self.age() {
            Some(d) if d.num_days() > 0 => format!("{}d", d.num_days()),
            Some(d) if d.num_hours() > 0 => format!("{}h", d.num_hours()),
            Some(d) if d.num_minutes() > 0 => format!("{}m", d.num_minutes()),
            Some(_) => "now".to_string(),
            None => "-".to_string(),
        }
    }

    /// Environment every process breq starts in this place inherits, so an in-pane `breq`
    /// invocation needs no `-w`.
    pub fn env(&self) -> Vec<(String, String)> {
        let mut env = vec![
            ("TOREN_SEGMENT".to_string(), self.segment.clone()),
            ("TOREN_WORKSPACE".to_string(), self.name.clone()),
            (
                "TOREN_WORKSPACE_PATH".to_string(),
                self.path.display().to_string(),
            ),
        ];
        if let Some(uid) = self.uid() {
            env.push(("TOREN_UID".to_string(), uid));
        }
        env
    }

    /// Mint annotations for a freshly created workspace.
    pub fn initialize(&mut self, base: Option<String>, parent: Option<&str>) -> Result<String> {
        let uid = annotations::mint_uid();
        self.annotations.set_str("uid", uid.clone());
        self.annotations.set_str("name", self.name.clone());
        self.annotations.set_str("segment", self.segment.clone());
        self.annotations
            .set_default("created_at", chrono::Utc::now().to_rfc3339());
        if let Some(base) = base {
            self.annotations.set_str("base", base);
        }
        if let Some(parent) = parent {
            self.annotations.set_str("parent", parent);
        }
        self.save()?;
        Ok(uid)
    }

    pub fn save(&self) -> Result<()> {
        self.annotations.save(&self.path)
    }

    /// Drop `<ws>/.toren/` — the inverse of adoption.
    pub fn undecorate(&self) -> Result<()> {
        let dir = annotations::toren_dir(&self.path);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .with_context(|| format!("Failed to remove {}", dir.display()))?;
        }
        Ok(())
    }
}

/// Enumerates places across segments. Holds no state of its own beyond config-derived
/// managers — every read hits the VCS and the workspaces' own `.toren/` directories.
pub struct PlaceRegistry {
    pub segments: SegmentManager,
    pub workspaces: WorkspaceManager,
}

impl PlaceRegistry {
    pub fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            segments: SegmentManager::new(config)?,
            workspaces: WorkspaceManager::new(
                config.ancillaries.workspace_root.clone(),
                Some(config.proxy.domain.clone()),
            ),
        })
    }

    /// Resolve a segment by name, or from the current directory.
    pub fn segment(&self, name: Option<&str>) -> Result<Segment> {
        if let Some(name) = name {
            return self
                .segments
                .find_by_name(name)
                .with_context(|| format!("Segment '{}' not found in any segment root", name));
        }
        let cwd = std::env::current_dir()?;
        self.segments.resolve_from_path(&cwd).with_context(|| {
            "Current directory is not under any configured segment.\n\
             Configure segments in ~/.toren/config.toml:\n\
             [ancillaries]\n\
             segments = [\"~/proj/*\"]"
                .to_string()
        })
    }

    /// Every place in a segment: VCS-tracked working copies plus anything else sitting in
    /// the segment's workspace directory.
    pub fn list(&self, segment: &Segment) -> Vec<Place> {
        let tracked = self
            .workspaces
            .list_workspaces(&segment.path)
            .unwrap_or_default();

        let mut names: Vec<String> = tracked
            .iter()
            .filter(|n| *n != "default")
            .cloned()
            .collect();

        // Directories with no VCS registration still deserve a row — they're the prunable
        // leftovers and the adoptable strays.
        let segment_dir = self.workspaces.workspace_path(&segment.name, "");
        if let Ok(entries) = std::fs::read_dir(&segment_dir) {
            for entry in entries.flatten() {
                let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else {
                    continue;
                };
                if name.starts_with('.') || !entry.path().is_dir() {
                    continue;
                }
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }

        names.sort_by_key(|n| crate::naming::word_to_number(n).unwrap_or(u32::MAX));

        names
            .into_iter()
            .map(|name| {
                let path = self.workspaces.workspace_path(&segment.name, &name);
                let vcs_tracked = tracked.contains(&name);
                Place::load(segment, &name, path, vcs_tracked)
            })
            .filter(|p| p.exists() || p.vcs_tracked)
            .collect()
    }

    /// Every place, across every configured segment.
    pub fn list_all(&self) -> Vec<Place> {
        self.segments
            .list_all()
            .iter()
            .flat_map(|segment| self.list(segment))
            .collect()
    }

    /// A specific place by name, whether or not it is decorated.
    pub fn get(&self, segment: &Segment, name: &str) -> Place {
        let name = name.to_lowercase();
        let path = self.workspaces.workspace_path(&segment.name, &name);
        let vcs_tracked = self
            .workspaces
            .list_workspaces(&segment.path)
            .unwrap_or_default()
            .contains(&name);
        Place::load(segment, &name, path, vcs_tracked)
    }

    /// A place that must exist on disk.
    pub fn require(&self, segment: &Segment, name: &str) -> Result<Place> {
        let place = self.get(segment, name);
        if !place.exists() {
            anyhow::bail!(
                "Workspace '{}' not found at {}",
                place.name,
                place.path.display()
            );
        }
        Ok(place)
    }

    /// The place containing `path`, if any. The segment root itself is not a place — only
    /// workspaces under the workspace root are.
    pub fn resolve_from_path(&self, path: &Path) -> Option<Place> {
        let canonical = path.canonicalize().ok()?;
        let root = self
            .workspaces
            .root()
            .canonicalize()
            .unwrap_or_else(|_| self.workspaces.root().to_path_buf());
        let relative = canonical.strip_prefix(&root).ok()?;

        let mut components = relative.components();
        let segment_name = components.next()?.as_os_str().to_str()?.to_string();
        let ws_name = components.next()?.as_os_str().to_str()?.to_string();

        let segment = self.segments.find_by_name(&segment_name)?;
        Some(self.get(&segment, &ws_name))
    }

    /// The place the current process is standing in — `$TOREN_WORKSPACE` when breq runs
    /// inside a pane breq started, else the cwd.
    pub fn resolve_from_env(&self) -> Option<Place> {
        if let (Ok(segment_name), Ok(ws_name)) = (
            std::env::var("TOREN_SEGMENT"),
            std::env::var("TOREN_WORKSPACE"),
        ) {
            if let Some(segment) = self.segments.find_by_name(&segment_name) {
                let place = self.get(&segment, &ws_name);
                if place.exists() {
                    return Some(place);
                }
            }
        }
        let cwd = std::env::current_dir().ok()?;
        self.resolve_from_path(&cwd)
    }

    /// Materialize a new place: VCS workspace, setup (or fork) hooks, annotations.
    ///
    /// `from` stacks the new workspace on another one that is still in flight — the child's
    /// base is the parent's working copy, so its change set covers only its own work.
    pub fn create(
        &self,
        segment: &Segment,
        name: Option<&str>,
        from: Option<&Place>,
        pool_size: u32,
    ) -> Result<Place> {
        let name = match name {
            Some(name) => name.to_lowercase(),
            None => self.next_available_name(segment, pool_size),
        };
        let num = crate::naming::word_to_number(&name).unwrap_or(0);

        let origin = match from {
            Some(parent) => {
                let revision = self
                    .workspaces
                    .fork_point(&segment.path, &parent.path)
                    .with_context(|| {
                        format!(
                            "Could not resolve a revision to fork from '{}'",
                            parent.name
                        )
                    })?;
                crate::workspace::WorkspaceOrigin::Stacked {
                    parent: parent.name.clone(),
                    revision,
                }
            }
            None => crate::workspace::WorkspaceOrigin::Tip,
        };

        let (path, _) = self.workspaces.create_workspace_with_setup(
            &segment.path,
            &segment.name,
            &name,
            num,
            &origin,
        )?;

        let base = self.workspaces.base_revision(&segment.path, &path);
        let mut place = Place::load(segment, &name, path, true);
        place.initialize(base, from.map(|p| p.name.as_str()))?;
        Ok(place)
    }

    /// Adopt an existing working copy: mint annotations and run setup hooks in place.
    ///
    /// The inverse of `teardown --no-delete`, and the reason workspaces are duck-typed —
    /// anything that is a working copy can become a place breq manages.
    pub fn adopt(&self, place: &mut Place) -> Result<()> {
        let num = crate::naming::word_to_number(&place.name).unwrap_or(0);

        // Hook failures are not fatal here: the working copy already exists and is the
        // user's, so there is nothing to roll back and nothing gained by refusing to adopt.
        if let Err(e) =
            self.workspaces
                .run_setup(&place.segment_path, &place.path, &place.name, num, None)
        {
            tracing::warn!("Setup hooks reported a problem while adopting: {:#}", e);
        }

        let base = self
            .workspaces
            .base_revision(&place.segment_path, &place.path);
        place.initialize(base, None)?;
        Ok(())
    }

    /// Next free workspace name in a segment, filling gaps before extending the pool.
    pub fn next_available_name(&self, segment: &Segment, pool_size: u32) -> String {
        let occupied: std::collections::HashSet<u32> = self
            .list(segment)
            .iter()
            .filter_map(|p| crate::naming::word_to_number(&p.name))
            .collect();

        for n in 1..=pool_size.max(1) {
            if !occupied.contains(&n) {
                return crate::naming::number_to_word(n).to_lowercase();
            }
        }
        let max = occupied.iter().max().copied().unwrap_or(0);
        crate::naming::number_to_word(max + 1).to_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment_at(path: &Path, name: &str) -> Segment {
        Segment {
            name: name.to_string(),
            path: path.to_path_buf(),
        }
    }

    fn registry_with_root(root: &Path) -> PlaceRegistry {
        let mut config = Config::default();
        config.ancillaries.workspace_root = root.to_path_buf();
        config.ancillaries.segments = vec![];
        PlaceRegistry::new(&config).unwrap()
    }

    #[test]
    fn undecorated_working_copies_are_listed() {
        let dir = tempfile::tempdir().unwrap();
        let ws_root = dir.path().join("workspaces");
        std::fs::create_dir_all(ws_root.join("demo/one")).unwrap();

        let registry = registry_with_root(&ws_root);
        let segment = segment_at(&dir.path().join("demo"), "demo");
        let places = registry.list(&segment);

        assert_eq!(places.len(), 1);
        assert_eq!(places[0].name, "one");
        assert!(!places[0].is_decorated());
        assert!(!places[0].vcs_tracked);
    }

    #[test]
    fn annotations_come_from_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let ws_root = dir.path().join("workspaces");
        let ws = ws_root.join("demo/two");
        std::fs::create_dir_all(&ws).unwrap();

        let registry = registry_with_root(&ws_root);
        let segment = segment_at(&dir.path().join("demo"), "demo");

        let mut place = registry.get(&segment, "two");
        let uid = place.initialize(Some("abc123".into()), None).unwrap();

        let reloaded = registry.get(&segment, "two");
        assert_eq!(reloaded.uid().as_deref(), Some(uid.as_str()));
        assert_eq!(reloaded.base().as_deref(), Some("abc123"));
        assert!(reloaded.is_decorated());
        assert!(reloaded.session_name().ends_with(&uid));
    }

    #[test]
    fn next_name_fills_gaps() {
        let dir = tempfile::tempdir().unwrap();
        let ws_root = dir.path().join("workspaces");
        std::fs::create_dir_all(ws_root.join("demo/one")).unwrap();
        std::fs::create_dir_all(ws_root.join("demo/three")).unwrap();

        let registry = registry_with_root(&ws_root);
        let segment = segment_at(&dir.path().join("demo"), "demo");
        assert_eq!(registry.next_available_name(&segment, 10), "two");
    }

    #[test]
    fn resolve_from_path_finds_the_containing_place() {
        let dir = tempfile::tempdir().unwrap();
        let ws_root = dir.path().join("workspaces");
        let deep = ws_root.join("demo/one/src/nested");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir_all(dir.path().join("demo/.git")).unwrap();

        let mut config = Config::default();
        config.ancillaries.workspace_root = ws_root.clone();
        config.ancillaries.segments = vec![dir.path().join("demo").display().to_string()];
        config.segment_paths = config.compute_segment_paths();
        let registry = PlaceRegistry::new(&config).unwrap();

        let place = registry.resolve_from_path(&deep).expect("place in cwd");
        assert_eq!(place.name, "one");
        assert_eq!(place.segment, "demo");
    }

    #[test]
    fn resolve_from_path_outside_the_root_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let ws_root = dir.path().join("workspaces");
        std::fs::create_dir_all(&ws_root).unwrap();
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();

        let registry = registry_with_root(&ws_root);
        assert!(registry.resolve_from_path(&elsewhere).is_none());
    }

    #[test]
    fn undecorate_removes_only_the_state_dir() {
        let dir = tempfile::tempdir().unwrap();
        let ws_root = dir.path().join("workspaces");
        let ws = ws_root.join("demo/one");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("keep.txt"), "work").unwrap();

        let registry = registry_with_root(&ws_root);
        let segment = segment_at(&dir.path().join("demo"), "demo");
        let mut place = registry.get(&segment, "one");
        place.initialize(None, None).unwrap();

        place.undecorate().unwrap();
        assert!(!annotations::is_decorated(&ws));
        assert!(ws.join("keep.txt").exists());
    }
}
