//! Runs coding agents inside rmux panes and mirrors those panes to connected browsers.
//!
//! Agents are spawned into the same sessions `breq do` uses, so either side can attach to the
//! other's agent without re-spawning it.
//!
//! The mirroring itself lives in `toren-mirror`, which the local `breq` client uses too; what is
//! here is the daemon's bookkeeping, and it is three things:
//!
//! * **Which panes are mirrored, and for whom.** A mirror costs an rmux connection, an output
//!   subscription and a pump, so it exists while somebody is watching and not a moment longer.
//!   Mirrors used to accumulate for the life of the daemon, which is how a workspace ran out of
//!   the sixteen output subscriptions a connection gets.
//! * **Which viewer's geometry reaches the PTY.** One pane, one size, several viewers; the one
//!   that typed most recently wins and the rest scale what they have.
//! * **What every pane's process is doing**, whether or not anyone is looking. Watching is
//!   separate from mirroring on purpose: a pane nobody has open is exactly the pane whose death
//!   used to go unnoticed until somebody opened it.

use anyhow::{anyhow, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, watch, Mutex, RwLock};
use toren_mirror::{
    connect, find_window_pane, transport_is_dead, MirroredPane, PaneLiveness, PaneMirror, PaneRole,
    Rmux,
};
use tracing::{debug, info, warn};

use toren_lib::rmux as rmux_conv;

/// How long a mirror outlives its last viewer.
///
/// Switching tabs, reloading a page and reconnecting a dropped socket all detach and re-attach
/// within a second or two, and rebuilding a mirror means a fresh subscription and a fresh screen
/// paint. Lingering absorbs that churn; anything longer is just holding a subscription for nobody.
const MIRROR_LINGER: Duration = Duration::from_secs(30);

/// How often mirrors nobody is watching are swept up.
const SWEEP_INTERVAL: Duration = Duration::from_secs(10);

/// How long resizes are collected before one of them is applied.
///
/// A browser's `ResizeObserver` fires through a drag, and a terminal emits `SIGWINCH` the same
/// way. Only the size it settles on matters, and every one before it is a round trip to rmux and
/// a `SIGWINCH` to the app inside the pane.
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(120);

/// How many lifecycle events are held for a subscriber that is briefly behind.
const LIFECYCLE_DEPTH: usize = 256;

/// One viewer of one mirrored pane, for as long as it is attached.
///
/// Handed out by [`PaneRunner::attach_viewer`] and released by [`PaneRunner::detach_viewer`]. The
/// id is what makes size ownership answerable — "the viewer that typed last" needs viewers to
/// have names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewerId(u64);

/// What the daemon tracks for one window of one rmux session.
struct TrackedPane {
    session: String,
    /// Held behind an `Arc` so a caller can take the mirror out from under the lock before
    /// talking to rmux. Everything here is one `RwLock`, and an rmux round trip is milliseconds
    /// at best and a ten-second deadline at worst; holding the lock across one would stall every
    /// other pane behind it.
    pane: Arc<MirroredPane>,
    /// Everything currently watching this mirror, which is what keeps it from being swept.
    viewers: Vec<ViewerId>,
    /// The viewer whose geometry reaches the PTY, if any viewer's does.
    owner: Option<ViewerId>,
    /// The size the owner last asked for, held until the debounce applies it.
    pending_size: Option<(u16, u16)>,
    /// When the last viewer left, which starts the linger.
    idle_since: Option<Instant>,
    /// The pane's grid and its owner, pushed to every attached viewer as it changes.
    geometry: watch::Sender<Geometry>,
}

impl TrackedPane {
    fn is_idle(&self) -> bool {
        self.viewers.is_empty()
    }

    /// Publish the pane's geometry and who is choosing it, if either has moved.
    fn publish_geometry(&self, size: (u16, u16)) {
        let next = Geometry {
            cols: size.0,
            rows: size.1,
            owner: self.owner,
        };
        self.geometry.send_if_modified(|held| {
            if *held == next {
                return false;
            }
            *held = next;
            true
        });
    }
}

/// The pane's grid and which viewer chose it.
///
/// Pushed rather than asked for. A resize is debounced before it reaches the PTY, so a viewer
/// that asked and was immediately told the answer would be told the *old* geometry — and, on the
/// first resize of an unowned pane, told it did not own a size it was about to be given. Viewers
/// learn what actually happened, when it happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Geometry {
    pub cols: u16,
    pub rows: u16,
    pub owner: Option<ViewerId>,
}

impl Geometry {
    pub fn owned_by(&self, viewer: ViewerId) -> bool {
        self.owner == Some(viewer)
    }
}

/// What a pane's process is doing, pushed the moment it changes.
#[derive(Debug, Clone, Serialize)]
pub struct PaneLifecycle {
    /// The mirror tracking key, `<session>:<window>`.
    pub key: String,
    pub session: String,
    pub window: String,
    /// `running`, `exited`, or `unknown`.
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

impl PaneLifecycle {
    fn from(key: &str, session: &str, window: &str, liveness: &PaneLiveness) -> Self {
        let (status, exit_code) = match liveness {
            PaneLiveness::Running => ("running", None),
            PaneLiveness::Exited(exit) => ("exited", exit.code),
            PaneLiveness::Unknown => ("unknown", None),
        };
        Self {
            key: key.to_string(),
            session: session.to_string(),
            window: window.to_string(),
            status,
            exit_code,
        }
    }
}

/// A pane being watched for its own sake, not because anything is showing it.
struct Watched {
    session: String,
    window: String,
    liveness: PaneLiveness,
    watcher: tokio::task::JoinHandle<()>,
}

impl Drop for Watched {
    fn drop(&mut self) {
        self.watcher.abort();
    }
}

/// Owns the daemon's rmux connections and the set of panes it is mirroring and watching.
pub struct PaneRunner {
    /// For control operations — finding a window's pane, spawning, killing. Mirrors do not run on
    /// this: each takes a connection of its own, because output subscriptions are capped per
    /// connection and mirrors sharing one starve each other.
    rmux: Mutex<Option<Arc<Rmux>>>,
    tracked: RwLock<HashMap<String, TrackedPane>>,
    watched: RwLock<HashMap<String, Watched>>,
    lifecycle: broadcast::Sender<PaneLifecycle>,
    next_viewer: std::sync::atomic::AtomicU64,
}

impl PaneRunner {
    pub fn new() -> Arc<Self> {
        let (lifecycle, _) = broadcast::channel(LIFECYCLE_DEPTH);
        let runner = Arc::new(Self {
            rmux: Mutex::new(None),
            tracked: RwLock::new(HashMap::new()),
            watched: RwLock::new(HashMap::new()),
            lifecycle,
            next_viewer: std::sync::atomic::AtomicU64::new(0),
        });
        tokio::spawn(sweep(Arc::downgrade(&runner)));
        runner
    }

    /// Connect to the rmux daemon, starting one if needed.
    ///
    /// Deferred so a machine without rmux still boots the toren daemon. Not a `OnceCell`: the
    /// SDK's client dies for good when any request on it is cancelled mid-flight (an HTTP handler
    /// dropped by a disconnecting browser is enough), and it does not survive an rmux-server
    /// restart either — so a client found dead is thrown away and the next call reconnects.
    async fn rmux(&self) -> Result<Arc<Rmux>> {
        let mut slot = self.rmux.lock().await;
        if let Some(client) = slot.as_ref() {
            return Ok(client.clone());
        }
        let client = connect().await?;
        *slot = Some(client.clone());
        Ok(client)
    }

    /// Forget `client` if it is still the shared one, so the next call reconnects.
    async fn forget(&self, client: &Arc<Rmux>) {
        let mut slot = self.rmux.lock().await;
        if slot.as_ref().is_some_and(|held| Arc::ptr_eq(held, client)) {
            info!("rmux client transport is gone; reconnecting on next use");
            *slot = None;
        }
    }

    /// Pass an SDK result through, discarding the shared client when the error says its transport
    /// is dead — the one failure a retry on the same client can never recover from.
    async fn healing<T>(&self, client: &Arc<Rmux>, result: Result<T>) -> Result<T> {
        if let Err(e) = &result {
            if transport_is_dead(e) {
                self.forget(client).await;
            }
        }
        result
    }

    /// Every pane-state change the daemon observes, for surfaces that would otherwise poll.
    ///
    /// A window list showing "running" next to a process killed ten minutes ago is the symptom
    /// this exists for: liveness used to be observed only by a mirror's pump, and only mirrored
    /// panes had one.
    pub fn lifecycle(&self) -> broadcast::Receiver<PaneLifecycle> {
        self.lifecycle.subscribe()
    }

    /// Start watching a window's pane, if it is not already watched.
    ///
    /// Watching is cheap — a long poll on a transport of its own, no output subscription — and it
    /// survives every viewer detaching, which is the whole point.
    ///
    /// Keyed by session and window rather than by whatever key the caller mirrors under: what is
    /// being watched is a pane, and a pane does not stop existing because nothing is showing it.
    async fn watch(self: &Arc<Self>, session: &str, window: &str) {
        let key = &window_key(session, window);
        if self.watched.read().await.contains_key(key) {
            return;
        }
        let Ok(rmux) = self.rmux().await else { return };
        let found = self
            .healing(&rmux, find_window_pane(&rmux, session, window).await)
            .await;
        let Ok(pane_id) = found else { return };

        let mut watched = self.watched.write().await;
        if watched.contains_key(key) {
            return;
        }
        let watcher = tokio::spawn(follow_liveness(
            Arc::downgrade(self),
            key.to_string(),
            session.to_string(),
            pane_id,
        ));
        watched.insert(
            key.to_string(),
            Watched {
                session: session.to_string(),
                window: window.to_string(),
                liveness: PaneLiveness::Unknown,
                watcher,
            },
        );
    }

    /// Watch every window of a session, so its whole window list stays honest.
    ///
    /// Called when a browser looks at a workspace, which is the moment it starts caring what the
    /// panes in it are doing — including the ones it is not showing. Watching only what is being
    /// mirrored is what let a killed process go on reading "running" in the list until somebody
    /// clicked into that window.
    pub async fn watch_session(self: &Arc<Self>, session: &str) {
        if !rmux_conv::session_exists(session) {
            return;
        }
        for window in rmux_conv::list_windows(session).unwrap_or_default() {
            self.watch(session, &window).await;
        }
    }

    /// Record what a watcher saw and pass it on, if it is news.
    async fn observed(&self, key: &str, liveness: PaneLiveness) {
        let event = {
            let mut watched = self.watched.write().await;
            let Some(entry) = watched.get_mut(key) else {
                return;
            };
            if entry.liveness == liveness {
                return;
            }
            entry.liveness = liveness.clone();
            PaneLifecycle::from(key, &entry.session, &entry.window, &liveness)
        };
        debug!("{}: pane is {}", key, event.status);
        let _ = self.lifecycle.send(event);
    }

    /// Start (or restart) an agent in a workspace's session and begin mirroring its pane.
    ///
    /// The session name comes from the caller, which derives it from the workspace's
    /// state — that's what keys it to the instance uid.
    pub async fn start_agent(
        self: &Arc<Self>,
        key: &str,
        session: &str,
        workspace_path: &Path,
        env: &[(String, String)],
        argv: &[String],
    ) -> Result<String> {
        // Same helpers `breq` uses, so both interfaces produce identical sessions.
        rmux_conv::ensure_session(session, workspace_path, env)?;
        rmux_conv::spawn_agent(session, workspace_path, argv)?;

        info!("{}: spawned {} in rmux session {}", key, argv[0], session);

        // A respawn puts a new pane behind the name, so the old watcher is watching a corpse.
        self.watched
            .write()
            .await
            .remove(&window_key(session, rmux_conv::AGENT_WINDOW));
        self.track(key, session, rmux_conv::AGENT_WINDOW).await?;
        self.watch(session, rmux_conv::AGENT_WINDOW).await;
        Ok(session.to_string())
    }

    /// Open a fresh shell window in the workspace's session, returning its window name.
    ///
    /// The browser can hold several shells at once (a dev server, a log tail, a scratch prompt),
    /// so each gets a unique name; the caller then mirrors it like any other window.
    pub async fn open_shell(
        self: &Arc<Self>,
        session: &str,
        workspace_path: &Path,
        env: &[(String, String)],
    ) -> Result<String> {
        rmux_conv::ensure_session(session, workspace_path, env)?;
        let window = rmux_conv::open_shell(session, workspace_path)?;
        info!("opened shell window '{}' in {}", window, session);
        self.watch(session, &window).await;
        Ok(window)
    }

    /// Run a one-shot command in a `cmd` window of its own, returning that window's name.
    ///
    /// Held, so the output of something that ran and finished stays readable until it is
    /// dismissed — which is why the browser drives workflow scripts through here instead of
    /// streaming their output over HTTP.
    pub async fn run_command(
        self: &Arc<Self>,
        session: &str,
        workspace_path: &Path,
        env: &[(String, String)],
        argv: &[String],
    ) -> Result<String> {
        rmux_conv::ensure_session(session, workspace_path, env)?;
        let window = rmux_conv::spawn_command(session, workspace_path, argv, true)?;
        info!(
            "ran '{}' in window '{}' of {}",
            argv.join(" "),
            window,
            session
        );
        self.watch(session, &window).await;
        Ok(window)
    }

    /// Point a mirror at the pane running right now in `window`.
    ///
    /// Adopts a session this process never started — which is all re-adoption after a restart
    /// amounts to, since the daemon persists nothing — and replaces a mirror left following a pane
    /// that `breq do` or a resume has since swapped out. `key` scopes the mirror to one window, so
    /// a workspace can have its agent and several shells mirrored at once.
    pub async fn ensure_current(
        self: &Arc<Self>,
        key: &str,
        session: &str,
        window: &str,
    ) -> Result<String> {
        if !rmux_conv::session_exists(session) {
            return Err(anyhow!("No rmux session '{}'", session));
        }

        let rmux = self.rmux().await?;
        let live = self
            .healing(&rmux, find_window_pane(&rmux, session, window).await)
            .await?;

        self.watch(session, window).await;

        let is_current = self
            .tracked
            .read()
            .await
            .get(key)
            .is_some_and(|t| t.pane.pane_id() == live && t.pane.is_current());
        if is_current {
            return Ok(session.to_string());
        }

        self.track(key, session, window).await?;
        Ok(session.to_string())
    }

    /// Mirror `window`'s current pane, replacing any mirror already under `key`.
    ///
    /// The mirror takes a connection of its own rather than sharing the runner's, for the
    /// subscription budget this module's header describes.
    async fn track(&self, key: &str, session: &str, window: &str) -> Result<()> {
        let rmux = self.rmux().await?;
        let pane_id = self
            .healing(&rmux, find_window_pane(&rmux, session, window).await)
            .await?;
        let pane = Arc::new(MirroredPane::attach(session, pane_id, role_of(window)).await?);
        let size = pane.size().await.unwrap_or_default();

        // Viewers of the outgoing mirror carry over: they asked for this window, not for one
        // particular pane in it, and dropping them here would close their sockets on a resume.
        let carried = self
            .tracked
            .read()
            .await
            .get(key)
            .map(|t| t.viewers.clone())
            .unwrap_or_default();

        // Dropping the outgoing mirror releases whatever is still reading it.
        self.tracked.write().await.insert(
            key.to_string(),
            TrackedPane {
                session: session.to_string(),
                pane,
                viewers: carried,
                owner: None,
                pending_size: None,
                idle_since: None,
                geometry: watch::channel(Geometry {
                    cols: size.0,
                    rows: size.1,
                    owner: None,
                })
                .0,
            },
        );
        Ok(())
    }

    pub async fn mirror(&self, key: &str) -> Option<Arc<PaneMirror>> {
        self.tracked.read().await.get(key).map(|t| t.pane.mirror())
    }

    /// Follow the pane's grid and who is choosing it.
    pub async fn geometry(&self, key: &str) -> Option<watch::Receiver<Geometry>> {
        self.tracked
            .read()
            .await
            .get(key)
            .map(|entry| entry.geometry.subscribe())
    }

    pub async fn session_of(&self, key: &str) -> Option<String> {
        self.tracked
            .read()
            .await
            .get(key)
            .map(|t| t.session.clone())
    }

    /// Register a viewer of `key`, so the mirror is kept for as long as it is watched.
    pub async fn attach_viewer(&self, key: &str) -> Option<ViewerId> {
        let id = ViewerId(
            self.next_viewer
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        );
        let mut tracked = self.tracked.write().await;
        let entry = tracked.get_mut(key)?;
        entry.viewers.push(id);
        entry.idle_since = None;
        Some(id)
    }

    /// Release a viewer. The mirror stays for a moment in case it comes back, and is swept if it
    /// does not.
    pub async fn detach_viewer(&self, key: &str, viewer: ViewerId) {
        let departed = {
            let mut tracked = self.tracked.write().await;
            let Some(entry) = tracked.get_mut(key) else {
                return;
            };
            entry.viewers.retain(|held| *held != viewer);
            if entry.owner == Some(viewer) {
                entry.owner = None;
                let held = *entry.geometry.borrow();
                entry.publish_geometry((held.cols, held.rows));
            }
            if !entry.is_idle() {
                return;
            }
            entry.idle_since = Some(Instant::now());
            // The last viewer leaving is what hands the pane's size back, so a terminal still
            // attached can have it — "the browser tab closed, resize to the terminal".
            entry.pane.clone()
        };
        if let Err(e) = departed.release_size().await {
            debug!("{}: failed to release the pane's size: {:#}", key, e);
        }
    }

    /// Forward browser keystrokes to the mirrored pane, verbatim.
    ///
    /// Typing is also what makes a viewer the active one, which is what decides whose geometry
    /// the pane takes: the viewer somebody is working in owns the size, and the rest scale.
    pub async fn send_input(&self, key: &str, viewer: ViewerId, text: &str) -> Result<()> {
        let (pane, became_owner) = {
            let mut tracked = self.tracked.write().await;
            let entry = tracked
                .get_mut(key)
                .ok_or_else(|| anyhow!("No tracked pane for {}", key))?;
            let became_owner = entry.owner != Some(viewer);
            entry.owner = Some(viewer);
            let held = *entry.geometry.borrow();
            entry.publish_geometry((held.cols, held.rows));
            (entry.pane.clone(), became_owner)
        };

        pane.send_text(text).await?;
        if became_owner {
            if let Err(e) = pane.claim_size().await {
                debug!("{}: failed to claim the pane's size: {:#}", key, e);
            }
        }
        Ok(())
    }

    /// Repaint a mirrored pane from its screen, returning the epoch the paint opens.
    ///
    /// What a browser asks for when its terminal has gone out of step — because it fell behind, or
    /// because rmux dropped output on the way here. The paint reaches every client attached to the
    /// pane, not just the one that asked; a fresh screen is never wrong for any of them.
    pub async fn resync(&self, key: &str) -> Result<u32> {
        self.pane_of(key).await?.repaint().await
    }

    /// The pane's geometry as *rmux* has it.
    ///
    /// Viewers are told the geometry through [`Self::geometry`], which is what the daemon
    /// believes and pushes. This asks the source of truth instead, which is what makes it worth
    /// asserting against — a test that checked the daemon's own belief would pass on a resize
    /// that never reached the PTY.
    #[cfg(test)]
    pub async fn size(&self, key: &str) -> Result<(u16, u16)> {
        self.pane_of(key).await?.size().await
    }

    /// Whether this viewer's geometry is the one the pane takes, asked of rmux rather than
    /// believed. See [`Self::size`] for why the tests ask rather than read the pushed value.
    ///
    /// Both halves have to hold: this viewer is the active one among those the daemon is showing,
    /// *and* the daemon is the process sizing the pane at all — a `breq` terminal mirroring the
    /// same pane is a writer this daemon cannot see the viewers of.
    #[cfg(test)]
    pub async fn owns_size(&self, key: &str, viewer: ViewerId) -> bool {
        let pane = {
            let tracked = self.tracked.read().await;
            match tracked.get(key) {
                Some(entry) if entry.owner == Some(viewer) => entry.pane.clone(),
                _ => return false,
            }
        };
        pane.owns_size().await.unwrap_or(false)
    }

    /// Ask for the pane to take a viewer's geometry, and say whether it will.
    ///
    /// Only the owning viewer's size reaches the PTY. Everything else is told `false` and scales
    /// the grid it has — the alternative, which is what this replaces, is two viewers writing
    /// different sizes to one PTY and the loser rendering a screen laid out for the winner.
    ///
    /// Applied after a pause: a drag or a font load fires this many times a second, and only the
    /// size it stops at is worth a round trip and a `SIGWINCH`.
    pub async fn resize(
        self: &Arc<Self>,
        key: &str,
        viewer: ViewerId,
        cols: u16,
        rows: u16,
    ) -> Result<bool> {
        let schedule = {
            let mut tracked = self.tracked.write().await;
            let Some(entry) = tracked.get_mut(key) else {
                return Err(anyhow!("No tracked pane for {}", key));
            };
            // An unowned pane goes to whoever asks first; after that it takes its owner's size.
            if entry.owner.is_none() {
                entry.owner = Some(viewer);
                let held = *entry.geometry.borrow();
                entry.publish_geometry((held.cols, held.rows));
            }
            if entry.owner != Some(viewer) {
                return Ok(false);
            }
            let first = entry.pending_size.is_none();
            entry.pending_size = Some((cols, rows));
            first
        };

        if schedule {
            let runner = Arc::downgrade(self);
            let key = key.to_string();
            tokio::spawn(async move {
                tokio::time::sleep(RESIZE_DEBOUNCE).await;
                if let Some(runner) = runner.upgrade() {
                    runner.apply_pending_size(&key).await;
                }
            });
        }
        Ok(true)
    }

    /// Take a viewer's geometry whether or not it was already the owner — the "make this the
    /// window that decides" affordance.
    pub async fn take_size(
        self: &Arc<Self>,
        key: &str,
        viewer: ViewerId,
        cols: u16,
        rows: u16,
    ) -> Result<()> {
        let pane = {
            let mut tracked = self.tracked.write().await;
            let entry = tracked
                .get_mut(key)
                .ok_or_else(|| anyhow!("No tracked pane for {}", key))?;
            entry.owner = Some(viewer);
            let held = *entry.geometry.borrow();
            entry.publish_geometry((held.cols, held.rows));
            entry.pane.clone()
        };
        pane.claim_size().await?;
        self.resize(key, viewer, cols, rows).await?;
        Ok(())
    }

    /// Write whatever size the debounce settled on.
    async fn apply_pending_size(&self, key: &str) {
        let size = {
            let mut tracked = self.tracked.write().await;
            match tracked.get_mut(key) {
                Some(entry) => entry.pending_size.take(),
                None => None,
            }
        };
        let Some((cols, rows)) = size else { return };
        let Ok(pane) = self.pane_of(key).await else {
            return;
        };

        // The pane may be sized from another *process* — a `breq` terminal mirroring the same
        // pane — in which case this daemon is not the one writing it, and what the viewers should
        // be told is what the pane actually is rather than what they asked for.
        let applied = match pane.resize_as_owner(cols, rows).await {
            Ok(true) => {
                debug!("{}: pane resized to {}x{}", key, cols, rows);
                Some((cols, rows))
            }
            Ok(false) => {
                debug!("{}: another process owns the pane's size", key);
                pane.size().await.ok()
            }
            Err(e) => {
                warn!("{}: failed to resize the pane: {:#}", key, e);
                None
            }
        };

        let Some(size) = applied else { return };
        if let Some(entry) = self.tracked.read().await.get(key) {
            entry.publish_geometry(size);
        }
    }

    /// The mirror under `key`, taken out from under the lock so the caller can await on it.
    async fn pane_of(&self, key: &str) -> Result<Arc<MirroredPane>> {
        self.tracked
            .read()
            .await
            .get(key)
            .map(|entry| entry.pane.clone())
            .ok_or_else(|| anyhow!("No tracked pane for {}", key))
    }

    /// Liveness of a window's current pane.
    ///
    /// Answered from what the watcher has already seen when there is one, which is both faster
    /// and more current than asking — the watcher is pushed to, and asking means forking the
    /// rmux binary. A window nothing has watched yet is resolved the slow way, and watched from
    /// then on.
    pub async fn status(self: &Arc<Self>, session: &str, window: &str) -> PaneLiveness {
        let key = window_key(session, window);
        if let Some(watched) = self.watched.read().await.get(&key) {
            if watched.liveness != PaneLiveness::Unknown {
                return watched.liveness.clone();
            }
        }

        let Ok(rmux) = self.rmux().await else {
            return PaneLiveness::Unknown;
        };
        let found = self
            .healing(&rmux, find_window_pane(&rmux, session, window).await)
            .await;
        let Ok(pane_id) = found else {
            return PaneLiveness::Unknown;
        };
        let liveness = self
            .healing(&rmux, toren_mirror::liveness(&rmux, session, pane_id).await)
            .await
            .unwrap_or(PaneLiveness::Unknown);

        self.watch(session, window).await;
        liveness
    }

    /// Kill the agent window and stop mirroring, leaving the session's shells alive.
    ///
    /// Works off the workspace rather than what this process tracks, so a `breq do` agent is
    /// equally stoppable. Returns whether an agent was actually running.
    pub async fn stop_agent(&self, key: &str, session: &str) -> Result<bool> {
        self.close_window(key, session, rmux_conv::AGENT_WINDOW)
            .await
    }

    /// Kill one window and stop mirroring it, leaving the rest of the session alive.
    ///
    /// A workspace's session is a set of windows; closing one (an agent, a finished dev-server
    /// shell) is not the same as tearing the whole session down.
    pub async fn close_window(&self, key: &str, session: &str, window: &str) -> Result<bool> {
        // Attached browsers are following a pane that is about to die; dropping the mirror is what
        // tells them so.
        self.tracked.write().await.remove(key);
        self.watched
            .write()
            .await
            .remove(&window_key(session, window));

        let was_live = rmux_conv::window_exists(session, window);
        // Kill even a dead window so the next spawn of that name starts clean.
        rmux_conv::kill_window(session, window)?;
        if was_live {
            info!("{}: killed window '{}' in {}", key, window, session);
        }
        Ok(was_live)
    }

    /// Drop mirrors nobody has watched for a while, freeing their connections and subscriptions.
    async fn evict_idle(&self) {
        let stale: Vec<String> = self
            .tracked
            .read()
            .await
            .iter()
            .filter(|(_, entry)| {
                entry
                    .idle_since
                    .is_some_and(|since| since.elapsed() > MIRROR_LINGER)
            })
            .map(|(key, _)| key.clone())
            .collect();
        if stale.is_empty() {
            return;
        }
        let mut tracked = self.tracked.write().await;
        for key in stale {
            // Re-checked under the write lock: a viewer may have arrived since the scan.
            if tracked.get(&key).is_some_and(|entry| {
                entry.is_idle()
                    && entry
                        .idle_since
                        .is_some_and(|since| since.elapsed() > MIRROR_LINGER)
            }) {
                debug!("{}: no viewers, dropping the mirror", key);
                tracked.remove(&key);
            }
        }
    }

    /// The pane a mirror is following.
    #[cfg(test)]
    async fn tracked_pane_id(&self, key: &str) -> Option<toren_mirror::PaneId> {
        self.tracked.read().await.get(key).map(|t| t.pane.pane_id())
    }
}

/// A mirror tracking key scoped to one window of one session.
pub fn window_key(session: &str, window: &str) -> String {
    format!("{}:{}", session, window)
}

/// Watch one pane's process for as long as the runner cares about it.
async fn follow_liveness(
    runner: Weak<PaneRunner>,
    key: String,
    session: String,
    pane_id: toren_mirror::PaneId,
) {
    // A connection of its own, so a long poll never sits in front of a control request.
    let Ok(rmux) = connect().await else { return };

    loop {
        let Some(alive) = runner.upgrade() else {
            return;
        };
        let liveness = toren_mirror::liveness(&rmux, &session, pane_id)
            .await
            .unwrap_or(PaneLiveness::Unknown);
        let settled = matches!(liveness, PaneLiveness::Exited(_));
        alive.observed(&key, liveness).await;
        drop(alive);

        if settled {
            // An exit is final. Nothing else about this pane will change, and a resume replaces
            // the pane rather than reviving it — which drops this watcher and starts another.
            return;
        }

        // Push, not poll: this blocks on rmux's own pane-state stream until the process is
        // actually gone, so the whole watch costs one idle long poll per pane rather than a
        // forked `list-panes` every two seconds.
        if let Err(e) = toren_mirror::wait_until_exited(&rmux, &session, pane_id).await {
            debug!("{}: lost the liveness watch ({}); retrying", key, e);
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
}

/// Sweep up mirrors nobody is watching, for as long as the runner exists.
async fn sweep(runner: Weak<PaneRunner>) {
    let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let Some(runner) = runner.upgrade() else {
            return;
        };
        runner.evict_idle().await;
    }
}

/// An agent pane offers to resume its session where a shell offers to re-run; that is the only
/// thing the mirror needs to know about what a window was created for.
fn role_of(window: &str) -> PaneRole {
    if window == rmux_conv::AGENT_WINDOW {
        PaneRole::Agent
    } else {
        PaneRole::Shell
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session created the way `breq do` creates one is mirrored here: seed, live stream,
    /// input, status, geometry. Needs rmux installed.
    #[tokio::test]
    async fn mirrors_a_session_created_the_breq_way() {
        if !rmux_conv::is_available() {
            eprintln!("skipping: rmux not on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("one");
        std::fs::create_dir(&workspace).unwrap();

        let segment = format!("panerunner{}", std::process::id());
        let session = rmux_conv::session_name(&segment, "one", None);
        rmux_conv::ensure_session(&session, &workspace, &[]).unwrap();
        rmux_conv::spawn_agent(
            &session,
            &workspace,
            &[
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo BEFORE-ATTACH; cat".to_string(),
            ],
        )
        .unwrap();

        // The tracking key is the place's session name (uid-embedded).
        let runner = PaneRunner::new();
        wait_for(|| async {
            runner
                .ensure_current(&session, &session, rmux_conv::AGENT_WINDOW)
                .await
                .is_ok()
        })
        .await;

        let mirror = runner.mirror(&session).await.expect("pane is tracked");
        let viewer = runner.attach_viewer(&session).await.expect("a viewer slot");

        // The seed is a paint of the pane's screen, so output from before the attach is there.
        wait_for(|| async { contains(&mirror.attach().await.0.bytes, "BEFORE-ATTACH") }).await;

        // Live: output produced after, delivered on the subscription taken with the backfill.
        let (_, mut live) = mirror.attach().await;
        runner
            .send_input(&session, viewer, "AFTER-ATTACH\n")
            .await
            .unwrap();

        let mut seen = Vec::new();
        while !contains(&seen, "AFTER-ATTACH") {
            let frame = tokio::time::timeout(Duration::from_secs(5), live.recv())
                .await
                .expect("live output arrives")
                .expect("subscription stays open");
            seen.extend_from_slice(&frame.bytes);
        }

        // A resync paints the pane's screen under a new epoch, which is what lets an attached
        // client throw away the frames it had queued from the old one.
        let before = mirror.epoch();
        let epoch = runner.resync(&session).await.unwrap();
        assert!(epoch > before, "a paint opens an epoch of its own");

        // Frames left over from the screen the paint replaces are the ones a client discards; the
        // paint is what it applies instead.
        let painted = loop {
            let frame = tokio::time::timeout(Duration::from_secs(5), live.recv())
                .await
                .expect("the paint reaches clients already attached")
                .expect("subscription stays open");
            if frame.epoch == epoch {
                break frame;
            }
            assert!(frame.epoch < epoch, "nothing outranks the newest paint");
        };
        assert!(
            contains(&painted.bytes, "AFTER-ATTACH"),
            "the paint describes the pane's screen, not the bytes that built it"
        );

        assert_eq!(
            runner.status(&session, rmux_conv::AGENT_WINDOW).await,
            PaneLiveness::Running
        );
        let rmux = runner.rmux().await.unwrap();
        let pane_id = find_window_pane(&rmux, &session, rmux_conv::AGENT_WINDOW)
            .await
            .unwrap();
        assert_eq!(
            runner.tracked_pane_id(&session).await,
            Some(pane_id),
            "the mirror is keyed by the window's live pane id"
        );

        // A detached window sits at 80x24, so this guards the window-vs-pane distinction. The
        // viewer that typed owns the size, so its request is the one that lands — after the
        // debounce that keeps a drag from being a hundred round trips.
        assert!(runner.resize(&session, viewer, 100, 30).await.unwrap());
        wait_for(|| async { runner.size(&session).await.ok() == Some((100, 30)) }).await;

        assert!(runner.stop_agent(&session, &session).await.unwrap());
        // Nothing left to stop, and it must say so rather than report success.
        assert!(!runner.stop_agent(&session, &session).await.unwrap());
        rmux_conv::kill_session(&session).unwrap();
    }

    /// Mirrors that shared one connection ran out of subscriptions at the sixteenth, and a live
    /// pane past that was reported as exited and being held — so starting an agent in it answered
    /// 409 while the browser showed it dead. Twenty panes, all mirrored, none of them lying.
    /// Needs rmux installed.
    #[tokio::test]
    async fn mirrors_more_panes_than_one_connection_can_subscribe_to() {
        if !rmux_conv::is_available() {
            eprintln!("skipping: rmux not on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("one");
        std::fs::create_dir(&workspace).unwrap();

        let segment = format!("subcap{}", std::process::id());
        let session = rmux_conv::session_name(&segment, "one", None);
        rmux_conv::ensure_session(&session, &workspace, &[]).unwrap();

        let runner = PaneRunner::new();
        // Comfortably past the cap, so a shared connection could not possibly carry them all.
        let windows: Vec<String> = (0..20)
            .map(|_| rmux_conv::open_shell(&session, &workspace).unwrap())
            .collect();

        let mut keys = Vec::new();
        for window in &windows {
            let key = window_key(&session, window);
            wait_for(|| async { runner.ensure_current(&key, &session, window).await.is_ok() })
                .await;
            // Something has to be watching, or the sweeper is entitled to take the mirror back.
            runner.attach_viewer(&key).await.expect("a viewer slot");
            keys.push(key);
        }

        // Every mirror seeds from its pane's screen, which it cannot do without a subscription.
        for key in &keys {
            let mirror = runner.mirror(key).await.expect("every window is mirrored");
            wait_for(|| async { !mirror.attach().await.0.bytes.is_empty() }).await;
            assert!(
                !mirror.has_ended(),
                "{}: a live pane must never be reported as ended",
                key
            );
        }

        rmux_conv::kill_session(&session).unwrap();
    }

    /// One pane, one size, two viewers. The one that typed most recently decides, and the other
    /// is told `false` so it can scale rather than fight for the PTY. Needs rmux installed.
    #[tokio::test]
    async fn only_the_viewer_that_typed_last_sizes_the_pane() {
        if !rmux_conv::is_available() {
            eprintln!("skipping: rmux not on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("one");
        std::fs::create_dir(&workspace).unwrap();

        let segment = format!("sizeowner{}", std::process::id());
        let session = rmux_conv::session_name(&segment, "one", None);
        let runner = PaneRunner::new();
        runner
            .start_agent(
                &session,
                &session,
                &workspace,
                &[],
                &["/bin/sh".to_string(), "-c".to_string(), "cat".to_string()],
            )
            .await
            .unwrap();

        let terminal = runner.attach_viewer(&session).await.unwrap();
        let browser = runner.attach_viewer(&session).await.unwrap();

        // Nobody has typed, so the first to ask takes it.
        assert!(runner.resize(&session, terminal, 100, 30).await.unwrap());
        wait_for(|| async { runner.size(&session).await.ok() == Some((100, 30)) }).await;

        // The other viewer's geometry is refused rather than written over the owner's.
        assert!(
            !runner.resize(&session, browser, 60, 20).await.unwrap(),
            "a viewer that does not own the size must be told so"
        );
        assert_eq!(runner.size(&session).await.unwrap(), (100, 30));
        assert!(runner.owns_size(&session, terminal).await);
        assert!(!runner.owns_size(&session, browser).await);

        // Typing is what moves ownership, which is what makes it follow whoever is working.
        runner.send_input(&session, browser, "\n").await.unwrap();
        assert!(runner.resize(&session, browser, 90, 26).await.unwrap());
        wait_for(|| async { runner.size(&session).await.ok() == Some((90, 26)) }).await;

        // The owner leaving hands the pane back, so the viewer still there can size it.
        runner.detach_viewer(&session, browser).await;
        assert!(runner.resize(&session, terminal, 110, 40).await.unwrap());
        wait_for(|| async { runner.size(&session).await.ok() == Some((110, 40)) }).await;

        runner.stop_agent(&session, &session).await.unwrap();
        rmux_conv::kill_session(&session).unwrap();
    }

    /// A mirror lives while somebody is watching. Mirrors used to accumulate for the life of the
    /// daemon, which is what exhausted the subscription budget. Needs rmux installed.
    #[tokio::test]
    async fn drops_a_mirror_once_nobody_is_watching_it() {
        if !rmux_conv::is_available() {
            eprintln!("skipping: rmux not on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("one");
        std::fs::create_dir(&workspace).unwrap();

        let segment = format!("evict{}", std::process::id());
        let session = rmux_conv::session_name(&segment, "one", None);
        let runner = PaneRunner::new();
        runner
            .start_agent(
                &session,
                &session,
                &workspace,
                &[],
                &["/bin/sh".to_string(), "-c".to_string(), "cat".to_string()],
            )
            .await
            .unwrap();

        let viewer = runner.attach_viewer(&session).await.unwrap();
        assert!(runner.mirror(&session).await.is_some());

        // Still watched, so a sweep must leave it alone.
        runner.evict_idle().await;
        assert!(runner.mirror(&session).await.is_some());

        // Unwatched, but inside the linger that absorbs a tab switch or a reconnect.
        runner.detach_viewer(&session, viewer).await;
        runner.evict_idle().await;
        assert!(
            runner.mirror(&session).await.is_some(),
            "a mirror must survive the moment between one viewer leaving and the next arriving"
        );

        // Past the linger.
        runner
            .tracked
            .write()
            .await
            .get_mut(&session)
            .unwrap()
            .idle_since = Some(Instant::now() - MIRROR_LINGER * 2);
        runner.evict_idle().await;
        assert!(runner.mirror(&session).await.is_none());

        // The pane itself is untouched: evicting a mirror is not closing anything.
        assert_eq!(
            runner.status(&session, rmux_conv::AGENT_WINDOW).await,
            PaneLiveness::Running
        );

        runner.stop_agent(&session, &session).await.unwrap();
        rmux_conv::kill_session(&session).unwrap();
    }

    /// A pane nobody has open is exactly the pane whose death used to go unnoticed. Watching is
    /// separate from mirroring, so killing a process is news the moment it happens rather than
    /// the moment somebody looks. Needs rmux installed.
    #[tokio::test]
    async fn reports_an_exit_in_a_pane_nobody_is_watching() {
        if !rmux_conv::is_available() {
            eprintln!("skipping: rmux not on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("one");
        std::fs::create_dir(&workspace).unwrap();

        let segment = format!("lifecycle{}", std::process::id());
        let session = rmux_conv::session_name(&segment, "one", None);
        let runner = PaneRunner::new();
        let mut events = runner.lifecycle();

        let key = window_key(&session, rmux_conv::AGENT_WINDOW);
        runner
            .start_agent(
                &key,
                &session,
                &workspace,
                &[],
                &[
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "sleep 1; exit 5".to_string(),
                ],
            )
            .await
            .unwrap();

        // Nothing is watching this pane: no viewer ever attaches, and the mirror is fair game
        // for the sweeper. The exit still has to arrive.
        let exited = loop {
            let event = tokio::time::timeout(Duration::from_secs(20), events.recv())
                .await
                .expect("an exit is pushed rather than waited for")
                .expect("the lifecycle channel stays open");
            if event.key == key && event.status == "exited" {
                break event;
            }
        };
        assert_eq!(exited.exit_code, Some(5));
        assert_eq!(exited.window, rmux_conv::AGENT_WINDOW);

        // And the cheap answer agrees with the pushed one, so a 409 check reads the same thing.
        assert!(matches!(
            runner.status(&session, rmux_conv::AGENT_WINDOW).await,
            PaneLiveness::Exited(_)
        ));

        rmux_conv::kill_session(&session).unwrap();
    }

    /// Replacing the agent re-points the mirror and releases clients on the old pane; re-adopting
    /// an unchanged pane changes nothing. Needs rmux installed.
    #[tokio::test]
    async fn re_adoption_refreshes_the_mirror_only_when_the_pane_changed() {
        if !rmux_conv::is_available() {
            eprintln!("skipping: rmux not on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("one");
        std::fs::create_dir(&workspace).unwrap();

        let segment = format!("readopt{}", std::process::id());
        let session = rmux_conv::session_name(&segment, "one", None);
        let runner = PaneRunner::new();

        let first_agent = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "echo FIRST-AGENT; sleep 30".to_string(),
        ];
        runner
            .start_agent(&session, &session, &workspace, &[], &first_agent)
            .await
            .unwrap();

        let first_mirror = runner.mirror(&session).await.unwrap();
        wait_for(|| async { contains(&first_mirror.attach().await.0.bytes, "FIRST-AGENT") }).await;

        runner
            .ensure_current(&session, &session, rmux_conv::AGENT_WINDOW)
            .await
            .unwrap();
        assert!(
            Arc::ptr_eq(&first_mirror, &runner.mirror(&session).await.unwrap()),
            "re-adopting an unchanged pane should not rebuild the mirror"
        );

        // Replace the agent the way `breq do` does, behind the daemon's back.
        let mut state = first_mirror.state();
        rmux_conv::spawn_agent(
            &session,
            &workspace,
            &[
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo SECOND-AGENT; sleep 30".to_string(),
            ],
        )
        .unwrap();

        runner
            .ensure_current(&session, &session, rmux_conv::AGENT_WINDOW)
            .await
            .unwrap();

        let second_mirror = runner.mirror(&session).await.unwrap();
        assert!(
            !Arc::ptr_eq(&first_mirror, &second_mirror),
            "a replaced pane must get a fresh mirror"
        );
        assert!(
            state.borrow_and_update().is_ended(),
            "clients attached to the old pane must be told it ended"
        );
        wait_for(|| async { contains(&second_mirror.attach().await.0.bytes, "SECOND-AGENT") })
            .await;

        runner.stop_agent(&session, &session).await.unwrap();
        rmux_conv::kill_session(&session).unwrap();
    }

    /// A mirror built after the fact opens on a paint of the pane's screen, which is what makes a
    /// restarted daemon's reattach look like nothing happened. Needs rmux installed.
    #[tokio::test]
    async fn a_fresh_mirror_seeds_from_the_panes_screen() {
        if !rmux_conv::is_available() {
            eprintln!("skipping: rmux not on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("one");
        std::fs::create_dir(&workspace).unwrap();

        let segment = format!("history{}", std::process::id());
        let session = rmux_conv::session_name(&segment, "one", None);

        {
            let runner = PaneRunner::new();
            runner
                .start_agent(
                    &session,
                    &session,
                    &workspace,
                    &[],
                    &[
                        "/bin/sh".to_string(),
                        "-c".to_string(),
                        "echo WORK-THAT-HAPPENED; sleep 30".to_string(),
                    ],
                )
                .await
                .unwrap();

            let mirror = runner.mirror(&session).await.unwrap();
            wait_for(|| async { contains(&mirror.attach().await.0.bytes, "WORK-THAT-HAPPENED") })
                .await;
        }

        // A fresh PaneRunner stands in for a daemon restart: no in-memory replay buffer left.
        let restarted = PaneRunner::new();
        restarted
            .ensure_current(&session, &session, rmux_conv::AGENT_WINDOW)
            .await
            .unwrap();

        let mirror = restarted.mirror(&session).await.unwrap();
        wait_for(|| async { contains(&mirror.attach().await.0.bytes, "WORK-THAT-HAPPENED") }).await;

        rmux_conv::kill_session(&session).unwrap();
    }

    /// A held pane renders its exit status into the stream, so every surface shows it without
    /// implementing anything. Needs rmux installed.
    #[tokio::test]
    async fn a_held_pane_renders_its_exit_status() {
        if !rmux_conv::is_available() {
            eprintln!("skipping: rmux not on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("one");
        std::fs::create_dir(&workspace).unwrap();

        let segment = format!("held{}", std::process::id());
        let session = rmux_conv::session_name(&segment, "one", None);
        let runner = PaneRunner::new();

        // `spawn_agent` is the path that sets `remain-on-exit`, which is what holds the pane; the
        // sleep gives it time to land before the process is gone.
        runner
            .start_agent(
                &session,
                &session,
                &workspace,
                &[],
                &[
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "sleep 1; exit 3".to_string(),
                ],
            )
            .await
            .unwrap();

        let mirror = runner.mirror(&session).await.unwrap();
        wait_for(|| async { contains(&mirror.attach().await.0.bytes, "[exited 3") }).await;

        let held = String::from_utf8_lossy(&mirror.attach().await.0.bytes).into_owned();
        assert!(
            held.contains("<ENTER> resume"),
            "an agent pane resumes rather than re-runs: {:?}",
            held
        );
        // The status line lands first, so clients see it before they are told it is over.
        wait_for(|| async { mirror.has_ended() }).await;
        assert_eq!(mirror.state().borrow().exit_code(), Some(3));

        rmux_conv::kill_session(&session).unwrap();
    }

    /// Attaching to a pane that had already exited before anyone looked.
    ///
    /// The case a browser hits on every reload of a workspace whose agent finished while the tab
    /// was closed. A retired mirror used to answer "still current" forever, so re-adopting the
    /// window handed back the corpse of whatever was there first; now it is rebuilt, and a mirror
    /// built over a held pane has to arrive at the same screen and the same status line as one
    /// that watched it die. Needs rmux installed.
    #[tokio::test]
    async fn attaches_to_a_pane_that_had_already_exited() {
        if !rmux_conv::is_available() {
            eprintln!("skipping: rmux not on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("one");
        std::fs::create_dir(&workspace).unwrap();

        let segment = format!("reattach{}", std::process::id());
        let session = rmux_conv::session_name(&segment, "one", None);
        let runner = PaneRunner::new();
        runner
            .start_agent(
                &session,
                &session,
                &workspace,
                &[],
                &[
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "sleep 1; exit 4".to_string(),
                ],
            )
            .await
            .unwrap();

        let first = runner.mirror(&session).await.unwrap();
        wait_for(|| async { first.has_ended() }).await;

        // A held pane is still there, so re-adopting it must produce a mirror rather than an
        // error — and must produce a *fresh* one, because the retired one is not what a client
        // arriving now should be handed.
        runner
            .ensure_current(&session, &session, rmux_conv::AGENT_WINDOW)
            .await
            .expect("a held pane is still a pane");
        let second = runner.mirror(&session).await.unwrap();
        assert!(
            !Arc::ptr_eq(&first, &second),
            "a retired mirror must not be handed to the client that attaches next"
        );

        // Same verdict, arrived at by asking rather than by having watched it happen. What the
        // pane *shows* is rmux's own `remain-on-exit` notice, which it paints over the screen the
        // moment the process goes — so what the work printed is not recoverable from a held pane,
        // by anyone, and the status line is what carries the ending.
        wait_for(|| async { second.has_ended() }).await;
        let screen = String::from_utf8_lossy(&second.attach().await.0.bytes).into_owned();
        assert!(screen.contains("[exited 4"), "{:?}", screen);
        assert!(screen.contains("<ENTER> resume"), "{:?}", screen);
        assert_eq!(second.state().borrow().exit_code(), Some(4));

        rmux_conv::kill_session(&session).unwrap();
    }

    /// Dropping a mirror must not take the runner's shared client down with it. The SDK kills a
    /// whole client when a request future is dropped mid-flight, and a firehose pane keeps the
    /// pump's requests in flight almost continuously. Needs rmux installed.
    #[tokio::test]
    async fn dropping_a_mirror_leaves_the_shared_client_usable() {
        if !rmux_conv::is_available() {
            eprintln!("skipping: rmux not on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("one");
        std::fs::create_dir(&workspace).unwrap();

        let segment = format!("mirrordrop{}", std::process::id());
        let session = rmux_conv::session_name(&segment, "one", None);
        let runner = PaneRunner::new();
        runner
            .start_agent(
                &session,
                &session,
                &workspace,
                &[],
                &[
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "while :; do echo FIREHOSE; done".to_string(),
                ],
            )
            .await
            .unwrap();

        let client = runner.rmux().await.unwrap();
        let pane_id = find_window_pane(&client, &session, rmux_conv::AGENT_WINDOW)
            .await
            .unwrap();

        for _ in 0..10 {
            let mirrored = MirroredPane::attach(&session, pane_id, PaneRole::Agent)
                .await
                .unwrap();
            // Dropping before the pump has issued a request proves nothing.
            wait_for(|| async { contains(&mirrored.mirror().attach().await.0.bytes, "FIREHOSE") })
                .await;
        }

        find_window_pane(&client, &session, rmux_conv::AGENT_WINDOW)
            .await
            .expect("the shared client survives every dropped mirror");

        // Now poison the shared client the way anything outside this crate can — by cancelling a
        // request mid-flight — and check the runner heals. The subject is a window opened behind
        // the runner's back, so nothing is watching it: a watched window is answered from what
        // the watcher last saw and never touches the shared transport at all.
        let unwatched = rmux_conv::open_shell(&session, &workspace).unwrap();
        poison(&client, &session).await;
        assert!(
            toren_mirror::liveness(&client, &session, pane_id)
                .await
                .is_err(),
            "the poisoned client is genuinely dead"
        );
        runner.status(&session, &unwatched).await;
        let replaced = runner.rmux().await.unwrap();
        assert!(
            !Arc::ptr_eq(&client, &replaced),
            "a client found dead must be thrown away rather than kept and retried"
        );
        assert_eq!(
            runner.status(&session, rmux_conv::AGENT_WINDOW).await,
            PaneLiveness::Running,
            "the call after a dead client runs on a fresh connection"
        );

        runner.stop_agent(&session, &session).await.unwrap();
        rmux_conv::kill_session(&session).unwrap();
    }

    /// Kill a client's transport by dropping a request future while its ordered response is
    /// pending — the SDK aborts the whole client when that happens.
    async fn poison(client: &Arc<Rmux>, session: &str) {
        for _ in 0..100 {
            let cancelled = tokio::time::timeout(
                Duration::from_micros(50),
                client.find_panes().session(session).all(),
            )
            .await;
            if cancelled.is_ok() {
                continue;
            }
            let probe: anyhow::Result<_> = client
                .find_panes()
                .session(session)
                .all()
                .await
                .map_err(Into::into);
            if probe
                .as_ref()
                .err()
                .is_some_and(toren_mirror::transport_is_dead)
            {
                return;
            }
        }
        panic!("cancelling requests mid-flight never killed the client");
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        String::from_utf8_lossy(haystack).contains(needle)
    }

    /// rmux spawns asynchronously, so a fixed sleep would be flaky or slow.
    async fn wait_for<F, Fut>(mut condition: F)
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        for _ in 0..100 {
            if condition().await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("condition never became true");
    }
}
