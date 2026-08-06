//! The rmux half of a mirror: one pane followed by [`PaneId`], for as long as it lives.

use anyhow::{anyhow, Context, Result};
use rmux_sdk::{
    Pane, PaneExitState, PaneId, PaneOutputChunk, PaneOutputStart, PaneStateEvent,
    PaneStateEventsOptions, Rmux, SessionName, TerminalSizeSpec,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::buffer::PaneMirror;
use crate::filter::QueryFilter;
use crate::held::{held_status_line, PaneRole};
use crate::seed::screen_paint;

/// The pump's own polling cadence, matching what the SDK's `next()` would use. `poll_once` is
/// driven directly instead because the SDK is not cancel-safe (below), so the backoff has to live
/// where the cancellation point is.
const POLL_FLOOR: Duration = Duration::from_millis(2);
const POLL_CEILING: Duration = Duration::from_millis(50);

/// How long a mirror waits before trying to follow its pane again, after rmux refused.
///
/// A refused subscription means a full connection, not a dead pane, and the slot frees when some
/// other mirror is evicted — so it waits and asks again rather than declaring the pane over.
const RETRY_FLOOR: Duration = Duration::from_millis(250);
const RETRY_CEILING: Duration = Duration::from_secs(5);

/// The pane-local option a mirror writes to say it is the one sizing the pane.
///
/// One PTY has one size, and a mirror puts two viewers with different geometries in front of it —
/// a terminal window and a browser tab. Both resizing it meant last-write-wins, and the loser
/// rendered a screen laid out for the winner: the cursor at the bottom of a pane whose UI is
/// drawn higher up. Arbitration needs a record of who the writer is, and it has to live somewhere
/// both *processes* can read, because `breq` and the daemon mirror the same pane from different
/// ones. The pane itself is that place.
const SIZE_OWNER_OPTION: &str = "@toren-size-owner";

/// How long a mirror believes its own claim without asking again.
///
/// Claims are made on every burst of input, which is what makes ownership follow whoever is
/// actually working (tmux's `window-size latest`, arrived at the same way). Writing the option on
/// every keystroke would be a round trip per keystroke; believing a recent claim costs nothing and
/// is wrong for at most this long.
const CLAIM_TTL: Duration = Duration::from_secs(5);

/// How long any one request to rmux may take before it is given up on.
///
/// The SDK's responses are ordered on a connection, so a request with no deadline can stall every
/// request queued behind it for as long as the daemon is busy. Nothing a mirror asks for is worth
/// waiting longer than this, and giving up frees the queue.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Connect to the rmux daemon, starting one if needed.
///
/// Every connection carries a request deadline: without one, a single slow request blocks the
/// ordered queue behind it indefinitely.
pub async fn connect() -> Result<Arc<Rmux>> {
    Rmux::builder()
        .default_timeout(REQUEST_TIMEOUT)
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

/// One pane as rmux's own inventory describes it: which window it is in, and whether its process
/// is still running.
///
/// Read through `Rmux::cmd` with [`FACTS_FORMAT`] rather than through [`Pane::info`], although
/// the fork costs more than the socket round trip: the info snapshot's wire format carries
/// `#{pane_start_command}`, a field the pane's own argv can break a line in. An agent handed a
/// multi-line prompt vanishes from that snapshot entirely, and a pane that cannot be parsed is
/// indistinguishable from a pane that is gone — which ended live mirrors. Liveness is pushed
/// (`state_events`), so this read runs on attach and on rare fallbacks, not on a clock.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PaneFacts {
    id: PaneId,
    window_index: u32,
    /// The pane's grid, which is what a viewer that does not size it renders at.
    size: (u16, u16),
    /// `None` while rmux has not said — which is not the same as a pane known to be alive.
    dead: Option<bool>,
    exit: PaneExitState,
}

/// The pane rmux is running in a named window right now.
///
/// The name is how the window was created and the only thing callers know up front; the id it
/// resolves to is what everything afterwards uses, because indices move and ids do not.
///
/// No socket equivalent in the SDK — a window can only be addressed by index, and only
/// `list-panes` reports the name alongside — so it forks. It runs once per attach rather than on
/// any repeating clock, which is what makes that acceptable.
pub async fn find_window_pane(rmux: &Rmux, session: &str, window: &str) -> Result<PaneId> {
    let name = session_name(session)?;
    let run = rmux
        .cmd([
            "list-panes",
            "-t",
            name.as_str(),
            "-s",
            "-F",
            "#{window_name}\t#{pane_id}",
        ])
        .await
        .with_context(|| format!("Failed to list panes in rmux session '{}'", session))?;
    if run.exit != Some(0) {
        return Err(anyhow!("rmux session '{}' has no panes", session));
    }
    String::from_utf8_lossy(&run.stdout)
        .lines()
        .filter_map(parse_window_pane)
        .find(|(name, _)| name == window)
        .map(|(_, id)| id)
        .ok_or_else(|| anyhow!("rmux session '{}' has no '{}' window", session, window))
}

/// One `#{window_name}\t#{pane_id}` line.
///
/// A pane spawned with a multi-line argv — an agent handed a task description as its prompt —
/// puts the tail of that argv on lines of its own. Those lines are not panes, and mistaking one
/// for a pane is how a window loses its id, so anything that does not parse is dropped.
fn parse_window_pane(line: &str) -> Option<(String, PaneId)> {
    let (window, pane) = line.split_once('\t')?;
    Some((window.to_string(), parse_pane_id(pane)?))
}

fn parse_pane_id(field: &str) -> Option<PaneId> {
    Some(PaneId::new(field.trim().strip_prefix('%')?.parse().ok()?))
}

/// Whether a pane's process is still running, without mirroring it.
pub async fn liveness(rmux: &Rmux, session: &str, pane_id: PaneId) -> Result<PaneLiveness> {
    Ok(
        match followed(rmux, &session_name(session)?, pane_id).await? {
            Followed::Running => PaneLiveness::Running,
            Followed::Exited(exit) => PaneLiveness::Exited(exit),
            Followed::Gone | Followed::Indeterminate => PaneLiveness::Unknown,
        },
    )
}

/// One pane, followed by id: its bytes fan out through [`MirroredPane::mirror`], and input,
/// geometry and liveness go back the other way.
///
/// A mirror owns its rmux connection. rmux caps output subscriptions at sixteen per connection,
/// so mirrors that share one starve each other once a workspace has more panes than that — which
/// is the whole of "live pane shown as exited and being held". Connections are cheap and the SDK
/// opens extra ones itself, so a mirror takes its own and the cap stops being reachable.
///
/// Dropping this stops following the pane and tells every attached client so.
pub struct MirroredPane {
    rmux: Arc<Rmux>,
    session: SessionName,
    pane_id: PaneId,
    pane: Pane,
    mirror: Arc<PaneMirror>,
    pump: JoinHandle<()>,
    /// Watches the pane's process, on a transport of its own, and pushes what it sees.
    watcher: JoinHandle<()>,
    /// What this mirror calls itself in the pane's size-owner note. Unique per mirror per
    /// process, so a terminal and a browser tab can tell each other's claims apart.
    token: String,
    /// When this mirror last wrote its own claim, so it need not keep writing it.
    claimed: tokio::sync::Mutex<Option<tokio::time::Instant>>,
    /// Cooperative shutdown for both tasks. Aborting them instead would cancel whatever SDK
    /// request was in flight, and a cancelled request kills the whole client (see
    /// [`transport_is_dead`]); they only ever stop between completed requests.
    stop: watch::Sender<bool>,
}

impl Drop for MirroredPane {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        self.watcher.abort();
        // Clients would otherwise wait on a subscription nothing feeds.
        if !self.mirror.has_ended() {
            self.mirror.mark_ended(None);
        }
    }
}

impl MirroredPane {
    /// Start mirroring `pane_id` on a connection of this mirror's own.
    pub async fn attach(session: &str, pane_id: PaneId, role: PaneRole) -> Result<Self> {
        Self::attach_on(connect().await?, session, pane_id, role).await
    }

    /// Start mirroring `pane_id` on the supplied connection.
    ///
    /// For callers that have already made one connection per mirror themselves; everything else
    /// wants [`Self::attach`], which is the same thing with the connection made for it.
    pub async fn attach_on(
        rmux: Arc<Rmux>,
        session: &str,
        pane_id: PaneId,
        role: PaneRole,
    ) -> Result<Self> {
        let session = session_name(session)?;
        let pane = pane_handle(&rmux, session.clone(), pane_id).await?;
        let mirror = PaneMirror::new(role);
        let (stop, stopped) = watch::channel(false);

        // What the process is doing, pushed rather than polled. The stream carries its own
        // transport, so it neither takes an output-subscription slot nor queues behind the
        // pump's requests.
        let (exit_tx, exit_rx) = watch::channel(None);
        let watcher = tokio::spawn(watch_lifecycle(
            rmux.clone(),
            session.clone(),
            pane.clone(),
            pane_id,
            exit_tx,
            stopped.clone(),
        ));
        let pump = tokio::spawn(pump(
            rmux.clone(),
            session.clone(),
            pane.clone(),
            pane_id,
            mirror.clone(),
            stopped,
            exit_rx,
        ));

        Ok(Self {
            rmux,
            session,
            pane_id,
            pane,
            mirror,
            pump,
            watcher,
            token: mirror_token(),
            claimed: tokio::sync::Mutex::new(None),
            stop,
        })
    }

    /// The client this mirror runs on, for holders deciding whether it is still usable.
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
    /// exactly what a client attaching afterwards should see — as long as the pane is still there.
    /// Once the runner has retired it, or the pane it was following has been replaced, the mirror
    /// is stale and asking for the pane again must build a new one.
    pub fn is_current(&self) -> bool {
        if self.mirror.has_ended() {
            // An ended mirror stays showable, but only until something asks for it to be rebuilt:
            // the runner drops it from what it tracks, so `ensure_current` mints a fresh one for
            // whatever is in the window now. Answering "current" forever is what left a resumed
            // agent showing the corpse of the one before it.
            return false;
        }
        self.is_following()
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

    /// Match the pane's geometry to a client's, whoever that client is.
    ///
    /// The window is resized first: a pane cannot exceed its window, so resizing the pane alone is
    /// a silent no-op on a window still at rmux's detached default.
    ///
    /// This is the mechanism with no policy attached — it writes the size it is given. Anything
    /// with more than one viewer wants [`Self::resize_as_owner`].
    pub async fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let index = self.window_index().await?;
        let session = self.rmux.session(self.session.clone()).await?;
        session.window(index).resize(Some(cols), Some(rows)).await?;
        self.pane.resize(TerminalSizeSpec::new(cols, rows)).await?;
        Ok(())
    }

    /// Resize the pane only if this mirror is the one entitled to, and say whether it did.
    ///
    /// An unowned pane is taken rather than left alone: somebody has to size it, and the first
    /// viewer to ask is as good a choice as any. An owned one is left to its owner, and the
    /// viewer that asked scales what it has instead of fighting for the PTY.
    pub async fn resize_as_owner(&self, cols: u16, rows: u16) -> Result<bool> {
        match self.size_owner().await? {
            Some(owner) if owner != self.token => return Ok(false),
            Some(_) => {}
            // Nobody is sizing it, so this viewer now is.
            None => self.claim_size().await?,
        }
        self.resize(cols, rows).await?;
        Ok(true)
    }

    /// Become the mirror that sizes this pane.
    ///
    /// Called when a viewer does something only an active viewer does — types, or asks for the
    /// size outright. Cheap to call on every keystroke: a claim this mirror already believes in
    /// is not written again.
    pub async fn claim_size(&self) -> Result<()> {
        {
            let believed = self.claimed.lock().await;
            if believed.is_some_and(|at| at.elapsed() < CLAIM_TTL) {
                return Ok(());
            }
        }
        self.pane
            .set_option(SIZE_OWNER_OPTION, self.token.clone())
            .await?;
        *self.claimed.lock().await = Some(tokio::time::Instant::now());
        Ok(())
    }

    /// Stop sizing this pane, so the next viewer to ask can.
    ///
    /// This is what makes "the terminal went away, resize to the browser" work without anything
    /// having to notice that the terminal went away: the departing viewer says so on its way out.
    /// An owner that vanishes without releasing is not fatal either — one keystroke in any other
    /// viewer takes ownership from it.
    pub async fn release_size(&self) -> Result<()> {
        *self.claimed.lock().await = None;
        if self.size_owner().await?.as_deref() == Some(self.token.as_str()) {
            self.pane.unset_option(SIZE_OWNER_OPTION).await?;
        }
        Ok(())
    }

    /// Whether this mirror is the one sizing the pane right now.
    pub async fn owns_size(&self) -> Result<bool> {
        Ok(match self.size_owner().await? {
            Some(owner) => owner == self.token,
            None => false,
        })
    }

    async fn size_owner(&self) -> Result<Option<String>> {
        Ok(self.pane.option(SIZE_OWNER_OPTION).await?)
    }

    /// The pane's grid as rmux has it, which is what a passive viewer scales itself to.
    ///
    /// Read off the pane's facts rather than a `snapshot()`: a snapshot is the whole cell grid,
    /// tens of kilobytes, and this wants two numbers out of it.
    pub async fn size(&self) -> Result<(u16, u16)> {
        facts(&self.rmux, &self.session, self.pane_id)
            .await?
            .map(|facts| facts.size)
            .ok_or_else(|| anyhow!("rmux pane {} is gone", self.pane_id))
    }

    pub async fn liveness(&self) -> Result<PaneLiveness> {
        Ok(
            match followed(&self.rmux, &self.session, self.pane_id).await? {
                Followed::Running => PaneLiveness::Running,
                Followed::Exited(exit) => PaneLiveness::Exited(exit),
                Followed::Gone | Followed::Indeterminate => PaneLiveness::Unknown,
            },
        )
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
        facts(&self.rmux, &self.session, self.pane_id)
            .await?
            .map(|facts| facts.window_index)
            .ok_or_else(|| anyhow!("rmux pane {} is gone", self.pane_id))
    }
}

/// What the pane's process did, once it has done it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Terminal {
    exit: Option<PaneExitState>,
}

/// Follow one pane's process state until it reaches a terminal one, then say so once.
async fn watch_lifecycle(
    rmux: Arc<Rmux>,
    session: SessionName,
    pane: Pane,
    pane_id: PaneId,
    exit: watch::Sender<Option<Terminal>>,
    mut stopped: watch::Receiver<bool>,
) {
    tokio::select! {
        _ = await_terminal(&rmux, &session, &pane, pane_id) => {}
        _ = stopped.changed() => return,
    }
    // The watch says *that* the process is gone, not what it left behind; the status comes from
    // the pane's own facts, asked for once.
    let _ = exit.send(Some(Terminal {
        exit: exited(&rmux, &session, pane_id).await,
    }));
}

/// Block until rmux says the pane's process is gone, without mirroring the pane.
///
/// For anything that wants to know when a pane dies but has no reason to be showing it — which
/// is most panes most of the time, and exactly the ones whose deaths used to go unnoticed until
/// somebody opened them.
pub async fn wait_until_exited(rmux: &Rmux, session: &str, pane_id: PaneId) -> Result<()> {
    let session = session_name(session)?;
    let pane = pane_handle(rmux, session.clone(), pane_id).await?;
    await_terminal(rmux, &session, &pane, pane_id).await;
    Ok(())
}

/// Wait for the pane's process to end, pushed if rmux will push it and asked for if it will not.
///
/// This is what replaced asking rmux every two seconds whether each mirrored pane was still
/// alive. That question went out as `list-panes` through `Rmux::cmd`, which forks the rmux binary
/// and lists every pane in the session — so fifteen mirrors meant roughly seven process spawns a
/// second, every one of them answering "still running".
///
/// `state_events` carries a transport of its own, takes no output-subscription slot, and reports
/// `Closed` when the process goes — including `DiedKept`, the held pane whose process died while
/// the pane stayed. That last case is why this is not `Pane::wait_for_exit`, which polls
/// `info()` on a clock of its own and carries a timeout.
async fn await_terminal(rmux: &Rmux, session: &SessionName, pane: &Pane, pane_id: PaneId) {
    // Subscribe first, then ask. The stream only reports what happens after it opens, so a pane
    // that was already over before anyone looked — a browser reloading onto an agent that finished
    // while the tab was shut — would wait for an event that had already been and gone. Asking
    // afterwards catches that; subscribing beforehand catches a pane that dies between the two.
    let subscribed = pane
        .state_events(PaneStateEventsOptions {
            include_title: false,
            include_options: false,
            include_foreground: false,
        })
        .await;

    match followed(rmux, session, pane_id).await {
        Ok(Followed::Exited(_) | Followed::Gone) => return,
        Ok(_) => {}
        Err(e) => debug!("Failed to read pane {} liveness: {}", pane_id, e),
    }

    match subscribed {
        Ok(mut events) => loop {
            match events.next().await {
                Ok(Some(PaneStateEvent::Closed { .. })) => return,
                Ok(Some(_)) => {}
                // The subscription ended without a verdict. Fall through and ask.
                Ok(None) | Err(_) => break,
            }
        },
        Err(e) => debug!(
            "Pane {} will not push its state ({}); asking on a clock instead",
            pane_id, e
        ),
    }

    const POLL: Duration = Duration::from_secs(2);
    loop {
        match followed(rmux, session, pane_id).await {
            Ok(Followed::Exited(_) | Followed::Gone) => return,
            Ok(_) => {}
            Err(e) => debug!("Failed to read pane {} liveness: {}", pane_id, e),
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Pump one pane's output into its mirror until the process is gone.
///
/// Every SDK call here runs to completion before anything else is awaited. The SDK's responses
/// are ordered on the connection and a request future dropped mid-flight aborts the whole client,
/// so nothing cancellable — the stop signal, the backoff sleep — may ever race a request. That is
/// why the stream is driven with `poll_once` and an explicit backoff rather than `next()`, whose
/// internal await could not be raced against the stop signal safely.
async fn pump(
    rmux: Arc<Rmux>,
    session: SessionName,
    pane: Pane,
    pane_id: PaneId,
    mirror: Arc<PaneMirror>,
    mut stopped: watch::Receiver<bool>,
    mut exited_at: watch::Receiver<Option<Terminal>>,
) {
    let mut retry = RETRY_FLOOR;

    let terminal = loop {
        if *stopped.borrow() {
            // The owner is being dropped and marks the mirror ended itself.
            return;
        }
        // The watcher may have got there first — a pane adopted after it was already over.
        if let Some(terminal) = exited_at.borrow_and_update().clone() {
            break Some(terminal);
        }

        // Subscribe before painting: a chunk landing between the two is applied twice at worst,
        // where the other order would lose it outright.
        let stream = match pane.output_stream_starting_at(PaneOutputStart::Now).await {
            Ok(stream) => stream,
            Err(e) => {
                // A refused subscription says nothing about the pane's process. rmux caps them
                // per connection, and a mirror that lost the race for a slot is looking at a
                // live pane it cannot currently read — so ask what the pane is actually doing
                // before telling anyone it is over.
                match followed(&rmux, &session, pane_id).await {
                    Ok(Followed::Exited(state)) => break Some(Terminal { exit: Some(state) }),
                    Ok(Followed::Gone) => break Some(Terminal { exit: None }),
                    Ok(Followed::Running | Followed::Indeterminate) | Err(_) => {}
                }
                warn!(
                    "Pane {} is running but cannot be followed ({}); retrying in {:?}",
                    pane_id, e, retry
                );
                mirror.mark_degraded(format!("{}", e));
                tokio::select! {
                    _ = tokio::time::sleep(retry) => {}
                    _ = stopped.changed() => return,
                    _ = exited_at.changed() => {}
                }
                retry = (retry * 2).min(RETRY_CEILING);
                continue;
            }
        };

        retry = RETRY_FLOOR;
        mirror.mark_live();
        match follow(
            &rmux,
            &session,
            stream,
            &pane,
            pane_id,
            &mirror,
            &mut stopped,
            &mut exited_at,
        )
        .await
        {
            Following::Stopped => return,
            Following::Ended(terminal) => break Some(terminal),
            // The subscription dropped under a pane that is still alive: take another.
            Following::Interrupted => continue,
        }
    };

    let exit = terminal.and_then(|terminal| terminal.exit);
    if let Some(exit) = &exit {
        mirror
            .push(Arc::new(held_status_line(exit, mirror.role())))
            .await;
    }
    mirror.mark_ended(exit);
}

/// Why the pump stopped reading one subscription.
enum Following {
    /// The owner asked it to stop.
    Stopped,
    /// The pane's process is over.
    Ended(Terminal),
    /// The subscription is gone and the pane is not: subscribe again.
    Interrupted,
}

/// Read one subscription for as long as it lasts.
#[allow(clippy::too_many_arguments)]
async fn follow(
    rmux: &Rmux,
    session: &SessionName,
    mut stream: rmux_sdk::PaneOutputStream,
    pane: &Pane,
    pane_id: PaneId,
    mirror: &Arc<PaneMirror>,
    stopped: &mut watch::Receiver<bool>,
    exited_at: &mut watch::Receiver<Option<Terminal>>,
) -> Following {
    reseed(rmux, pane, pane_id, mirror).await;

    let mut filter = QueryFilter::new();
    let mut delay = POLL_FLOOR;

    loop {
        if *stopped.borrow() {
            return Following::Stopped;
        }

        let chunks = match stream.poll_once().await {
            Ok(chunks) => chunks,
            Err(e) => {
                debug!("Pane {} output stream ended: {}", pane_id, e);
                return match followed(rmux, session, pane_id).await {
                    Ok(Followed::Exited(state)) => Following::Ended(Terminal { exit: Some(state) }),
                    Ok(Followed::Gone) => Following::Ended(Terminal { exit: None }),
                    _ => Following::Interrupted,
                };
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
                    reseed(rmux, pane, pane_id, mirror).await;
                }
                _ => {}
            }
        }
        if eof {
            return match exited(rmux, session, pane_id).await {
                exit @ Some(_) => Following::Ended(Terminal { exit }),
                None => Following::Ended(Terminal { exit: None }),
            };
        }
        // The watcher pushes the verdict; nothing here has to ask for it on a clock.
        if let Some(terminal) = exited_at.borrow_and_update().clone() {
            return Following::Ended(terminal);
        }

        // A mirror nobody is reading still drains its subscription — rmux retains output in a
        // bounded ring, and letting that overflow costs a screen paint — but it does so on the
        // slow clock. Chasing a firehose at two milliseconds for an audience of nobody is most of
        // what made a workspace full of panes feel heavy.
        delay = if streamed && mirror.viewers() > 0 {
            POLL_FLOOR
        } else {
            (delay * 2).min(POLL_CEILING)
        };

        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = stopped.changed() => return Following::Stopped,
            _ = exited_at.changed() => {}
        }
    }
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
    let Some(facts) = facts(rmux, session, pane_id).await? else {
        return Ok(Followed::Gone);
    };
    Ok(match facts.dead {
        Some(true) => Followed::Exited(facts.exit),
        Some(false) => Followed::Running,
        None => Followed::Indeterminate,
    })
}

/// Only fields rmux cannot break a line in — see [`PaneFacts`] for why `#{pane_start_command}`,
/// and with it [`Pane::info`], is off limits.
const FACTS_FORMAT: &str = "#{window_index}\t#{pane_id}\t#{pane_dead}\
                            \t#{pane_dead_status}\t#{pane_dead_signal}\
                            \t#{pane_width}\t#{pane_height}";

/// The pane's window and process state, as `list-panes` reports them.
async fn facts(rmux: &Rmux, session: &SessionName, pane_id: PaneId) -> Result<Option<PaneFacts>> {
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
    // A session rmux cannot find has no panes: the pane is gone, not unreadable.
    if run.exit != Some(0) {
        return Ok(None);
    }
    Ok(String::from_utf8_lossy(&run.stdout)
        .lines()
        .filter_map(parse_facts)
        .find(|facts| facts.id == pane_id))
}

/// One `FACTS_FORMAT` line. Anything that does not parse is the tail of some pane's multi-line
/// argv, not a pane, and is dropped — the same judgement as [`parse_window_pane`].
fn parse_facts(line: &str) -> Option<PaneFacts> {
    let mut fields = line.split('\t');
    let window_index = fields.next()?.trim().parse().ok()?;
    let id = parse_pane_id(fields.next()?)?;
    let dead = match fields.next()?.trim() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    };
    let code = fields.next()?.trim().parse().ok();
    let signal = fields.next()?.trim().parse().ok();
    let cols = fields.next()?.trim().parse().ok()?;
    let rows = fields.next()?.trim().parse().ok()?;
    // A signal beats a code: a process killed by one has no exit status of its own to report.
    let exit = match (code, signal) {
        (_, Some(signal)) => PaneExitState::from_signal(signal),
        (Some(code), None) => PaneExitState::from_code(code),
        (None, None) => PaneExitState::default(),
    };
    Some(PaneFacts {
        id,
        window_index,
        size: (cols, rows),
        dead,
        exit,
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

async fn pane_handle(rmux: &Rmux, session: SessionName, pane_id: PaneId) -> Result<Pane> {
    let session = rmux.session(session).await?;
    Ok(session.pane_by_id(pane_id).await?)
}

/// A name no other mirror answers to, in this process or any other.
fn mirror_token() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}:{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn session_name(session: &str) -> Result<SessionName> {
    SessionName::new(session).map_err(|e| anyhow!("Invalid rmux session name '{}': {}", session, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_window_and_its_pane_off_one_line() {
        let (window, id) = parse_window_pane("agent\t%7").unwrap();
        assert_eq!(window, "agent");
        assert_eq!(id, PaneId::new(7));
    }

    /// The failure this parse exists to avoid: a pane started with a multi-line argv puts the
    /// tail of its own start command on lines of its own. Those lines are not panes, and
    /// mistaking one for a pane is how a window loses its id.
    #[test]
    fn a_line_that_is_not_a_pane_is_dropped_rather_than_guessed_at() {
        assert!(parse_window_pane("## Description").is_none());
        assert!(parse_window_pane("").is_none());
        assert!(parse_window_pane("agent\tnot-a-pane-id").is_none());
        assert!(parse_window_pane("agent\t7").is_none());
    }

    #[test]
    fn a_live_pane_reads_as_not_dead() {
        let facts = parse_facts("2\t%7\t0\t\t\t80\t24").unwrap();
        assert_eq!(facts.id, PaneId::new(7));
        assert_eq!(facts.window_index, 2);
        assert_eq!(facts.dead, Some(false));
        assert_eq!(facts.size, (80, 24));
    }

    #[test]
    fn a_dead_pane_carries_the_status_it_died_with() {
        let facts = parse_facts("1\t%3\t1\t7\t\t80\t24").unwrap();
        assert_eq!(facts.dead, Some(true));
        assert_eq!(facts.exit, PaneExitState::from_code(7));

        let signalled = parse_facts("1\t%3\t1\t\t9\t80\t24").unwrap();
        assert_eq!(signalled.exit, PaneExitState::from_signal(9));
    }

    /// Why facts come from `FACTS_FORMAT` and not `Pane::info`: an agent handed a multi-line
    /// prompt splits its own listing row wherever the prompt breaks. Those fragments must be
    /// dropped, not read as a pane — and above all must not make a live pane look gone, which
    /// is what froze every mirror of a `breq do <task>` agent (tor-qze).
    #[test]
    fn the_tail_of_a_multi_line_argv_is_not_a_pane() {
        assert!(parse_facts("").is_none());
        assert!(parse_facts("and an approved justifyViolations").is_none());
        assert!(parse_facts("2\t%7\t0\t\t\t80").is_none());
        assert!(parse_facts("2\tnot-an-id\t0\t\t\t80\t24").is_none());
    }
}
