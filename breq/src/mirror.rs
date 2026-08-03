//! The local half of a pane mirror: this terminal, drawn from one rmux pane.
//!
//! `breq` used to hand the terminal to `rmux attach`, which brought a whole multiplexer with it —
//! a prefix key, a status bar, a window list, and a pane that would not go away when you typed
//! `exit`. What runs here instead is a thin client of the very pane the browser mirrors: pane
//! bytes to stdout, keystrokes to the pane, `SIGWINCH` to a resize, and nothing else on the
//! screen. Exiting the process returns you to the shell you started from, like `zsh` would.
//!
//! Everything about *what* is on the screen — the screen-paint seed, duplicate query replies, the
//! line a held pane grows when its process exits — lives in `toren-mirror` and is shared with the
//! daemon. What is local is the tty: raw mode, a dedicated reader thread, and the keys a held pane
//! answers to.

use anyhow::{Context, Result};
use std::io::{IsTerminal, Read, Write};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use toren_lib::rmux;
use toren_lib::Place;
use toren_mirror::{MirroredPane, PaneRole};

use nix::sys::termios::{self, SetArg, Termios};

/// Which pane to mirror, and what it is.
pub struct Pane {
    /// Window in the workspace's session. The pane inside it is resolved by id on attach, and
    /// re-resolved after every re-run, because replacing a window's process mints a new pane.
    pub window: String,
    /// What `<ENTER>` offers on a held pane: re-run for a command, resume for an agent.
    pub role: PaneRole,
    /// Whether the pane outlives its process — decided when the window was created (D10), and
    /// repeated here because it decides whether `breq` exits or waits for a key.
    pub hold: bool,
}

/// Put something back in a held pane's window: run the command again, resume the agent session.
///
/// Held panes are the only reason this exists — the mirror knows a key was pressed, not what the
/// window was created to run.
pub type Rerun<'a> = Box<dyn FnMut(&Place) -> Result<()> + 'a>;

/// Whether there is a terminal here to mirror into.
///
/// Both halves matter: pane bytes go to stdout and keystrokes come from stdin, so a redirection
/// either way means this is a script — which wants the command's own output, not a repainted
/// screen.
pub fn owns_terminal() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// Mirror a pane in this terminal until it is done, returning the exit code to leave with.
///
/// Blocks for as long as the pane lives. The tokio runtime is built here rather than around
/// `main` because this is the only thing in `breq` that needs one.
pub fn run(place: &Place, pane: Pane, rerun: Rerun<'_>) -> Result<i32> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("Failed to start the runtime the pane mirror needs")?;

    let _raw = RawTerminal::enter()?;
    runtime.block_on(mirror(place, pane, rerun))
}

async fn mirror(place: &Place, mut pane: Pane, mut rerun: Rerun<'_>) -> Result<i32> {
    let mut rmux_client = toren_mirror::connect().await?;
    let session = place.session_name();
    let mut input = read_stdin();
    let mut winch = signal(SignalKind::window_change())
        .context("Failed to listen for terminal resizes (SIGWINCH)")?;
    let mut out = std::io::stdout();
    let mut reconnected = false;

    loop {
        let attached = async {
            let pane_id =
                toren_mirror::find_window_pane(&rmux_client, &session, &pane.window).await?;
            // On a connection of its own, not this one: rmux caps output subscriptions per
            // connection, and the client here is also what resolves windows.
            MirroredPane::attach(&session, pane_id, pane.role).await
        }
        .await;
        let mirrored = match attached {
            Ok(mirrored) => {
                reconnected = false;
                mirrored
            }
            // A dead client stays dead (any request cancelled mid-flight kills it); one fresh
            // connection is the fix, and a second failure on it is a real error.
            Err(e) if toren_mirror::transport_is_dead(&e) && !reconnected => {
                rmux_client = toren_mirror::connect().await?;
                reconnected = true;
                continue;
            }
            Err(e) => return Err(e),
        };
        // Somebody just typed `breq` in this terminal, which makes it the viewer being worked in
        // — so it takes the pane's size, and any browser tab on the same pane scales to fit
        // instead of fighting it for the PTY. Ownership moves back the moment they type there.
        if let Err(e) = mirrored.claim_size().await {
            tracing::debug!("Failed to claim the pane's size: {:#}", e);
        }
        if let Some((cols, rows)) = terminal_size() {
            let _ = mirrored.resize(cols, rows).await;
        }

        let next = follow(&mirrored, &mut out, &mut input, &mut winch, pane.hold).await?;
        // This terminal is done sizing the pane, whatever happens next. Saying so is what lets a
        // browser tab still watching it resize to its own geometry, without anything having to
        // detect that the terminal went away.
        let _ = mirrored.release_size().await;

        match next {
            Next::Done(code) => return Ok(code),
            Next::Close(code) => {
                // Dismissal is the user saying they have read it; nothing else clears a held pane.
                let _ = rmux::kill_window(&session, &pane.window);
                return Ok(code);
            }
            // The window keeps its name across a re-run, and the loop re-resolves the pane behind
            // it — which is the whole point of never holding on to a pane id or a window index.
            Next::Rerun => {
                drop(mirrored);
                // An agent with nothing to resume, or a window that would not take the command
                // back: the held pane is still there to re-attach to, and saying why is better
                // than tearing the terminal down over a key that did nothing.
                if let Err(e) = rerun(place) {
                    paint(&mut out, format!("\r\n{:#}\r\n", e).as_bytes())?;
                }
            }
            // Dropping to a shell leaves the held pane where it is rather than trading it away: it
            // is still in the browser's window list, and `<Ctrl-c>` is the way to be rid of it.
            // The shell is a fresh one — every shell here is its own, never a mirror of somebody
            // else's.
            Next::Shell => {
                drop(mirrored);
                let window = rmux::open_shell(&session, &place.path)?;
                let _ = rmux::set_hold(&session, &window, false);
                pane = Pane {
                    window,
                    role: PaneRole::Shell,
                    hold: false,
                };
            }
        }
    }
}

/// What ended a pane's turn in the terminal.
enum Next {
    /// The process is gone and nothing holds the pane: `breq` is done.
    Done(i32),
    /// `<ENTER>` on a held pane: run it again in the same window.
    Rerun,
    /// `<ESC>` on a held pane: mirror the workspace's shell instead.
    Shell,
    /// `<Ctrl-c>` on a held pane: dismiss it and go.
    Close(i32),
}

/// Render one pane until it stops being the thing to render.
async fn follow(
    pane: &MirroredPane,
    out: &mut impl Write,
    input: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    winch: &mut tokio::signal::unix::Signal,
    hold: bool,
) -> Result<Next> {
    let mirror = pane.mirror();
    let (backfill, mut live) = mirror.attach().await;
    paint(out, &backfill.bytes)?;

    let mut state = mirror.state();
    let mut text = InputText::default();
    let mut replies = toren_mirror::QueryFilter::inbound();
    // Once the process is gone the keys mean something else, so the pane stops being typed at.
    let mut held: Option<i32> = None;

    loop {
        // Read before waiting: a pane that ended before this client attached never signals a
        // change, because a fresh subscription opens on the value it already has.
        if held.is_none() && mirror.has_ended() {
            let exit = state.borrow_and_update().exit_code();
            let code = drain(out, &mut live, exit)?;
            if !hold {
                return Ok(Next::Done(code));
            }
            held = Some(code);
        }

        tokio::select! {
            chunk = live.recv() => match chunk {
                Ok(frame) => paint(out, &frame.bytes)?,
                // Further behind than the fan-out buffer: what was missed is not in it any more,
                // so take everything the mirror still holds rather than splice a gap.
                Err(RecvError::Lagged(_)) => {
                    let (backfill, fresh) = mirror.attach().await;
                    live = fresh;
                    paint(out, &backfill.bytes)?;
                }
                Err(RecvError::Closed) => return Ok(Next::Done(held.unwrap_or_default())),
            },
            bytes = input.recv() => {
                // Stdin closed under a raw tty means the terminal went away with it.
                let Some(bytes) = bytes else {
                    return Ok(Next::Done(held.unwrap_or_default()));
                };
                match held {
                    Some(code) => if let Some(next) = held_key(&bytes, code) {
                        return Ok(next);
                    },
                    None => {
                        // This terminal answers queries like any other, and rmux has already
                        // answered the pane's. Its replies stop here rather than arriving at a
                        // program that asked once and was told long ago.
                        let text = replies.push_text(&text.push(&bytes));
                        if !text.is_empty() {
                            // Typing is what makes this the active viewer, and the active
                            // viewer is the one the pane takes its size from.
                            let _ = pane.claim_size().await;
                            pane.send_text(&text).await?;
                        }
                    }
                }
            },
            _ = winch.recv() => {
                if let Some((cols, rows)) = terminal_size() {
                    // Only if this terminal is the one sizing the pane. A browser tab that took
                    // ownership is drawing the geometry the app inside is laid out for, and
                    // resizing under it is how the cursor ends up somewhere the UI is not.
                    let _ = pane.resize_as_owner(cols, rows).await;
                }
            },
            changed = state.changed(), if held.is_none() => {
                if changed.is_err() {
                    return Ok(Next::Done(0));
                }
            },
        }
    }
}

/// Everything the pane has left to say, including the status line a held one ends with.
fn drain(
    out: &mut impl Write,
    live: &mut tokio::sync::broadcast::Receiver<toren_mirror::Frame>,
    exit: Option<i32>,
) -> Result<i32> {
    while let Ok(frame) = live.try_recv() {
        paint(out, &frame.bytes)?;
    }
    // A pane that closes with its process leaves no status behind to read; only a held one does.
    Ok(exit.unwrap_or_default())
}

fn paint(out: &mut impl Write, bytes: &[u8]) -> Result<()> {
    out.write_all(bytes)?;
    out.flush()?;
    Ok(())
}

/// What a keystroke means once the pane is held — the three affordances its status line offers.
fn held_key(chunk: &[u8], exit: i32) -> Option<Next> {
    // A lone `ESC` is the drop-to-shell key; an `ESC` opening a longer sequence is an arrow key or
    // a mouse report, which a pane with nothing running in it has no use for.
    if chunk == [0x1b] {
        return Some(Next::Shell);
    }
    chunk.iter().find_map(|byte| match byte {
        b'\r' | b'\n' => Some(Next::Rerun),
        0x03 => Some(Next::Close(exit)),
        _ => None,
    })
}

/// The terminal's current size, as the pane should be sized to match it.
fn terminal_size() -> Option<(u16, u16)> {
    terminal_size::terminal_size().map(|(w, h)| (w.0, h.0))
}

/// Read stdin on an OS thread of its own.
///
/// A raw-tty read cannot be cancelled, so it cannot sit inside a `select!`: the keystroke it is
/// blocked on would be swallowed along with the future the moment anything else fired.
fn read_stdin() -> mpsc::UnboundedReceiver<Vec<u8>> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if tx.send(buf[..read].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

/// Stdin, as much of it as rmux will take.
///
/// The SDK's only input path takes `&str`, so a keystroke has to be valid UTF-8 to be forwarded at
/// all. A character split across two reads waits here for the rest of itself; a byte that can
/// never start one is dropped, because there is nowhere for it to go.
#[derive(Default)]
struct InputText {
    pending: Vec<u8>,
}

impl InputText {
    fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut text = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(valid) => {
                    text.push_str(valid);
                    self.pending.clear();
                    return text;
                }
                Err(e) => {
                    let good = e.valid_up_to();
                    text.push_str(&String::from_utf8_lossy(&self.pending[..good]));
                    match e.error_len() {
                        // Truncated: the rest of the character is in the next read.
                        None => {
                            self.pending.drain(..good);
                            return text;
                        }
                        Some(bad) => {
                            self.pending.drain(..good + bad);
                        }
                    }
                }
            }
        }
    }
}

/// Raw mode, for as long as the mirror owns the terminal.
///
/// The pane's own terminal does the echoing, the line editing and the signal handling; this one
/// must do none of it, or every keystroke happens twice and `Ctrl-C` never reaches the process it
/// was meant for. Restoring on `Drop` is what leaves a usable shell behind on every exit path —
/// clean exit, error, or a dismissed held pane.
struct RawTerminal {
    saved: Termios,
}

impl RawTerminal {
    fn enter() -> Result<Self> {
        let stdin = std::io::stdin();
        let saved = termios::tcgetattr(&stdin).context("Failed to read the terminal's settings")?;
        let mut raw = saved.clone();
        termios::cfmakeraw(&mut raw);
        termios::tcsetattr(&stdin, SetArg::TCSANOW, &raw)
            .context("Failed to put the terminal in raw mode")?;
        Ok(Self { saved })
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        let _ = termios::tcsetattr(std::io::stdin(), SetArg::TCSADRAIN, &self.saved);
        // The pane may have left us in the alternate screen, with the cursor hidden or an SGR
        // half-applied; the shell we return to inherits whatever we do not undo.
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[?1049l\x1b[?25h\x1b[0m\r\n");
        let _ = out.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forwarded(chunks: &[&[u8]]) -> Vec<String> {
        let mut input = InputText::default();
        chunks.iter().map(|c| input.push(c)).collect()
    }

    #[test]
    fn keystrokes_pass_straight_through() {
        assert_eq!(forwarded(&[b"ls -la\r"]), vec!["ls -la\r".to_string()]);
    }

    #[test]
    fn a_character_split_across_reads_waits_for_the_rest_of_itself() {
        // The two halves of "é", as a slow paste or a 1-byte read would deliver them.
        assert_eq!(
            forwarded(&[b"caf\xc3", b"\xa9\r"]),
            vec!["caf".to_string(), "é\r".to_string()]
        );
    }

    #[test]
    fn a_byte_that_can_never_be_text_is_dropped() {
        // 8-bit Meta and latin-1 pastes are unforwardable; what surrounds them still goes.
        assert_eq!(forwarded(&[b"a\xffb"]), vec!["ab".to_string()]);
        assert_eq!(forwarded(&[b"\xff"]), vec![String::new()]);
    }

    #[test]
    fn an_unfinished_character_never_grows_without_bound() {
        let mut input = InputText::default();
        for _ in 0..100 {
            assert_eq!(input.push(b"\xc3"), "");
        }
        assert!(input.pending.len() <= 4, "{:?}", input.pending);
    }

    #[test]
    fn a_held_pane_answers_the_three_keys_its_status_line_offers() {
        assert!(matches!(held_key(b"\r", 0), Some(Next::Rerun)));
        assert!(matches!(held_key(b"\n", 0), Some(Next::Rerun)));
        assert!(matches!(held_key(&[0x1b], 0), Some(Next::Shell)));
        assert!(matches!(held_key(&[0x03], 7), Some(Next::Close(7))));
    }

    #[test]
    fn a_held_pane_ignores_everything_else() {
        assert!(held_key(b"x", 0).is_none());
        // An arrow key is an ESC sequence, not the drop-to-shell key.
        assert!(held_key(b"\x1b[A", 0).is_none());
        assert!(held_key(b"", 0).is_none());
    }

    /// The whole of `breq sh <ws> -- <cmd>`, minus the tty: a command that ran and finished is
    /// still on screen, says how it ended, and leaves with its own exit code when dismissed.
    /// Needs rmux installed.
    #[tokio::test]
    async fn a_finished_command_holds_until_it_is_dismissed() {
        if !rmux::is_available() {
            eprintln!("skipping: rmux not on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let session = rmux::session_name(&format!("breqmirror{}", std::process::id()), "one", None);
        rmux::ensure_session(&session, dir.path(), &[]).unwrap();
        let window = rmux::spawn_command(
            &session,
            dir.path(),
            &[
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo COMMAND-RAN; read _ignored; exit 3".to_string(),
            ],
            true,
        )
        .unwrap();

        let client = toren_mirror::connect().await.unwrap();
        let pane_id = loop {
            if let Ok(id) = toren_mirror::find_window_pane(&client, &session, &window).await {
                break id;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };
        let pane = MirroredPane::attach(&session, pane_id, PaneRole::Shell)
            .await
            .unwrap();

        // Wait for what the command printed, then let it finish: attaching to a pane that is
        // already dead is a different case, and rmux paints its own notice over that one.
        while !String::from_utf8_lossy(&pane.mirror().attach().await.0.bytes)
            .contains("COMMAND-RAN")
        {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        pane.send_text("\n").await.unwrap();

        // The mirror ends when the command does, which is what puts the pane in its held state.
        let mut state = pane.mirror().state();
        while !state.borrow_and_update().is_ended() {
            state.changed().await.unwrap();
        }

        // The dismissal key is waiting before the pane is followed, so nothing here races.
        let (keys, mut input) = mpsc::unbounded_channel();
        keys.send(vec![0x03]).unwrap();
        let mut winch = signal(SignalKind::window_change()).unwrap();
        let mut out: Vec<u8> = Vec::new();

        let next = follow(&pane, &mut out, &mut input, &mut winch, true)
            .await
            .unwrap();

        assert!(matches!(next, Next::Close(3)), "leaves with the exit code");
        let screen = String::from_utf8_lossy(&out);
        assert!(screen.contains("COMMAND-RAN"), "{:?}", screen);
        assert!(
            screen.contains("[exited 3 — <ENTER> re-run"),
            "the held pane says how it ended: {:?}",
            screen
        );

        rmux::kill_session(&session).unwrap();
    }
}
