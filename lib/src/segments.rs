use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

use crate::config::Config;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub name: String,
    pub path: PathBuf,
    pub source: SegmentSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SegmentSource {
    Glob,
    Path,
    Root,
}

#[derive(Debug, Clone)]
pub struct SegmentManager {
    segments: Vec<Segment>,
    roots: Vec<PathBuf>,
}

impl SegmentManager {
    pub fn new(config: &Config) -> Result<Self> {
        let mut manager = Self {
            segments: Vec::new(),
            roots: Vec::new(),
        };

        manager.discover_segments(config)?;

        info!("Discovered {} segments", manager.segments.len());
        for segment in &manager.segments {
            debug!("  {} -> {}", segment.name, segment.path.display());
        }

        Ok(manager)
    }

    fn discover_segments(&mut self, config: &Config) -> Result<()> {
        let mut seen_paths = HashSet::new();

        // Discover from globs
        for glob_pattern in &config.segments.globs {
            let expanded = Self::expand_path(glob_pattern)?;
            let pattern_str = expanded.to_string_lossy();
            debug!("Expanding glob: {} -> {}", glob_pattern, pattern_str);

            match glob::glob(&pattern_str) {
                Ok(paths) => {
                    for entry in paths.flatten() {
                        if entry.is_dir() {
                            let canonical = entry.canonicalize().unwrap_or(entry.clone());
                            if seen_paths.insert(canonical.clone()) {
                                if let Some(name) = canonical.file_name() {
                                    self.segments.push(Segment {
                                        name: name.to_string_lossy().to_string(),
                                        path: canonical,
                                        source: SegmentSource::Glob,
                                    });
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to expand glob pattern {}: {}", pattern_str, e);
                }
            }
        }

        // Add explicit paths
        for path in &config.segments.paths {
            let expanded = Self::expand_path(path.to_str().unwrap_or(""))?;
            if expanded.is_dir() {
                let canonical = expanded.canonicalize().unwrap_or(expanded.clone());
                if seen_paths.insert(canonical.clone()) {
                    if let Some(name) = canonical.file_name() {
                        self.segments.push(Segment {
                            name: name.to_string_lossy().to_string(),
                            path: canonical,
                            source: SegmentSource::Path,
                        });
                    }
                }
            }
        }

        // Store roots (expanded and canonical)
        for root in &config.segments.roots {
            let expanded = Self::expand_path(root.to_str().unwrap_or(""))?;
            if expanded.is_dir() {
                let canonical = expanded.canonicalize().unwrap_or(expanded.clone());
                self.roots.push(canonical);
            } else {
                warn!("Root directory does not exist: {}", expanded.display());
            }
        }

        Ok(())
    }

    fn expand_path(path_str: &str) -> Result<PathBuf> {
        let expanded = shellexpand::tilde(path_str);
        Ok(PathBuf::from(expanded.as_ref()))
    }

    pub fn list(&self) -> &[Segment] {
        &self.segments
    }

    pub fn get(&self, name: &str) -> Option<&Segment> {
        self.segments.iter().find(|s| s.name == name)
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn can_create_in(&self, root: &Path) -> bool {
        self.roots.iter().any(|r| r == root)
    }

    pub fn create_segment(&mut self, name: &str, root: &Path) -> Result<Segment> {
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
        let segment = Segment {
            name: name.to_string(),
            path: canonical,
            source: SegmentSource::Root,
        };

        self.segments.push(segment.clone());
        info!("Created new segment: {} at {}", name, path.display());

        Ok(segment)
    }
}
