//! Workflow verbs as `breq-<name>` scripts.
//!
//! Breq's own verbs manage places. Everything that is *your workflow* — what "complete" means,
//! whether shipping tears the workspace down, which status a review sets — lives in a plain
//! shell script called by name, git-style: `breq complete one` runs `breq-complete one`.
//!
//! The point is the seam. A script composes `breq get`/`set`/`sh` and knows nothing about
//! which tracker or forge is behind them, so customizing your workflow is editing one file
//! rather than patching breq.
//!
//! Scripts are found on `PATH` first, then in `~/.toren/bin` (where the shipped ones land), so
//! your own copy anywhere on PATH shadows the shipped default.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Scripts breq ships. Installed by `breq init` / `breq doctor`, never overwritten after that.
const SHIPPED: &[(&str, &str)] = &[
    (
        "breq-complete",
        include_str!("../../contrib/scripts/breq-complete"),
    ),
    (
        "breq-abort",
        include_str!("../../contrib/scripts/breq-abort"),
    ),
    (
        "breq-submit",
        include_str!("../../contrib/scripts/breq-submit"),
    ),
];

/// Scripts installed for everyone, versus ones that only make sense for a detected stack.
pub fn is_universal(name: &str) -> bool {
    matches!(name, "breq-complete" | "breq-abort")
}

/// Where shipped scripts live: `~/.toren/bin`.
pub fn bin_dir() -> PathBuf {
    crate::config::toren_root().join("bin")
}

/// The script implementing `breq <name>`, if there is one.
///
/// PATH wins over `~/.toren/bin` so a hand-rolled `breq-complete` earlier on your PATH
/// shadows the shipped one without deleting anything.
pub fn find(name: &str) -> Option<PathBuf> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let script = format!("breq-{}", name);

    if let Ok(path) = which::which(&script) {
        return Some(path);
    }

    let local = bin_dir().join(&script);
    if is_executable(&local) {
        return Some(local);
    }
    None
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Names of shipped scripts not yet present in `~/.toren/bin`.
///
/// `universal_only` skips combo-specific scripts, which `init` installs only when it detects
/// the stack they assume.
pub fn missing(universal_only: bool) -> Vec<&'static str> {
    let dir = bin_dir();
    SHIPPED
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !universal_only || is_universal(name))
        .filter(|name| !dir.join(name).exists())
        .collect()
}

/// Write a shipped script into `~/.toren/bin`, executable. Never clobbers an existing file —
/// once it's yours, it's yours.
pub fn install(name: &str) -> Result<Option<PathBuf>> {
    let (_, content) = SHIPPED
        .iter()
        .find(|(n, _)| *n == name)
        .with_context(|| format!("No shipped script named '{}'", name))?;

    let dir = bin_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;

    let path = dir.join(name);
    if path.exists() {
        return Ok(None);
    }

    std::fs::write(&path, content)
        .with_context(|| format!("Failed to write {}", path.display()))?;

    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms)?;

    Ok(Some(path))
}

/// All shipped script names.
pub fn shipped_names() -> Vec<&'static str> {
    SHIPPED.iter().map(|(name, _)| *name).collect()
}

/// A workflow verb as `breq --help` shows it.
pub struct Verb {
    pub name: String,
    /// How to call it, from the script's own first comment line.
    pub usage: String,
    pub summary: String,
}

/// Separators a script may put between its usage and what it does.
const DASHES: &[&str] = &[" — ", " – ", " -- ", " - ", ": "];

/// Read a script's self-description: the first comment line below the shebang, split into
/// how you call it and what it does.
///
/// A script that documents nothing still lists, under its filename — the whole point is that
/// dropping any `breq-<name>` into `~/.toren/bin` makes a working verb.
fn describe(path: &Path, name: &str) -> Verb {
    let fallback = format!("breq {}", name);
    let doc = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| {
            text.lines()
                .skip_while(|l| l.starts_with("#!") || l.trim().is_empty())
                .take_while(|l| l.trim_start().starts_with('#'))
                .map(|l| l.trim_start().trim_start_matches('#').trim().to_string())
                .find(|l| !l.is_empty())
        })
        .unwrap_or_default();

    let (usage, summary) = match DASHES
        .iter()
        .filter_map(|sep| doc.find(sep).map(|at| (at, sep.len())))
        .min()
    {
        Some((at, len)) if doc.starts_with(&fallback) => {
            (doc[..at].to_string(), doc[at + len..].to_string())
        }
        _ => (fallback, doc),
    };

    Verb {
        name: name.to_string(),
        usage,
        summary,
    }
}

/// Every `breq-<name>` script reachable as a verb, sorted by name.
///
/// Same precedence as [`find`], so what this lists is what would actually run.
pub fn discover() -> Vec<Verb> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    dirs.push(bin_dir());

    let mut verbs: Vec<Verb> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_prefix("breq-"))
            else {
                continue;
            };
            if name.is_empty()
                || verbs.iter().any(|v| v.name == name)
                || !is_executable(&path)
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                continue;
            }
            verbs.push(describe(&path, name));
        }
    }
    verbs.sort_by(|a, b| a.name.cmp(&b.name));
    verbs
}

/// The `Custom Commands:` section of `breq --help`.
pub fn verbs_help() -> String {
    let verbs = discover();
    if verbs.is_empty() {
        return "Custom Commands:\n  none installed — see `breq doctor` to add the shipped ones."
            .to_string();
    }

    let width = verbs.iter().map(|v| v.usage.len()).max().unwrap_or(0);
    let mut out = String::from("Custom Commands:\n");
    for verb in &verbs {
        if verb.summary.is_empty() {
            out.push_str(&format!("  {}\n", verb.usage));
        } else {
            out.push_str(&format!(
                "  {:<width$}  {}\n",
                verb.usage,
                verb.summary,
                width = width
            ));
        }
    }
    out.push_str("\nAny executable `breq-<name>` on PATH or in ~/.toren/bin is a verb.");
    out
}

/// Whether `~/.toren/bin` is on PATH — scripts are still reachable through `breq <name>`
/// either way, but a user typing `breq-complete` directly needs it.
pub fn bin_dir_on_path() -> bool {
    let dir = bin_dir();
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|p| p == dir))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_scripts_are_shell_and_documented() {
        for (name, content) in SHIPPED {
            assert!(
                content.starts_with("#!/usr/bin/env bash"),
                "{} needs a shebang",
                name
            );
            assert!(
                content.contains(&format!("breq {}", name.trim_start_matches("breq-"))),
                "{} should document its own invocation",
                name
            );
        }
    }

    #[test]
    fn universal_scripts_are_the_task_verbs() {
        assert!(is_universal("breq-complete"));
        assert!(is_universal("breq-abort"));
        assert!(!is_universal("breq-submit"));
    }

    #[test]
    fn a_script_describes_itself_by_its_first_comment() {
        let dir = tempfile::tempdir().unwrap();
        let write = |body: &str| {
            let path = dir.path().join("breq-ship");
            std::fs::write(&path, body).unwrap();
            describe(&path, "ship")
        };

        let documented = write("#!/usr/bin/env bash\n# breq ship [ws] — push it\n\ntrue\n");
        assert_eq!(documented.usage, "breq ship [ws]");
        assert_eq!(documented.summary, "push it");

        // No usage prefix: the whole line is what it does.
        let bare = write("#!/usr/bin/env bash\n# push it\ntrue\n");
        assert_eq!(bare.usage, "breq ship");
        assert_eq!(bare.summary, "push it");

        // A dash inside the summary is not a separator; only the first one splits.
        let dashed = write("#!/usr/bin/env bash\n# breq ship — push it - hard\ntrue\n");
        assert_eq!(dashed.usage, "breq ship");
        assert_eq!(dashed.summary, "push it - hard");

        // Undocumented still lists, so a scratch script is a working verb.
        let silent = write("#!/usr/bin/env bash\ntrue\n");
        assert_eq!(silent.usage, "breq ship");
        assert_eq!(silent.summary, "");
    }

    #[test]
    fn find_rejects_names_that_are_not_script_names() {
        assert!(find("../../etc/passwd").is_none());
        assert!(find("").is_none());
        assert!(find("with space").is_none());
    }
}
