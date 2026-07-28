//! Small filesystem helpers shared by the host and by plugin scripts.

use anyhow::{Context, Result};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// How much of a file's tail to consider when looking for its last line.
///
/// Agent session logs put whole tool results on one line, so a small window would routinely
/// land mid-line and find nothing.
const TAIL_WINDOW_BYTES: u64 = 262_144;

/// Read the last non-empty line of a file by seeking from the end.
///
/// Returns `None` for a missing or empty file, or when the last line is longer than the tail
/// window — callers treat that the same as "nothing to report".
pub fn read_last_line(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let file_len = file.metadata().ok()?.len();
    if file_len == 0 {
        return None;
    }

    let read_size = file_len.min(TAIL_WINDOW_BYTES);
    file.seek(SeekFrom::End(-(read_size as i64))).ok()?;

    let mut buf = vec![0u8; read_size as usize];
    file.read_exact(&mut buf).ok()?;

    let text = String::from_utf8_lossy(&buf);
    text.lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(|s| s.to_string())
}

/// Disambiguates temp files when one process writes the same path twice at once.
static WRITE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Write a file atomically: a temp file in the same directory, fsynced, then renamed over the
/// destination.
///
/// Every piece of state breq persists goes through here. A plain `fs::write` truncates first,
/// so a crash mid-write leaves a half-file — and for `<ws>/.toren/state.json` that means losing
/// the `uid` that names a live rmux session.
pub fn write_atomic(path: &Path, bytes: impl AsRef<[u8]>) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("state.tmp");
    let temp = dir.join(format!(
        ".{}.{}.{}.tmp",
        name,
        std::process::id(),
        WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));

    let write = || -> Result<()> {
        let mut file = std::fs::File::create(&temp)
            .with_context(|| format!("Failed to create {}", temp.display()))?;
        file.write_all(bytes.as_ref())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path).with_context(|| format!("Failed to write {}", path.display()))
    };

    let result = write();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_last_non_empty_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        std::fs::write(&path, "first\nsecond\n\n").unwrap();
        assert_eq!(read_last_line(&path).as_deref(), Some("second"));
    }

    #[test]
    fn empty_and_missing_files_read_as_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty");
        std::fs::write(&empty, "").unwrap();
        assert!(read_last_line(&empty).is_none());
        assert!(read_last_line(&dir.path().join("nope")).is_none());
    }

    #[test]
    fn atomic_writes_replace_the_previous_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        write_atomic(&path, "first").unwrap();
        write_atomic(&path, "second").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
    }

    #[test]
    fn atomic_writes_leave_no_temp_files_behind() {
        let dir = tempfile::tempdir().unwrap();
        write_atomic(&dir.path().join("state.json"), "x").unwrap();
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["state.json".to_string()]);
    }

    #[test]
    fn a_failed_write_keeps_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        write_atomic(&path, "keep").unwrap();
        // A directory cannot be renamed over by a file, so the second write fails late.
        assert!(write_atomic(&dir.path().join("missing/state.json"), "x").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep");
    }
}
