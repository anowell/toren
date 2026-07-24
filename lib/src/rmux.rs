//! rmux session conventions shared by `breq` and the toren daemon.
//!
//! One ancillary maps to one session ([`session_name`]) holding a `shell` window and an `agent`
//! window. Both interfaces derive the same name, so either can attach to the other's agent.
//!
//! Uses the tmux-compatible CLI rather than `rmux-sdk` so `breq` works with the toren daemon down.
//! The daemon uses the SDK where it needs byte streams; see `daemon/src/services/pane_runner.rs`.

use anyhow::{bail, Context, Result};
use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Stdio};

/// Window running the coding agent.
pub const AGENT_WINDOW: &str = "agent";
/// Window running a plain login shell in the workspace.
pub const SHELL_WINDOW: &str = "shell";

fn rmux_bin() -> String {
    std::env::var("TOREN_RMUX_BIN").unwrap_or_else(|_| "rmux".to_string())
}

/// Whether rmux is usable here. `TOREN_NO_RMUX=1` forces the direct-exec path everywhere.
pub fn is_available() -> bool {
    if std::env::var("TOREN_NO_RMUX").is_ok_and(|v| v != "0") {
        return false;
    }
    which::which(rmux_bin()).is_ok()
}

/// `toren-<segment>-<workspace>`, e.g. `toren-two-one`.
pub fn session_name(segment: &str, workspace: &str) -> String {
    format!("toren-{}-{}", sanitize(segment), sanitize(workspace))
}

/// Reduce a name to characters rmux accepts in a session name.
fn sanitize(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let collapsed = cleaned
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if collapsed.is_empty() {
        "x".to_string()
    } else {
        collapsed
    }
}

fn rmux<I, S>(args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new(rmux_bin())
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("Failed to run {}", rmux_bin()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("rmux failed: {}", stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Whether a session with this name currently exists.
pub fn session_exists(session: &str) -> bool {
    rmux(["has-session", "-t", session]).is_ok()
}

/// Create the session and its `shell` window if absent. Idempotent.
pub fn ensure_session(session: &str, cwd: &Path) -> Result<()> {
    if session_exists(session) {
        return Ok(());
    }

    let cwd = cwd.to_string_lossy().into_owned();
    rmux([
        "new-session",
        "-d",
        "-s",
        session,
        "-n",
        SHELL_WINDOW,
        "-c",
        &cwd,
    ])
    .with_context(|| format!("Failed to create rmux session '{}'", session))?;

    // Without this an exiting agent takes the whole session down mid-attach.
    rmux(["set-option", "-t", session, "remain-on-exit", "on"])
        .with_context(|| format!("Failed to configure rmux session '{}'", session))?;

    Ok(())
}

/// Replace the session's `agent` window with a fresh run of `argv`, and make it active.
///
/// `argv` reaches the process unmodified — no shell — so prompts need no escaping.
pub fn spawn_agent(session: &str, cwd: &Path, argv: &[String]) -> Result<()> {
    if argv.is_empty() {
        bail!("Cannot spawn an agent with an empty command");
    }

    // Window names aren't unique; without this a second spawn adds a second `agent`.
    let _ = rmux(["kill-window", "-t", &window_target(session, AGENT_WINDOW)]);

    let cwd = cwd.to_string_lossy().into_owned();
    let mut args = vec![
        "new-window".to_string(),
        "-t".to_string(),
        session.to_string(),
        "-n".to_string(),
        AGENT_WINDOW.to_string(),
        "-c".to_string(),
        cwd,
        "--".to_string(),
    ];
    args.extend(argv.iter().cloned());

    rmux(args).with_context(|| format!("Failed to spawn agent in rmux session '{}'", session))?;
    Ok(())
}

/// Make `window` the active window of `session`, so a subsequent attach lands there.
pub fn select_window(session: &str, window: &str) -> Result<()> {
    rmux(["select-window", "-t", &window_target(session, window)])
        .with_context(|| format!("Failed to select rmux window '{}'", window))?;
    Ok(())
}

/// Window names in the session, in index order.
pub fn list_windows(session: &str) -> Result<Vec<String>> {
    let out = rmux(["list-windows", "-t", session, "-F", "#{window_name}"])?;
    Ok(out
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// One pane in a session, as rmux currently sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneState {
    pub window: String,
    /// Process exited; `remain-on-exit` keeps the pane around.
    pub dead: bool,
    /// Current foreground command, e.g. `zsh` or `claude`.
    pub command: String,
    pub pid: i32,
}

impl PaneState {
    /// Whether this pane holds work, as opposed to a shell sitting at its prompt.
    pub fn is_busy(&self) -> bool {
        const SHELLS: &[&str] = &[
            "sh", "bash", "zsh", "fish", "dash", "ksh", "nu", "csh", "tcsh",
        ];
        !self.dead && !SHELLS.contains(&self.command.as_str())
    }
}

/// Every pane in the session, across all windows.
pub fn list_panes(session: &str) -> Result<Vec<PaneState>> {
    let out = rmux([
        "list-panes",
        "-t",
        session,
        "-s",
        "-F",
        "#{window_name}\t#{pane_dead}\t#{pane_current_command}\t#{pane_pid}",
    ])?;

    Ok(out
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let window = fields.next()?.trim().to_string();
            let dead = fields.next()?.trim() == "1";
            let command = fields.next().unwrap_or("").trim().to_string();
            let pid = fields
                .next()
                .and_then(|p| p.trim().parse().ok())
                .unwrap_or(0);
            Some(PaneState {
                window,
                dead,
                command,
                pid,
            })
        })
        .collect())
}

/// Whether the session's agent process is still alive.
pub fn agent_is_running(session: &str) -> bool {
    list_panes(session).is_ok_and(|panes| panes.iter().any(|p| p.window == AGENT_WINDOW && !p.dead))
}

/// Panes holding work. Callers about to destroy the session check this first.
pub fn busy_panes(session: &str) -> Vec<PaneState> {
    list_panes(session)
        .unwrap_or_default()
        .into_iter()
        .filter(PaneState::is_busy)
        .collect()
}

/// Respawn the shell window if its process exited.
///
/// `remain-on-exit` is session-wide, so typing `exit` leaves a dead pane every later
/// `breq shell` would attach to.
pub fn ensure_shell(session: &str, cwd: &Path) -> Result<()> {
    let shell_is_dead = list_panes(session)
        .unwrap_or_default()
        .iter()
        .any(|p| p.window == SHELL_WINDOW && p.dead);

    if !shell_is_dead {
        return Ok(());
    }

    let cwd = cwd.to_string_lossy().into_owned();
    rmux([
        "respawn-pane",
        "-k",
        "-t",
        &window_target(session, SHELL_WINDOW),
        "-c",
        &cwd,
    ])
    .with_context(|| format!("Failed to restart the shell in rmux session '{}'", session))?;
    Ok(())
}

/// Kill the whole session, agent and shell alike.
pub fn kill_session(session: &str) -> Result<()> {
    if !session_exists(session) {
        return Ok(());
    }
    rmux(["kill-session", "-t", session])
        .with_context(|| format!("Failed to kill rmux session '{}'", session))?;
    Ok(())
}

/// Kill just the agent window, leaving the session (and its shell) alive.
pub fn kill_agent(session: &str) -> Result<()> {
    let _ = rmux(["kill-window", "-t", &window_target(session, AGENT_WINDOW)]);
    Ok(())
}

/// The `rmux attach` command for a session; callers `exec()` it so the TUI owns the terminal.
pub fn attach_command(session: &str) -> Command {
    let mut cmd = Command::new(rmux_bin());
    cmd.args(["attach-session", "-t", session]);
    cmd
}

fn window_target(session: &str, window: &str) -> String {
    format!("{}:{}", session, window)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_name_is_deterministic() {
        assert_eq!(session_name("two", "one"), "toren-two-one");
        assert_eq!(session_name("Toren", "One"), "toren-toren-one");
    }

    #[test]
    fn session_name_sanitizes_separators() {
        assert_eq!(session_name("my.repo", "one"), "toren-my-repo-one");
        assert_eq!(session_name("a/b/c", "one"), "toren-a-b-c-one");
    }

    #[test]
    fn session_name_collapses_runs() {
        assert_eq!(session_name("foo...bar", "one"), "toren-foo-bar-one");
        assert_eq!(session_name("-leading-", "one"), "toren-leading-one");
    }

    #[test]
    fn session_name_survives_empty_components() {
        assert_eq!(session_name("...", "one"), "toren-x-one");
    }

    #[test]
    fn window_target_joins_with_colon() {
        assert_eq!(
            window_target("toren-two-one", AGENT_WINDOW),
            "toren-two-one:agent"
        );
    }

    /// The sequence `breq do` performs before it execs `rmux attach`. Needs rmux installed.
    #[test]
    fn ensure_and_spawn_produce_the_expected_session() {
        if !is_available() {
            eprintln!("skipping: rmux not on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let session = session_name(&format!("libtest{}", std::process::id()), "one");

        ensure_session(&session, dir.path()).unwrap();
        assert!(session_exists(&session));
        assert_eq!(list_windows(&session).unwrap(), vec![SHELL_WINDOW]);

        // Every `breq do` calls this.
        ensure_session(&session, dir.path()).unwrap();
        assert_eq!(list_windows(&session).unwrap(), vec![SHELL_WINDOW]);

        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "sleep 30".to_string(),
        ];
        spawn_agent(&session, dir.path(), &argv).unwrap();
        assert!(list_windows(&session)
            .unwrap()
            .contains(&AGENT_WINDOW.to_string()));

        // A second assignment replaces the agent window rather than stacking a duplicate.
        spawn_agent(&session, dir.path(), &argv).unwrap();
        let windows = list_windows(&session).unwrap();
        assert_eq!(windows.iter().filter(|w| *w == AGENT_WINDOW).count(), 1);

        // The shell window survives the agent, keeping the session reattachable.
        kill_agent(&session).unwrap();
        assert!(session_exists(&session));
        assert_eq!(list_windows(&session).unwrap(), vec![SHELL_WINDOW]);

        kill_session(&session).unwrap();
        assert!(!session_exists(&session));
        // Cleanup paths call this blindly.
        kill_session(&session).unwrap();
    }

    /// The signal cleanup relies on to avoid destroying live work. Needs rmux installed.
    #[test]
    fn busy_panes_distinguishes_an_idle_shell_from_live_work() {
        if !is_available() {
            eprintln!("skipping: rmux not on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let session = session_name(&format!("busytest{}", std::process::id()), "one");
        ensure_session(&session, dir.path()).unwrap();

        // Must not look busy, or `breq complete` could never succeed.
        assert!(busy_panes(&session).is_empty());
        assert!(!agent_is_running(&session));

        spawn_agent(
            &session,
            dir.path(),
            &["/bin/sleep".to_string(), "30".to_string()],
        )
        .unwrap();
        assert!(agent_is_running(&session));
        let busy = busy_panes(&session);
        assert_eq!(
            busy.len(),
            1,
            "expected only the agent to be busy: {:?}",
            busy
        );
        assert_eq!(busy[0].window, AGENT_WINDOW);
        assert!(busy[0].pid > 0, "busy pane should report a pid for --kill");

        // A dead agent is not live work.
        kill_agent(&session).unwrap();
        assert!(!agent_is_running(&session));
        assert!(busy_panes(&session).is_empty());

        kill_session(&session).unwrap();
    }

    /// Needs rmux installed.
    #[test]
    fn ensure_shell_revives_a_shell_the_user_exited() {
        if !is_available() {
            eprintln!("skipping: rmux not on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let session = session_name(&format!("shelltest{}", std::process::id()), "one");
        ensure_session(&session, dir.path()).unwrap();

        ensure_shell(&session, dir.path()).unwrap();
        assert!(!shell_is_dead(&session));

        // The user typing `exit`.
        rmux([
            "send-keys",
            "-t",
            &window_target(&session, SHELL_WINDOW),
            "exit",
            "Enter",
        ])
        .unwrap();
        wait_until(|| shell_is_dead(&session));

        ensure_shell(&session, dir.path()).unwrap();
        assert!(!shell_is_dead(&session), "shell should have been respawned");

        kill_session(&session).unwrap();
    }

    fn shell_is_dead(session: &str) -> bool {
        list_panes(session)
            .unwrap_or_default()
            .iter()
            .any(|p| p.window == SHELL_WINDOW && p.dead)
    }

    /// rmux applies commands asynchronously, so a fixed sleep would be flaky or slow.
    fn wait_until(mut condition: impl FnMut() -> bool) {
        for _ in 0..100 {
            if condition() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("condition never became true");
    }
}
