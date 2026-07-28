//! The line a pane grows when its process exits but the pane is held.
//!
//! `remain-on-exit` keeps a finished pane addressable but draws nothing, so the exit status and
//! what to do about it are ours to render. Drawing it into the pane's byte stream rather than as
//! surface chrome means the browser, the local tty, and anything later get it for free; only the
//! key handling is per-surface.

use rmux_sdk::PaneExitState;

/// What a pane was created to run, which decides what `<ENTER>` offers once it has exited.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PaneRole {
    /// A shell or a one-shot command: `<ENTER>` runs it again.
    #[default]
    Shell,
    /// A coding agent: its session id is known, so `<ENTER>` resumes that session rather than
    /// starting the agent cold.
    Agent,
}

/// The dim line an exited-but-held pane shows, with the exit status and the keys that act on it.
pub fn held_status_line(exit: &PaneExitState, role: PaneRole) -> Vec<u8> {
    let status = match exit.code {
        Some(code) => format!("exited {}", code),
        None => "exited".to_string(),
    };
    let rerun = match role {
        PaneRole::Shell => "re-run",
        PaneRole::Agent => "resume",
    };
    format!(
        "\r\n\x1b[2m[{} — <ENTER> {}, <ESC> drop to shell, <Ctrl-c> close]\x1b[0m",
        status, rerun
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(exit: &PaneExitState, role: PaneRole) -> String {
        String::from_utf8(held_status_line(exit, role)).unwrap()
    }

    #[test]
    fn shows_the_exit_code_and_the_three_affordances() {
        assert_eq!(
            line(&PaneExitState::from_code(0), PaneRole::Shell),
            "\r\n\x1b[2m[exited 0 — <ENTER> re-run, <ESC> drop to shell, <Ctrl-c> close]\x1b[0m"
        );
    }

    #[test]
    fn an_agent_pane_resumes_rather_than_re_running() {
        assert!(line(&PaneExitState::from_code(1), PaneRole::Agent).contains("<ENTER> resume,"));
    }

    #[test]
    fn falls_back_to_bare_exited_without_a_code() {
        let killed = PaneExitState::from_signal(9);
        assert!(line(&killed, PaneRole::Shell).contains("[exited —"));
    }
}
