//! Small filesystem helpers shared by the host and by plugin scripts.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

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
}
