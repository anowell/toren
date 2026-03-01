use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{debug, info};

use crate::config::Config;

/// A segment is a directory under a configured root, or a literal segment path.
/// Segments are resolved dynamically rather than pre-discovered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub name: String,
    pub path: PathBuf,
}

/// Manages segment discovery and resolution.
/// Supports glob-based roots (e.g., "~/proj/*") and literal segment paths.
#[derive(Debug, Clone)]
pub struct SegmentManager {
    /// Parent directories that contain segments as immediate children.
    roots: Vec<PathBuf>,
    /// Literal segment paths (non-glob entries in ancillaries.segments).
    literal_segments: Vec<PathBuf>,
    workspace_root: Option<PathBuf>,
}

impl SegmentManager {
    pub fn new(config: &Config) -> Result<Self> {
        let (roots, literal_segments) = config.resolve_segment_paths().clone();

        debug!(
            "Discovered {} segment roots, {} literal segments",
            roots.len(),
            literal_segments.len()
        );

        let ws_root = &config.ancillaries.workspace_root;
        let workspace_root = {
            let canonical = ws_root.canonicalize().unwrap_or_else(|_| ws_root.clone());
            if canonical.is_dir() {
                Some(canonical)
            } else {
                None
            }
        };

        Ok(Self {
            roots,
            literal_segments,
            workspace_root,
        })
    }

    /// Resolve a segment from a path.
    /// Checks literal segments first, then roots, then workspace-aware fallback,
    /// and finally CWD repo-root inference.
    pub fn resolve_from_path(&self, path: &Path) -> Option<Segment> {
        let canonical = path.canonicalize().ok()?;

        // Check literal segments (exact match or path is under a literal segment)
        for lit in &self.literal_segments {
            if canonical == *lit || canonical.starts_with(lit) {
                let name = lit.file_name()?.to_string_lossy().to_string();
                return Some(Segment {
                    name,
                    path: lit.clone(),
                });
            }
        }

        // Check roots (path is a root, or under a root)
        for root in &self.roots {
            if canonical == *root {
                let name = root.file_name()?.to_string_lossy().to_string();
                return Some(Segment {
                    name,
                    path: canonical,
                });
            }

            if canonical.starts_with(root) {
                let relative = canonical.strip_prefix(root).ok()?;
                let segment_name = relative.components().next()?;
                let segment_path = root.join(segment_name);

                if segment_path.is_dir() {
                    let name = segment_name.as_os_str().to_string_lossy().to_string();
                    return Some(Segment {
                        name,
                        path: segment_path,
                    });
                }
            }
        }

        // Fall back to workspace-aware resolution.
        if let Some(ref ws_root) = self.workspace_root {
            if canonical.starts_with(ws_root) {
                let relative = canonical.strip_prefix(ws_root).ok()?;
                let segment_component = relative.components().next()?;
                let name = segment_component.as_os_str().to_string_lossy().to_string();
                return self.find_by_name(&name);
            }
        }

        // CWD fallback: infer segment from repo root
        self.infer_segment_from_repo(path)
    }

    /// Infer a segment by detecting the repo root of the given path.
    /// The repo directory itself becomes the segment.
    fn infer_segment_from_repo(&self, path: &Path) -> Option<Segment> {
        let canonical = path.canonicalize().ok()?;

        // Walk up to find a repo root (.jj or .git)
        let mut current = Some(canonical.as_path());
        while let Some(dir) = current {
            if dir.join(".jj").exists() || dir.join(".git").exists() {
                let name = dir.file_name()?.to_string_lossy().to_string();
                return Some(Segment {
                    name,
                    path: dir.to_path_buf(),
                });
            }
            current = dir.parent();
        }

        None
    }

    /// Find a segment by name, searching literal segments first, then all roots.
    /// Returns the first matching directory found.
    pub fn find_by_name(&self, name: &str) -> Option<Segment> {
        // Check literal segments
        for lit in &self.literal_segments {
            if let Some(lit_name) = lit.file_name() {
                if lit_name.to_string_lossy() == name {
                    return Some(Segment {
                        name: name.to_string(),
                        path: lit.clone(),
                    });
                }
            }
        }

        // Check roots
        for root in &self.roots {
            let segment_path = root.join(name);
            if segment_path.is_dir() {
                return Some(Segment {
                    name: name.to_string(),
                    path: segment_path,
                });
            }
        }
        None
    }

    /// List all segment roots.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// List all segments from all roots and literal segments.
    /// Scans each root directory and returns all subdirectories as segments,
    /// plus all literal segments.
    pub fn list_all(&self) -> Vec<Segment> {
        let mut segments = Vec::new();

        // Add literal segments
        for lit in &self.literal_segments {
            if let Some(name) = lit.file_name() {
                let name = name.to_string_lossy().to_string();
                if !name.starts_with('.') {
                    segments.push(Segment {
                        name,
                        path: lit.clone(),
                    });
                }
            }
        }

        // Add segments from roots
        for root in &self.roots {
            if let Ok(entries) = std::fs::read_dir(root) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if path.is_dir() {
                        if let Some(name) = path.file_name() {
                            let name = name.to_string_lossy().to_string();
                            // Skip hidden directories and duplicates
                            if !name.starts_with('.')
                                && !segments.iter().any(|s| s.name == name)
                            {
                                segments.push(Segment { name, path });
                            }
                        }
                    }
                }
            }
        }

        segments.sort_by(|a, b| a.name.cmp(&b.name));
        segments
    }

    /// Check if a directory is a valid segment root for creating new segments.
    pub fn can_create_in(&self, root: &Path) -> bool {
        self.roots.iter().any(|r| r == root)
    }

    /// Create a new segment directory under a root.
    pub fn create_segment(&self, name: &str, root: &Path) -> Result<Segment> {
        if !self.can_create_in(root) {
            anyhow::bail!("Cannot create segments in: {}", root.display());
        }

        let path = root.join(name);
        if path.exists() {
            anyhow::bail!("Segment already exists: {}", path.display());
        }

        std::fs::create_dir_all(&path)
            .with_context(|| format!("Failed to create segment directory: {}", path.display()))?;

        let canonical = path.canonicalize()?;
        info!("Created new segment: {} at {}", name, path.display());

        Ok(Segment {
            name: name.to_string(),
            path: canonical,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config_with_segments(segments: Vec<String>) -> Config {
        let mut config = Config::default();
        config.ancillaries.segments = segments;
        config.segment_paths = config.compute_segment_paths();
        config
    }

    #[test]
    fn infer_segment_from_git_repo() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("myrepo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();

        let config = Config::default();
        let mgr = SegmentManager::new(&config).unwrap();

        let segment = mgr.infer_segment_from_repo(&repo);
        assert!(segment.is_some());
        let seg = segment.unwrap();
        assert_eq!(seg.name, "myrepo");
    }

    #[test]
    fn infer_segment_from_jj_repo() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("jjrepo");
        std::fs::create_dir_all(repo.join(".jj")).unwrap();

        let config = Config::default();
        let mgr = SegmentManager::new(&config).unwrap();

        let segment = mgr.infer_segment_from_repo(&repo);
        assert!(segment.is_some());
        let seg = segment.unwrap();
        assert_eq!(seg.name, "jjrepo");
    }

    #[test]
    fn infer_segment_from_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("myrepo");
        let subdir = repo.join("src/deep");
        std::fs::create_dir_all(subdir.join("nested")).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();

        let config = Config::default();
        let mgr = SegmentManager::new(&config).unwrap();

        let segment = mgr.infer_segment_from_repo(&subdir);
        assert!(segment.is_some());
        let seg = segment.unwrap();
        assert_eq!(seg.name, "myrepo");
    }

    #[test]
    fn no_inference_without_vcs() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();

        let config = Config::default();
        let mgr = SegmentManager::new(&config).unwrap();

        assert!(mgr.infer_segment_from_repo(&plain).is_none());
    }

    #[test]
    fn resolve_from_path_uses_cwd_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("fallback-repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();

        // No segments configured — should fall back to CWD repo inference
        let config = Config::default();
        let mgr = SegmentManager::new(&config).unwrap();

        let segment = mgr.resolve_from_path(&repo);
        assert!(segment.is_some());
        assert_eq!(segment.unwrap().name, "fallback-repo");
    }

    #[test]
    fn resolve_from_path_literal_segment() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("literalrepo");
        std::fs::create_dir_all(&repo).unwrap();

        let config = make_config_with_segments(vec![repo.display().to_string()]);
        let mgr = SegmentManager::new(&config).unwrap();

        let segment = mgr.resolve_from_path(&repo);
        assert!(segment.is_some());
        assert_eq!(segment.unwrap().name, "literalrepo");
    }

    #[test]
    fn resolve_from_path_glob_segment() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo1");
        std::fs::create_dir_all(&repo).unwrap();

        let config = make_config_with_segments(vec![format!("{}/*", dir.path().display())]);
        let mgr = SegmentManager::new(&config).unwrap();

        let segment = mgr.resolve_from_path(&repo);
        assert!(segment.is_some());
        assert_eq!(segment.unwrap().name, "repo1");
    }

    #[test]
    fn find_by_name_literal() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("named-repo");
        std::fs::create_dir_all(&repo).unwrap();

        let config = make_config_with_segments(vec![repo.display().to_string()]);
        let mgr = SegmentManager::new(&config).unwrap();

        let segment = mgr.find_by_name("named-repo");
        assert!(segment.is_some());
        assert_eq!(segment.unwrap().name, "named-repo");
    }

    #[test]
    fn list_all_includes_both() {
        let dir = tempfile::tempdir().unwrap();
        let literal = dir.path().join("literal-seg");
        let root = dir.path().join("root");
        let child = root.join("child-seg");
        std::fs::create_dir_all(&literal).unwrap();
        std::fs::create_dir_all(&child).unwrap();

        let config = make_config_with_segments(vec![
            literal.display().to_string(),
            format!("{}/*", root.display()),
        ]);
        let mgr = SegmentManager::new(&config).unwrap();

        let all = mgr.list_all();
        let names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"literal-seg"));
        assert!(names.contains(&"child-seg"));
    }
}
