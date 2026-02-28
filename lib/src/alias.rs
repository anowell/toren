//! Alias expansion and execution.
//!
//! Aliases are config-driven shell templates that expand positional arguments
//! (`$1`, `$2`, etc.) and environment variables set from clean output
//! (`$ID`, `$WORKSPACE`, `$SEGMENT`, `$REVISION`).

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::process::Command;

/// Expand positional placeholders ($1, $2, …) and strip unresolved ones.
pub fn expand_alias(template: &str, args: &[String]) -> String {
    let mut result = template.to_string();
    for (i, arg) in args.iter().enumerate() {
        let placeholder = format!("${}", i + 1);
        result = result.replace(&placeholder, arg);
    }
    // Strip unresolved positional placeholders (e.g. $3 if only 2 args)
    for i in (args.len() + 1)..=9 {
        let placeholder = format!("${}", i);
        result = result.replace(&placeholder, "");
    }
    result
}

/// Execute an expanded alias template via `sh -c`.
///
/// Additional environment variables can be passed (e.g., from clean output).
pub fn execute_alias(expanded: &str, env_vars: &HashMap<String, String>) -> Result<i32> {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(expanded);

    for (key, value) in env_vars {
        cmd.env(key, value);
    }

    let status = cmd
        .status()
        .with_context(|| format!("Failed to execute alias: {}", expanded))?;

    Ok(status.code().unwrap_or(1))
}

/// Default aliases that preserve the beads UX.
pub fn default_aliases() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert(
        "assign".to_string(),
        "bd update $1 --status in_progress --assignee claude && bd show $1 | breq cmd --id $1".to_string(),
    );
    m.insert(
        "complete".to_string(),
        "breq clean $1 --push 2>/dev/null && bd update $ID --status closed".to_string(),
    );
    m.insert(
        "abort".to_string(),
        "breq clean $1 2>/dev/null && bd update $ID --status open --assignee ''".to_string(),
    );
    m.insert(
        "show".to_string(),
        "breq list $1 --detail".to_string(),
    );
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_alias_basic() {
        let result = expand_alias("echo $1 $2", &["hello".into(), "world".into()]);
        assert_eq!(result, "echo hello world");
    }

    #[test]
    fn test_expand_alias_missing_args() {
        let result = expand_alias("echo $1 $2 $3", &["hello".into()]);
        assert_eq!(result, "echo hello  ");
    }

    #[test]
    fn test_expand_alias_no_args() {
        let result = expand_alias("breq list --detail", &[]);
        assert_eq!(result, "breq list --detail");
    }

    #[test]
    fn test_expand_alias_repeated_placeholder() {
        let result = expand_alias("echo $1 and $1", &["hello".into()]);
        assert_eq!(result, "echo hello and hello");
    }
}
