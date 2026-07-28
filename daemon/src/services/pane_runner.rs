//! Runs coding agents inside rmux panes and mirrors those panes to connected browsers.
//!
//! Agents are spawned into the same sessions `breq do` uses, so either side can attach to the
//! other's agent without re-spawning it.
//!
//! The mirroring itself lives in `toren-mirror`, which the local `breq` client uses too; what is
//! here is the daemon's bookkeeping — which pane each browser-facing key is following, and the
//! window-level lifecycle (spawn, open a shell, kill) that browsers drive.

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use toren_mirror::{
    connect, find_window_pane, transport_is_dead, MirroredPane, PaneLiveness, PaneMirror, PaneRole,
    Rmux,
};
use tracing::info;

use toren_lib::rmux as rmux_conv;

/// Everything the daemon tracks for one window of one rmux session.
struct TrackedPane {
    session: String,
    /// Keyed by `PaneId`, not by the window name it was adopted from: rmux recompresses indices
    /// and a resume replaces the pane behind the name, but the id a mirror holds is either the
    /// pane it started with or gone.
    pane: MirroredPane,
}

/// Owns the daemon's rmux connection and the set of panes it is mirroring.
pub struct PaneRunner {
    rmux: Mutex<Option<Arc<Rmux>>>,
    tracked: RwLock<HashMap<String, TrackedPane>>,
}

impl PaneRunner {
    pub fn new() -> Self {
        Self {
            rmux: Mutex::new(None),
            tracked: RwLock::new(HashMap::new()),
        }
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

    /// Start (or restart) an agent in a workspace's session and begin mirroring its pane.
    ///
    /// The session name comes from the caller, which derives it from the workspace's
    /// state — that's what keys it to the instance uid.
    pub async fn start_agent(
        &self,
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

        self.track(key, session, rmux_conv::AGENT_WINDOW).await?;
        Ok(session.to_string())
    }

    /// Open a fresh shell window in the workspace's session, returning its window name.
    ///
    /// The browser can hold several shells at once (a dev server, a log tail, a scratch prompt),
    /// so each gets a unique name; the caller then mirrors it like any other window.
    pub async fn open_shell(
        &self,
        session: &str,
        workspace_path: &Path,
        env: &[(String, String)],
    ) -> Result<String> {
        rmux_conv::ensure_session(session, workspace_path, env)?;
        let window = rmux_conv::open_shell(session, workspace_path)?;
        info!("opened shell window '{}' in {}", window, session);
        Ok(window)
    }

    /// Run a one-shot command in a `cmd` window of its own, returning that window's name.
    ///
    /// Held, so the output of something that ran and finished stays readable until it is
    /// dismissed — which is why the browser drives workflow scripts through here instead of
    /// streaming their output over HTTP.
    pub async fn run_command(
        &self,
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
        Ok(window)
    }

    /// Point a mirror at the pane running right now in `window`.
    ///
    /// Adopts a session this process never started — which is all re-adoption after a restart
    /// amounts to, since the daemon persists nothing — and replaces a mirror left following a pane
    /// that `breq do` or a resume has since swapped out. `key` scopes the mirror to one window, so
    /// a workspace can have its agent and several shells mirrored at once.
    pub async fn ensure_current(&self, key: &str, session: &str, window: &str) -> Result<String> {
        if !rmux_conv::session_exists(session) {
            return Err(anyhow!("No rmux session '{}'", session));
        }

        let rmux = self.rmux().await?;
        let live = self
            .healing(&rmux, find_window_pane(&rmux, session, window).await)
            .await?;

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
    async fn track(&self, key: &str, session: &str, window: &str) -> Result<()> {
        let rmux = self.rmux().await?;
        let pane_id = self
            .healing(&rmux, find_window_pane(&rmux, session, window).await)
            .await?;
        let pane = self
            .healing(
                &rmux,
                MirroredPane::attach(rmux.clone(), session, pane_id, role_of(window)).await,
            )
            .await?;

        // Dropping the outgoing mirror releases the clients still attached to it.
        self.tracked.write().await.insert(
            key.to_string(),
            TrackedPane {
                session: session.to_string(),
                pane,
            },
        );
        Ok(())
    }

    pub async fn mirror(&self, key: &str) -> Option<Arc<PaneMirror>> {
        self.tracked.read().await.get(key).map(|t| t.pane.mirror())
    }

    pub async fn session_of(&self, key: &str) -> Option<String> {
        self.tracked
            .read()
            .await
            .get(key)
            .map(|t| t.session.clone())
    }

    /// Forward browser keystrokes to the mirrored pane, verbatim.
    pub async fn send_input(&self, key: &str, text: &str) -> Result<()> {
        let (client, result) = {
            let tracked = self.tracked.read().await;
            let pane = &self.require(&tracked, key)?.pane;
            (pane.client(), pane.send_text(text).await)
        };
        self.healing(&client, result).await
    }

    /// Repaint a mirrored pane from its screen, returning the epoch the paint opens.
    ///
    /// What a browser asks for when its terminal has gone out of step — because it fell behind, or
    /// because rmux dropped output on the way here. The paint reaches every client attached to the
    /// pane, not just the one that asked; a fresh screen is never wrong for any of them.
    pub async fn resync(&self, key: &str) -> Result<u32> {
        let (client, result) = {
            let tracked = self.tracked.read().await;
            let pane = &self.require(&tracked, key)?.pane;
            (pane.client(), pane.repaint().await)
        };
        self.healing(&client, result).await
    }

    /// Match the mirrored pane's geometry to the browser terminal's.
    ///
    /// `window-size` is left at its default so an attached human isn't fighting a browser tab for
    /// geometry.
    pub async fn resize(&self, key: &str, cols: u16, rows: u16) -> Result<()> {
        let (client, result) = {
            let tracked = self.tracked.read().await;
            let pane = &self.require(&tracked, key)?.pane;
            (pane.client(), pane.resize(cols, rows).await)
        };
        self.healing(&client, result).await
    }

    fn require<'a>(
        &self,
        tracked: &'a HashMap<String, TrackedPane>,
        key: &str,
    ) -> Result<&'a TrackedPane> {
        tracked
            .get(key)
            .ok_or_else(|| anyhow!("No tracked pane for {}", key))
    }

    /// Liveness of a window's current pane, derived from rmux rather than tracked separately.
    ///
    /// Works off the window name because callers ask before anything is mirrored; the name is
    /// resolved to a pane id and the answer is that pane's, not whichever pane rmux lists first.
    pub async fn status(&self, session: &str, window: &str) -> PaneLiveness {
        let Ok(rmux) = self.rmux().await else {
            return PaneLiveness::Unknown;
        };
        let found = self
            .healing(&rmux, find_window_pane(&rmux, session, window).await)
            .await;
        let Ok(pane_id) = found else {
            return PaneLiveness::Unknown;
        };
        self.healing(&rmux, toren_mirror::liveness(&rmux, session, pane_id).await)
            .await
            .unwrap_or(PaneLiveness::Unknown)
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

        let was_live = rmux_conv::window_exists(session, window);
        // Kill even a dead window so the next spawn of that name starts clean.
        rmux_conv::kill_window(session, window)?;
        if was_live {
            info!("{}: killed window '{}' in {}", key, window, session);
        }
        Ok(was_live)
    }

    /// The pane a mirror is following.
    #[cfg(test)]
    async fn tracked_pane_id(&self, key: &str) -> Option<toren_mirror::PaneId> {
        self.tracked.read().await.get(key).map(|t| t.pane.pane_id())
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

        // The seed is a paint of the pane's screen, so output from before the attach is there.
        wait_for(|| async { contains(&mirror.attach().await.0.bytes, "BEFORE-ATTACH") }).await;

        // Live: output produced after, delivered on the subscription taken with the backfill.
        let (_, mut live) = mirror.attach().await;
        runner.send_input(&session, "AFTER-ATTACH\n").await.unwrap();

        let mut seen = Vec::new();
        while !contains(&seen, "AFTER-ATTACH") {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(5), live.recv())
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
            let frame = tokio::time::timeout(std::time::Duration::from_secs(5), live.recv())
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

        // A detached window sits at 80x24, so this guards the window-vs-pane distinction.
        runner.resize(&session, 100, 30).await.unwrap();
        let snapshot = rmux
            .pane_by_id(toren_mirror::SessionName::new(&session).unwrap(), pane_id)
            .await
            .unwrap()
            .snapshot()
            .await
            .unwrap();
        assert_eq!((snapshot.cols, snapshot.rows), (100, 30));

        assert!(runner.stop_agent(&session, &session).await.unwrap());
        // Nothing left to stop, and it must say so rather than report success.
        assert!(!runner.stop_agent(&session, &session).await.unwrap());
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

    /// Dropping a mirror must not take the shared rmux client down with it. The SDK kills the
    /// whole client when a request future is dropped mid-flight, and a firehose pane keeps the
    /// pump's requests in flight almost continuously — so an aborted (rather than cooperatively
    /// stopped) pump would poison the client here almost every iteration. Needs rmux installed.
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
            let mirrored = MirroredPane::attach(client.clone(), &session, pane_id, PaneRole::Agent)
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
        // request mid-flight — and check the runner heals: the poisoned call fails and discards
        // the client, the next one reconnects.
        poison(&client, &session).await;
        runner
            .ensure_current(&session, &session, rmux_conv::AGENT_WINDOW)
            .await
            .expect_err("a poisoned client fails the call that finds it dead");
        runner
            .ensure_current(&session, &session, rmux_conv::AGENT_WINDOW)
            .await
            .expect("the call after a dead client runs on a fresh connection");

        runner.stop_agent(&session, &session).await.unwrap();
        rmux_conv::kill_session(&session).unwrap();
    }

    /// Kill a client's transport by dropping a request future while its ordered response is
    /// pending — the SDK aborts the whole client when that happens.
    async fn poison(client: &Arc<Rmux>, session: &str) {
        for _ in 0..100 {
            let cancelled = tokio::time::timeout(
                std::time::Duration::from_micros(50),
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
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("condition never became true");
    }
}
