//! rmux session conventions shared by `breq` and the toren daemon.
//!
//! One workspace incarnation maps to one session ([`session_name`]) holding a `shell` window and
//! an `agent` window. Both interfaces derive the same name from the workspace's annotations, so
//! either can attach to the other's agent.
//!
//! Session names carry the workspace's instance uid. A session that matches the workspace but not
//! its uid is provably left over from a deleted-and-recreated workspace — see [`stale_sessions`].
//! Delete-and-recreate is routine, not a corner case, so this is what keeps a new incarnation from
//! attaching to a dead one's panes.
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

/// `toren-<segment>-<workspace>-<uid>`, e.g. `toren-toren-one-k3m9xz`.
///
/// A workspace with no uid (undecorated, or pre-annotation) gets the bare prefix, so breq still
/// works in a working copy it merely adopted.
pub fn session_name(segment: &str, workspace: &str, uid: Option<&str>) -> String {
    let prefix = session_prefix(segment, workspace);
    match uid {
        Some(uid) if !uid.is_empty() => format!("{}-{}", prefix, sanitize(uid)),
        _ => prefix,
    }
}

/// `toren-<segment>-<workspace>` — every incarnation of one workspace slot shares this.
pub fn session_prefix(segment: &str, workspace: &str) -> String {
    format!("toren-{}-{}", sanitize(segment), sanitize(workspace))
}

/// Names of all sessions the rmux server currently holds.
pub fn list_sessions() -> Vec<String> {
    rmux(["list-sessions", "-F", "#{session_name}"])
        .map(|out| {
            out.lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Sessions belonging to earlier incarnations of this workspace slot.
///
/// Matching the slot but not the uid means the workspace was deleted and recreated underneath
/// them; their panes point at a directory that no longer exists. Callers kill these rather than
/// attaching to them.
pub fn stale_sessions(segment: &str, workspace: &str, uid: Option<&str>) -> Vec<String> {
    let prefix = session_prefix(segment, workspace);
    let current = session_name(segment, workspace, uid);
    list_sessions()
        .into_iter()
        .filter(|s| s != &current)
        .filter(|s| s == &prefix || s.starts_with(&format!("{}-", prefix)))
        .collect()
}

/// Kill every session left over from an earlier incarnation. Returns how many died.
pub fn reconcile(segment: &str, workspace: &str, uid: Option<&str>) -> usize {
    let stale = stale_sessions(segment, workspace, uid);
    for session in &stale {
        tracing::info!("Killing stale rmux session '{}'", session);
        let _ = kill_session(session);
    }
    stale.len()
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

/// Create the session and its `shell` window if absent, exporting `env` into it. Idempotent.
///
/// The environment is set on the session rather than the pane, so every window opened later —
/// by breq, by the daemon, or by the user splitting inside the session — inherits the workspace
/// context and an in-pane `breq` needs no `-w`.
///
/// The session default is `remain-on-exit off`: a shell you `exit` closes its window like any
/// terminal. Only the agent window overrides this to `on` (see [`spawn_agent`]), because a
/// finished or crashed agent should linger as an observable dead pane rather than vanish.
pub fn ensure_session(session: &str, cwd: &Path, env: &[(String, String)]) -> Result<()> {
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

    // Shells close on exit; the agent window opts back into remain-on-exit for observability.
    rmux(["set-option", "-t", session, "remain-on-exit", "off"])
        .with_context(|| format!("Failed to configure rmux session '{}'", session))?;

    for (key, value) in env {
        let _ = rmux(["set-environment", "-t", session, key, value]);
    }

    Ok(())
}

/// Replace `window` with a fresh run of `argv`, and make it active.
///
/// The one way breq starts a long-lived process in a session. When a window of this name already
/// exists it is replaced by rename → create-fresh → kill-old, rather than kill-then-create. Two
/// reasons: the replacement gets a *new* pane, which is how the daemon notices the swap and hands
/// an attached browser over to it; and the session never momentarily drops to zero windows (which
/// would make rmux destroy it) even when `window` is the only one left — the case that arises once
/// every shell has been exited. `argv` reaches the process unmodified — no shell — so prompts need
/// no escaping.
pub fn run_in_window(session: &str, window: &str, cwd: &Path, argv: &[String]) -> Result<()> {
    if argv.is_empty() {
        bail!("Cannot spawn an empty command in rmux window '{}'", window);
    }

    let cwd = cwd.to_string_lossy().into_owned();

    // Move any existing same-named window aside so the fresh one is unambiguous and the session
    // keeps at least one window throughout.
    let stashed = if window_exists(session, window) {
        let stash = format!("{}-replacing", window);
        let _ = rmux([
            "rename-window",
            "-t",
            &window_target(session, window),
            &stash,
        ]);
        Some(stash)
    } else {
        None
    };

    let mut args = vec![
        "new-window".to_string(),
        "-t".to_string(),
        session.to_string(),
        "-n".to_string(),
        window.to_string(),
        "-c".to_string(),
        cwd,
        "--".to_string(),
    ];
    args.extend(argv.iter().cloned());

    let spawned = rmux(args)
        .with_context(|| format!("Failed to spawn '{}' in rmux session '{}'", window, session));

    if let Some(stash) = stashed {
        match &spawned {
            // Replaced: drop the old window.
            Ok(_) => {
                let _ = rmux(["kill-window", "-t", &window_target(session, &stash)]);
            }
            // Spawn failed: restore the old window's name rather than orphan it under the stash.
            Err(_) => {
                let _ = rmux([
                    "rename-window",
                    "-t",
                    &window_target(session, &stash),
                    window,
                ]);
            }
        }
    }

    spawned.map(|_| ())
}

/// Run the coding agent in the session's `agent` window. Sugar over [`run_in_window`].
///
/// Unlike a shell, the agent window keeps `remain-on-exit on`: a finished or crashed agent stays
/// as a dead pane so the `exited` status is observable and the browser can still show what it did.
/// Continuing an agent is a separate act — `breq do --resume` starts a fresh process from the
/// agent's session id. The option is re-applied on every spawn because `run_in_window` recreates
/// the window, which would otherwise inherit the session's `off` default.
pub fn spawn_agent(session: &str, cwd: &Path, argv: &[String]) -> Result<()> {
    run_in_window(session, AGENT_WINDOW, cwd, argv)?;
    let _ = rmux([
        "set-option",
        "-w",
        "-t",
        &window_target(session, AGENT_WINDOW),
        "remain-on-exit",
        "on",
    ]);
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

    /// Coarse liveness, as `breq list`/`get` report it.
    ///
    /// `remain-on-exit` is what makes `Exited` observable at all: a crashed one-shot leaves a
    /// dead pane behind, which is a different thing from a session that was never started.
    pub fn status(&self) -> PaneStatus {
        if self.dead {
            PaneStatus::Exited
        } else if self.is_busy() {
            PaneStatus::Running
        } else {
            PaneStatus::Idle
        }
    }
}

/// Liveness of one pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneStatus {
    Idle,
    Running,
    Exited,
}

impl std::fmt::Display for PaneStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PaneStatus::Idle => "idle",
            PaneStatus::Running => "running",
            PaneStatus::Exited => "exited",
        })
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

/// Ensure the session has a live `shell` window to attach to.
///
/// Shells close when you `exit` them (they don't remain-on-exit), so the primary `shell` window
/// may be gone entirely rather than merely dead — recreate it in that case, and revive it in the
/// rare case its pane is dead but the window lingers.
pub fn ensure_shell(session: &str, cwd: &Path) -> Result<()> {
    let cwd = cwd.to_string_lossy().into_owned();

    if !window_exists(session, SHELL_WINDOW) {
        rmux(["new-window", "-t", session, "-n", SHELL_WINDOW, "-c", &cwd])
            .with_context(|| format!("Failed to open the shell in rmux session '{}'", session))?;
        return Ok(());
    }

    let shell_is_dead = list_panes(session)
        .unwrap_or_default()
        .iter()
        .any(|p| p.window == SHELL_WINDOW && p.dead);
    if shell_is_dead {
        rmux([
            "respawn-pane",
            "-k",
            "-t",
            &window_target(session, SHELL_WINDOW),
            "-c",
            &cwd,
        ])
        .with_context(|| format!("Failed to restart the shell in rmux session '{}'", session))?;
    }
    Ok(())
}

/// Whether a window of this name exists in the session.
pub fn window_exists(session: &str, window: &str) -> bool {
    list_windows(session)
        .unwrap_or_default()
        .iter()
        .any(|w| w == window)
}

/// The next free shell window name: `shell`, then `shell-2`, `shell-3`, …
///
/// Shell windows are a set, not a slot — you can have several open at once (a dev server, a log
/// tail, a scratch prompt). Only the first keeps the bare `shell` name so existing tooling and
/// `breq sh`'s default attach still find it.
pub fn next_shell_window(session: &str) -> String {
    let existing = list_windows(session).unwrap_or_default();
    if !existing.iter().any(|w| w == SHELL_WINDOW) {
        return SHELL_WINDOW.to_string();
    }
    for n in 2.. {
        let candidate = format!("{}-{}", SHELL_WINDOW, n);
        if !existing.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!("the loop always returns")
}

/// Open a new window running a plain login shell, returning its name.
///
/// Distinct from [`run_in_window`], which runs a given argv: a shell window has no command of
/// its own, so an exiting shell (`remain-on-exit`) leaves an observable dead pane rather than a
/// respawn loop. Names are unique, so this never tramples an existing window.
pub fn open_shell(session: &str, cwd: &Path) -> Result<String> {
    let window = next_shell_window(session);
    let cwd = cwd.to_string_lossy().into_owned();
    rmux(["new-window", "-t", session, "-n", &window, "-c", &cwd]).with_context(|| {
        format!(
            "Failed to open a shell window in rmux session '{}'",
            session
        )
    })?;
    Ok(window)
}

/// Kill a single window by name, leaving the rest of the session alive.
pub fn kill_window(session: &str, window: &str) -> Result<()> {
    let _ = rmux(["kill-window", "-t", &window_target(session, window)]);
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
        assert_eq!(session_name("two", "one", None), "toren-two-one");
        assert_eq!(session_name("Toren", "One", None), "toren-toren-one");
    }

    #[test]
    fn session_name_carries_the_instance_uid() {
        assert_eq!(
            session_name("toren", "one", Some("k3m9xz")),
            "toren-toren-one-k3m9xz"
        );
        // An undecorated workspace still gets a usable name.
        assert_eq!(session_name("toren", "one", Some("")), "toren-toren-one");
    }

    #[test]
    fn session_name_sanitizes_separators() {
        assert_eq!(session_name("my.repo", "one", None), "toren-my-repo-one");
        assert_eq!(session_name("a/b/c", "one", None), "toren-a-b-c-one");
    }

    #[test]
    fn session_name_collapses_runs() {
        assert_eq!(session_name("foo...bar", "one", None), "toren-foo-bar-one");
        assert_eq!(session_name("-leading-", "one", None), "toren-leading-one");
    }

    #[test]
    fn session_name_survives_empty_components() {
        assert_eq!(session_name("...", "one", None), "toren-x-one");
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
        let uid = "aaa111";
        let session = session_name(&format!("libtest{}", std::process::id()), "one", Some(uid));

        ensure_session(&session, dir.path(), &[]).unwrap();
        assert!(session_exists(&session));
        assert_eq!(list_windows(&session).unwrap(), vec![SHELL_WINDOW]);

        // Every `breq do` calls this.
        ensure_session(&session, dir.path(), &[]).unwrap();
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

    /// Multiple shells coexist as a set; the first keeps the bare name. Needs rmux installed.
    #[test]
    fn shells_open_as_a_set_with_unique_names() {
        if !is_available() {
            eprintln!("skipping: rmux not on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let session = session_name(&format!("shellsettest{}", std::process::id()), "one", None);
        ensure_session(&session, dir.path(), &[]).unwrap();

        // The session's birth shell already holds the bare name, so the next is `shell-2`.
        assert_eq!(next_shell_window(&session), format!("{}-2", SHELL_WINDOW));
        let second = open_shell(&session, dir.path()).unwrap();
        assert_eq!(second, format!("{}-2", SHELL_WINDOW));
        assert!(window_exists(&session, &second));

        let third = open_shell(&session, dir.path()).unwrap();
        assert_eq!(third, format!("{}-3", SHELL_WINDOW));

        // Killing one window leaves the others — a set, not an all-or-nothing session.
        kill_window(&session, &second).unwrap();
        assert!(!window_exists(&session, &second));
        assert!(window_exists(&session, &third));
        assert!(window_exists(&session, SHELL_WINDOW));

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
        let session = session_name(&format!("busytest{}", std::process::id()), "one", None);
        ensure_session(&session, dir.path(), &[]).unwrap();

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

    /// Exiting a shell closes its window (no remain-on-exit); an exited agent lingers as a dead
    /// pane. This asymmetry is the fix for "I typed exit and the window got stuck." Needs rmux.
    #[test]
    fn shells_close_on_exit_but_agents_linger() {
        if !is_available() {
            eprintln!("skipping: rmux not on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let session = session_name(&format!("exittest{}", std::process::id()), "one", None);
        ensure_session(&session, dir.path(), &[]).unwrap();

        // Keep the session alive independently of the shell so exiting the shell doesn't end it.
        spawn_agent(&session, dir.path(), &["/bin/sleep".into(), "300".into()]).unwrap();
        assert!(window_exists(&session, SHELL_WINDOW));

        // The user types `exit`: the window closes entirely rather than leaving a dead pane.
        rmux([
            "send-keys",
            "-t",
            &window_target(&session, SHELL_WINDOW),
            "exit",
            "Enter",
        ])
        .unwrap();
        wait_until(|| !window_exists(&session, SHELL_WINDOW));

        // A finished agent, by contrast, stays observable as a dead pane.
        spawn_agent(
            &session,
            dir.path(),
            &["/bin/sh".into(), "-c".into(), "true".into()],
        )
        .unwrap();
        wait_until(|| {
            list_panes(&session)
                .unwrap_or_default()
                .iter()
                .any(|p| p.window == AGENT_WINDOW && p.dead)
        });
        assert!(
            window_exists(&session, AGENT_WINDOW),
            "the exited agent lingers"
        );

        // ensure_shell brings back a shell to attach to after the user exited theirs.
        ensure_shell(&session, dir.path()).unwrap();
        assert!(window_exists(&session, SHELL_WINDOW));
        assert!(!shell_is_dead(&session), "the recreated shell is live");

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
