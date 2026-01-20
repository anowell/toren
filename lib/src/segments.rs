use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

use crate::config::Config;

/// A segment is a directory under a configured root.
/// Segments are resolved dynamically rather than pre-discovered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub name: String,
    pub path: PathBuf,
}

/// Manages segment discovery and resolution.
/// Any subdirectory of a configured root is a valid segment.
#[derive(Debug, Clone)]
pub struct SegmentManager {
    roots: Vec<PathBuf>,
}

impl SegmentManager {
    pub fn new(config: &Config) -> Result<Self> {
        let mut roots = Vec::new();

        for root in &config.segments.roots {
            let expanded = Self::expand_path(root.to_str().unwrap_or(""))?;
            if expanded.is_dir() {
                let canonical = expanded.canonicalize().unwrap_or(expanded.clone());
                debug!("Registered segment root: {}", canonical.display());
                roots.push(canonical);
            } else {
                warn!("Segment root does not exist: {}", expanded.display());
            }
        }

        info!("Discovered {} segment roots", roots.len());

        Ok(Self { roots })
    }

    fn expand_path(path_str: &str) -> Result<PathBuf> {
        let expanded = shellexpand::tilde(path_str);
        Ok(PathBuf::from(expanded.as_ref()))
    }

    /// Resolve a segment from a path.
    /// If the path is under a root, returns the segment (the immediate child of the root).
    /// If the path itself is a root, returns a segment for that root.
    pub fn resolve_from_path(&self, path: &Path) -> Option<Segment> {
        let canonical = path.canonicalize().ok()?;

        for root in &self.roots {
            // Check if path equals a root (special case: root itself is the segment)
            if canonical == *root {
                let name = root.file_name()?.to_string_lossy().to_string();
                return Some(Segment {
                    name,
                    path: canonical,
                });
            }

            // Check if path is under this root
            if canonical.starts_with(root) {
                // Find the segment directory (immediate child of root that contains path)
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

        None
    }

    /// Find a segment by name, searching all roots.
    /// Returns the first matching directory found.
    pub fn find_by_name(&self, name: &str) -> Option<Segment> {
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

    /// List all segments from all roots.
    /// Scans each root directory and returns all subdirectories as segments.
    pub fn list_all(&self) -> Vec<Segment> {
        let mut segments = Vec::new();

        for root in &self.roots {
            if let Ok(entries) = std::fs::read_dir(root) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if path.is_dir() {
                        if let Some(name) = path.file_name() {
                            let name = name.to_string_lossy().to_string();
                            // Skip hidden directories
                            if !name.starts_with('.') {
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
