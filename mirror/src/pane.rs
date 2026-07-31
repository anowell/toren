//! The rmux half of a mirror: one pane followed by [`PaneId`], for as long as it lives.

use anyhow::{anyhow, Context, Result};
use rmux_sdk::{
    Pane, PaneExitState, PaneId, PaneOutputChunk, PaneOutputStart, Rmux, SessionName,
    TerminalSizeSpec,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{debug, warn};

use crate::buffer::PaneMirror;
use crate::filter::QueryFilter;
use crate::held::{held_status_line, PaneRole};
use crate::seed::screen_paint;

/// How often a mirrored pane is asked whether its process is still alive.
///
/// A held pane keeps its output subscription open after the process exits, so nothing else would
/// say it is over. The subscription itself long-polls an order of magnitude more often than this.
const LIVENESS_POLL: Duration = Duration::from_secs(2);

/// The pump's own polling cadence, matching what the SDK's `next()` would use. `poll_once` is
/// driven directly instead because the SDK is not cancel-safe (below), so the backoff has to live
/// where the cancellation point is.
const POLL_FLOOR: Duration = Duration::from_millis(2);
const POLL_CEILING: Duration = Duration::from_millis(50);

/// Connect to the rmux daemon, starting one if needed.
pub async fn connect() -> Result<Arc<Rmux>> {
    Rmux::builder()
        .connect_or_start()
        .await
        .map(Arc::new)
        .context("Failed to reach the rmux daemon. Is `rmux` installed and on PATH?")
}

/// Whether an error means the client's transport is gone for good.
///
/// The SDK's responses are ordered on one connection, so dropping any request future mid-flight
/// aborts the whole client, and every later call on it fails with `BrokenPipe`. Nothing in this
/// crate cancels a request, but anything can — a browser closing an HTTP connection drops the
/// handler mid-await — so holders of a shared client use this to know when to throw it away and
/// connect again.
pub fn transport_is_dead(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<rmux_sdk::RmuxError>(),
            Some(rmux_sdk::RmuxError::Transport { source, .. })
                if matches!(
                    source.kind(),
                    std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::UnexpectedEof
                )
        )
    })
}

/// What rmux says about a pane's process right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneLiveness {
    /// No such pane, or rmux has not observed its process yet.
    Unknown,
    Running,
    /// The process is gone and the pane outlives it, because it was created to hold.
    Exited(PaneExitState),
}

/// One pane as rmux's own formats describe it: which window it is in, and whether its process is
/// still running.
///
/// Deliberately not [`Pane::info`], which reads panes through a tab-separated format carrying
/// `#{pane_start_command}`: a pane spawned with a multi-line argv — an agent handed a task
/// description as its prompt — spills across lines and drops out of that parse entirely. Every
/// field read here is one rmux cannot break a line in, so a pane's identity does not depend on
/// what it was started with.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PaneFacts {
    window: String,
    window_index: u32,
    id: PaneId,
    /// `None` while rmux has not said — which is not the same as a pane known to be alive.
    dead: Option<bool>,
    exit: PaneExitState,
}

const FACTS_FORMAT: &str = "#{window_name}\t#{window_index}\t#{pane_id}\
                            \t#{pane_dead}\t#{pane_dead_status}\t#{pane_dead_signal}";

/// Every pane in a session, in window then pane order.
///
/// A session rmux cannot find is an empty list rather than an error: callers are asking about
/// panes that may well be gone, and a session that has been killed has no panes by definition.
async fn session_panes(rmux: &Rmux, session: &SessionName) -> Result<Vec<PaneFacts>> {
    let run = rmux
        .cmd([
            "list-panes",
            "-t",
            session.as_str(),
            "-s",
            "-F",
            FACTS_FORMAT,
        ])
        .await
        .with_context(|| format!("Failed to list panes in rmux session '{}'", session))?;
    if run.exit != Some(0) {
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&run.stdout)
        .lines()
        .filter_map(parse_facts)
        .collect())
}

async fn pane_facts(
    rmux: &Rmux,
    session: &SessionName,
    pane_id: PaneId,
) -> Result<Option<PaneFacts>> {
    Ok(session_panes(rmux, session)
        .await?
        .into_iter()
        .find(|pane| pane.id == pane_id))
}

fn parse_facts(line: &str) -> Option<PaneFacts> {
    let mut fields = line.split('\t');
    let window = fields.next()?.to_string();
    let window_index = fields.next()?.trim().parse().ok()?;
    let id = parse_pane_id(fields.next()?)?;
    let dead = match fields.next()?.trim() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    };
    let code = fields.next().and_then(|field| field.trim().parse().ok());
    let signal = fields.next().and_then(|field| field.trim().parse().ok());
    // A signal beats a code: a process killed by one has no exit status of its own to report.
    let exit = match (code, signal) {
        (_, Some(signal)) => PaneExitState::from_signal(signal),
        (Some(code), None) => PaneExitState::from_code(code),
        (None, None) => PaneExitState::default(),
    };
    Some(PaneFacts {
        window,
        window_index,
        id,
        dead,
        exit,
    })
}

fn parse_pane_id(field: &str) -> Option<PaneId> {
    Some(PaneId::new(field.trim().strip_prefix('%')?.parse().ok()?))
}

/// The pane running in a named window right now.
///
/// The name is how the window was created and the only thing callers know up front; the id it
/// resolves to is what everything afterwards uses, because indices move and ids do not. Panes
/// come back in window order, so the first match is the window's lowest pane index.
pub async fn find_window_pane(rmux: &Rmux, session: &str, window: &str) -> Result<PaneId> {
    let name = session_name(session)?;
    let panes = session_panes(rmux, &name).await?;
    panes
        .into_iter()
        .find(|pane| pane.window == window)
        .map(|pane| pane.id)
        .ok_or_else(|| anyhow!("rmux session '{}' has no '{}' window", session, window))
}

/// Whether a pane's process is still running, without mirroring it.
pub async fn liveness(rmux: &Rmux, session: &str, pane_id: PaneId) -> Result<PaneLiveness> {
    pane_liveness(rmux, &session_name(session)?, pane_id).await
}

/// One pane, followed by id: its bytes fan out through [`MirroredPane::mirror`], and input,
/// geometry and liveness go back the other way.
///
/// Dropping this stops following the pane and tells every attached client so.
pub struct MirroredPane {
    rmux: Arc<Rmux>,
    session: SessionName,
    pane_id: PaneId,
    pane: Pane,
    mirror: Arc<PaneMirror>,
    pump: JoinHandle<()>,
    /// Cooperative shutdown for the pump. Aborting the task instead would cancel whatever SDK
    /// request it had in flight, and a cancelled request kills the whole shared client (see
    /// [`transport_is_dead`]); the pump only ever stops between completed requests.
    stop: watch::Sender<bool>,
}

impl Drop for MirroredPane {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        // Clients would otherwise wait on a subscription nothing feeds.
        if !self.mirror.has_ended() {
            self.mirror.mark_ended(None);
        }
    }
}

impl MirroredPane {
    /// Start mirroring `pane_id`, seeding the mirror with a paint of the pane's current screen.
    pub async fn attach(
        rmux: Arc<Rmux>,
        session: &str,
        pane_id: PaneId,
        role: PaneRole,
    ) -> Result<Self> {
        let session = session_name(session)?;
        let pane = pane_handle(&rmux, session.clone(), pane_id).await?;
        let mirror = PaneMirror::new(role);
        let (stop, stopped) = watch::channel(false);
        let pump = tokio::spawn(pump(
            rmux.clone(),
            session.clone(),
            pane.clone(),
            pane_id,
            mirror.clone(),
            stopped,
        ));

        Ok(Self {
            rmux,
            session,
            pane_id,
            pane,
            mirror,
            pump,
            stop,
        })
    }

    /// The client this mirror runs on, for holders deciding whether to keep sharing it.
    pub fn client(&self) -> Arc<Rmux> {
        self.rmux.clone()
    }

    pub fn mirror(&self) -> Arc<PaneMirror> {
        self.mirror.clone()
    }

    pub fn pane_id(&self) -> PaneId {
        self.pane_id
    }

    pub fn session(&self) -> &SessionName {
        &self.session
    }

    /// Whether the pump is still following the pane.
    pub fn is_following(&self) -> bool {
        !self.pump.is_finished()
    }

    /// Whether this is still the mirror to show for its pane.
    ///
    /// A held pane's mirror stops pumping but keeps the final screen and the status line, which is
    /// exactly what a client attaching afterwards should see; only a mirror that stopped for some
    /// other reason is worth rebuilding.
    pub fn is_current(&self) -> bool {
        self.is_following() || self.mirror.exit().is_some()
    }

    /// Send input to the pane.
    ///
    /// This is the whole input path: rmux takes text, not bytes, so input that is not valid UTF-8
    /// cannot be forwarded at all. 8-bit Meta, latin-1 pastes and legacy X10 mouse reports past
    /// column 95 are unreachable through a mirror, by construction rather than by omission.
    pub async fn send_text(&self, text: &str) -> Result<()> {
        self.pane.send_text(text).await?;
        Ok(())
    }

    /// Match the pane's geometry to a client's.
    ///
    /// The window is resized first: a pane cannot exceed its window, so resizing the pane alone is
    /// a silent no-op on a window still at rmux's detached default.
    pub async fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let index = self.window_index().await?;
        let session = self.rmux.session(self.session.clone()).await?;
        session.window(index).resize(Some(cols), Some(rows)).await?;
        self.pane.resize(TerminalSizeSpec::new(cols, rows)).await?;
        Ok(())
    }

    pub async fn liveness(&self) -> Result<PaneLiveness> {
        pane_liveness(&self.rmux, &self.session, self.pane_id).await
    }

    /// Put every attached client back on the pane's real screen, returning the epoch the paint
    /// opens.
    ///
    /// What a client whose terminal has gone wrong needs is not the bytes it missed but a
    /// description of where the pane is now — and an epoch its own queue can be measured against,
    /// so the bytes still travelling behind the paint are dropped rather than applied over it.
    pub async fn repaint(&self) -> Result<u32> {
        let paint = screen_paint(&self.rmux, &self.pane, self.pane_id).await?;
        Ok(self.mirror.reseed(paint).await)
    }

    /// The index of the pane's window right now.
    ///
    /// Resolved on every call and never cached: rmux renumbers windows as they come and go, and
    /// there is no way to address one by id, so a remembered index eventually resizes a stranger.
    async fn window_index(&self) -> Result<u32> {
        pane_facts(&self.rmux, &self.session, self.pane_id)
            .await?
            .map(|pane| pane.window_index)
            .ok_or_else(|| anyhow!("rmux pane {} is gone", self.pane_id))
    }
}

/// Pump one pane's output into its mirror until the process is gone.
///
/// Every SDK call here runs to completion before anything else is awaited. The SDK's responses
/// are ordered on the shared connection and a request future dropped mid-flight aborts the whole
/// client, so nothing cancellable — the stop signal, the backoff sleep — may ever race a request.
/// That is why the stream is driven with `poll_once` and an explicit backoff rather than `next()`,
/// whose internal await could not be raced against the liveness clock safely.
async fn pump(
    rmux: Arc<Rmux>,
    session: SessionName,
    pane: Pane,
    pane_id: PaneId,
    mirror: Arc<PaneMirror>,
    mut stopped: watch::Receiver<bool>,
) {
    // Subscribe before painting: a chunk landing between the two is applied twice at worst, where
    // the other order would lose it outright.
    let mut stream = match pane.output_stream_starting_at(PaneOutputStart::Now).await {
        Ok(stream) => stream,
        Err(e) => {
            warn!("Failed to subscribe to pane {} output: {}", pane_id, e);
            mirror.mark_ended(None);
            return;
        }
    };
    reseed(&rmux, &pane, pane_id, &mirror).await;

    let mut filter = QueryFilter::new();
    let mut delay = POLL_FLOOR;
    // Due immediately: the pane may have been adopted after it was already over.
    let mut liveness_due = Instant::now();

    let exit = loop {
        if *stopped.borrow() {
            // The owner is being dropped and marks the mirror ended itself.
            return;
        }

        let chunks = match stream.poll_once().await {
            Ok(chunks) => chunks,
            Err(e) => {
                debug!("Pane {} output stream ended: {}", pane_id, e);
                break exited(&rmux, &session, pane_id).await;
            }
        };

        let mut streamed = false;
        let mut eof = false;
        for chunk in chunks {
            match chunk {
                // An empty chunk is how the subscription says the pane's output is over.
                PaneOutputChunk::Bytes { bytes, .. } if bytes.is_empty() => eof = true,
                PaneOutputChunk::Bytes { bytes, .. } => {
                    streamed = true;
                    let visible = filter.push(&bytes);
                    if !visible.is_empty() {
                        mirror.push(Arc::new(visible)).await;
                    }
                }
                // A gap means the mirror's idea of the screen is wrong, and the bytes that
                // would fix it are the ones that were dropped. Paint it again instead.
                PaneOutputChunk::Lag(notice) => {
                    debug!("rmux dropped pane {} output: {:?}", pane_id, notice);
                    filter.reset();
                    reseed(&rmux, &pane, pane_id, &mirror).await;
                }
                _ => {}
            }
        }
        if eof {
            break exited(&rmux, &session, pane_id).await;
        }
        delay = if streamed {
            POLL_FLOOR
        } else {
            (delay * 2).min(POLL_CEILING)
        };

        // A held pane's stream stays open after its process exits, so ask; a pane that is gone
        // altogether keeps its subscription silent, so this is also what notices that.
        if Instant::now() >= liveness_due {
            liveness_due = Instant::now() + LIVENESS_POLL;
            match followed(&rmux, &session, pane_id).await {
                Ok(Followed::Exited(exit)) => break Some(exit),
                Ok(Followed::Gone) => break None,
                Ok(Followed::Running | Followed::Indeterminate) => {}
                Err(e) => debug!("Failed to read pane {} liveness: {}", pane_id, e),
            }
        }

        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = stopped.changed() => return,
        }
    };

    if let Some(exit) = &exit {
        mirror
            .push(Arc::new(held_status_line(exit, mirror.role())))
            .await;
    }
    mirror.mark_ended(exit);
}

/// What following the pane turned up, distinguishing "the pane no longer exists" from a snapshot
/// that just cannot say yet — a mirror must end on the former and shrug at the latter.
enum Followed {
    Running,
    Exited(PaneExitState),
    Gone,
    Indeterminate,
}

async fn followed(rmux: &Rmux, session: &SessionName, pane_id: PaneId) -> Result<Followed> {
    let Some(facts) = pane_facts(rmux, session, pane_id).await? else {
        return Ok(Followed::Gone);
    };
    Ok(match facts.dead {
        Some(true) => Followed::Exited(facts.exit),
        Some(false) => Followed::Running,
        None => Followed::Indeterminate,
    })
}

async fn reseed(rmux: &Rmux, pane: &Pane, pane_id: PaneId, mirror: &PaneMirror) {
    match screen_paint(rmux, pane, pane_id).await {
        Ok(paint) => {
            mirror.reseed(paint).await;
        }
        Err(e) => debug!("Failed to seed pane {} from its screen: {:#}", pane_id, e),
    }
}

/// The pane's exit status, which rmux only fills in once the process is actually gone.
async fn exited(rmux: &Rmux, session: &SessionName, pane_id: PaneId) -> Option<PaneExitState> {
    match followed(rmux, session, pane_id).await {
        Ok(Followed::Exited(exit)) => Some(exit),
        Ok(_) => None,
        Err(e) => {
            debug!("Failed to read pane {} liveness: {}", pane_id, e);
            None
        }
    }
}

async fn pane_liveness(
    rmux: &Rmux,
    session: &SessionName,
    pane_id: PaneId,
) -> Result<PaneLiveness> {
    Ok(match followed(rmux, session, pane_id).await? {
        Followed::Running => PaneLiveness::Running,
        Followed::Exited(exit) => PaneLiveness::Exited(exit),
        Followed::Gone | Followed::Indeterminate => PaneLiveness::Unknown,
    })
}

async fn pane_handle(rmux: &Rmux, session: SessionName, pane_id: PaneId) -> Result<Pane> {
    let session = rmux.session(session).await?;
    Ok(session.pane_by_id(pane_id).await?)
}

fn session_name(session: &str) -> Result<SessionName> {
    SessionName::new(session).map_err(|e| anyhow!("Invalid rmux session name '{}': {}", session, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_live_pane_off_one_line() {
        let facts = parse_facts("agent\t2\t%7\t0\t\t").unwrap();
        assert_eq!(facts.window, "agent");
        assert_eq!(facts.window_index, 2);
        assert_eq!(facts.id, PaneId::new(7));
        assert_eq!(facts.dead, Some(false));
    }

    #[test]
    fn a_dead_pane_carries_the_status_it_died_with() {
        let facts = parse_facts("agent\t2\t%7\t1\t7\t").unwrap();
        assert_eq!(facts.dead, Some(true));
        assert_eq!(facts.exit, PaneExitState::from_code(7));
    }

    #[test]
    fn a_signal_is_the_whole_story_of_an_exit() {
        // rmux reports a status alongside the signal; the signal is what actually ended it.
        let facts = parse_facts("agent\t2\t%7\t1\t0\t9").unwrap();
        assert_eq!(facts.exit, PaneExitState::from_signal(9));
    }

    #[test]
    fn a_pane_rmux_cannot_speak_for_is_neither_alive_nor_dead() {
        let facts = parse_facts("agent\t2\t%7\t\t\t").unwrap();
        assert_eq!(facts.dead, None);
    }

    /// The failure this whole path exists to avoid: a pane started with a multi-line argv puts
    /// the tail of its own start command on lines of its own. Those lines are not panes, and
    /// mistaking one for a pane is how a window loses its id.
    #[test]
    fn a_line_that_is_not_a_pane_is_dropped_rather_than_guessed_at() {
        assert!(parse_facts("## Description").is_none());
        assert!(parse_facts("").is_none());
        assert!(parse_facts("agent\tsecond\t%7\t0\t\t").is_none());
        assert!(parse_facts("agent\t2\tnot-a-pane-id\t0\t\t").is_none());
    }
}
