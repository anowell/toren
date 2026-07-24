//! Runs coding agents inside rmux panes and mirrors those panes to connected browsers.
//!
//! Agents are spawned into the same sessions `breq do` uses, so either side can attach to the
//! other's agent without re-spawning it.
//!
//! Every mirrored pane is also recorded to a transcript file. rmux keeps scrollback in daemon
//! memory only, and an exited pane loses its screen entirely, so that file is the only history
//! that survives either.

use anyhow::{anyhow, Context, Result};
use rmux_sdk::{PaneId, PaneOutputChunk, PaneOutputStart, PaneProcessState, Rmux, SessionName};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex, OnceCell, RwLock};
use tracing::{debug, info, warn};

use toren_lib::rmux as rmux_conv;

/// How much output a newly connected browser replays. The transcript keeps the full record.
const REPLAY_CAP_BYTES: usize = 2 * 1024 * 1024;

/// Fan-out depth; a client further behind than this gets a lag notice rather than back-pressuring
/// the recorder.
const BROADCAST_CAPACITY: usize = 512;

/// Prefixed to a backfill that had older output aged out, so the start reads as elision.
const TRUNCATION_NOTICE: &[u8] = b"\x1b[2m[earlier output truncated \xe2\x80\x94 see the transcript for the full record]\x1b[0m\r\n";

/// Liveness of an ancillary's agent pane, as far as rmux is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneStatus {
    /// No session, or no agent window in it.
    Idle,
    Working,
    Exited {
        code: Option<i32>,
    },
}

/// A live mirror of one pane: the bytes seen so far, plus a subscription to what comes next.
pub struct PaneMirror {
    state: Mutex<MirrorState>,
    tx: broadcast::Sender<Arc<Vec<u8>>>,
    /// Set when the pane's stream ends, so clients hear about it instead of staring at a frame
    /// that will never update.
    ended: tokio::sync::watch::Sender<bool>,
}

struct MirrorState {
    /// Trailing window of output, for replay to newly connected clients.
    replay: Vec<u8>,
    truncated: bool,
}

impl PaneMirror {
    fn new(seed: Vec<u8>, truncated: bool) -> Arc<Self> {
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        let (ended, _) = tokio::sync::watch::channel(false);
        Arc::new(Self {
            state: Mutex::new(MirrorState {
                replay: seed,
                truncated,
            }),
            tx,
            ended,
        })
    }

    pub fn ended(&self) -> tokio::sync::watch::Receiver<bool> {
        self.ended.subscribe()
    }

    pub fn has_ended(&self) -> bool {
        *self.ended.borrow()
    }

    fn mark_ended(&self) {
        let _ = self.ended.send(true);
    }

    /// Backfill plus a live subscription, taken under the lock the writer holds while appending
    /// and broadcasting — so a client can neither miss a chunk nor see one twice.
    pub async fn attach(&self) -> (Vec<u8>, broadcast::Receiver<Arc<Vec<u8>>>) {
        let state = self.state.lock().await;
        let rx = self.tx.subscribe();

        let mut backfill = Vec::with_capacity(state.replay.len() + TRUNCATION_NOTICE.len());
        if state.truncated {
            backfill.extend_from_slice(TRUNCATION_NOTICE);
        }
        backfill.extend_from_slice(&state.replay);
        (backfill, rx)
    }

    async fn push(&self, bytes: Arc<Vec<u8>>) {
        let mut state = self.state.lock().await;
        state.replay.extend_from_slice(&bytes);
        if state.replay.len() > REPLAY_CAP_BYTES {
            let drop_to = state.replay.len() - REPLAY_CAP_BYTES;
            // Cut on a line boundary so clients don't start mid-escape-sequence.
            let cut = state.replay[drop_to..]
                .iter()
                .position(|b| *b == b'\n')
                .map_or(drop_to, |offset| drop_to + offset + 1);
            state.replay.drain(..cut);
            state.truncated = true;
        }
        // Send while holding the lock so `attach` cannot interleave.
        let _ = self.tx.send(bytes);
    }
}

/// Everything the daemon tracks for one ancillary's rmux session.
struct TrackedPane {
    session: String,
    /// A different id from the session's live agent pane means this mirror is stale.
    pane_id: PaneId,
    mirror: Arc<PaneMirror>,
    recorder: tokio::task::JoinHandle<()>,
}

impl Drop for TrackedPane {
    fn drop(&mut self) {
        self.recorder.abort();
    }
}

/// Owns the daemon's rmux connection and the set of panes it is mirroring.
pub struct PaneRunner {
    rmux: OnceCell<Arc<Rmux>>,
    tracked: RwLock<HashMap<String, TrackedPane>>,
    transcript_root: PathBuf,
}

impl PaneRunner {
    pub fn new(transcript_root: PathBuf) -> Self {
        Self {
            rmux: OnceCell::new(),
            tracked: RwLock::new(HashMap::new()),
            transcript_root,
        }
    }

    /// Connect to the rmux daemon, starting one if needed.
    ///
    /// Deferred so a machine without rmux still boots the toren daemon.
    async fn rmux(&self) -> Result<Arc<Rmux>> {
        self.rmux
            .get_or_try_init(|| async {
                Rmux::builder()
                    .connect_or_start()
                    .await
                    .map(Arc::new)
                    .context("Failed to reach the rmux daemon. Is `rmux` installed and on PATH?")
            })
            .await
            .cloned()
    }

    /// Start (or restart) an agent for an ancillary and begin mirroring its pane.
    pub async fn start_agent(
        &self,
        ancillary_id: &str,
        segment: &str,
        workspace_path: &Path,
        argv: &[String],
        transcript_name: &str,
    ) -> Result<String> {
        let ws_name = workspace_name(workspace_path)?;
        let session = rmux_conv::session_name(segment, &ws_name);

        // Same helpers `breq` uses, so both interfaces produce identical sessions.
        rmux_conv::ensure_session(&session, workspace_path)?;
        rmux_conv::spawn_agent(&session, workspace_path, argv)?;

        info!(
            "{}: spawned {} in rmux session {}",
            ancillary_id, argv[0], session
        );

        self.track(ancillary_id, &session, transcript_name).await?;
        Ok(session)
    }

    /// Point the ancillary's mirror at the pane running right now.
    ///
    /// Adopts a session this process never started, and replaces a mirror left following a pane
    /// that `breq do` or a resume has since swapped out.
    pub async fn ensure_current(
        &self,
        ancillary_id: &str,
        segment: &str,
        workspace_path: &Path,
        transcript_name: &str,
    ) -> Result<String> {
        let ws_name = workspace_name(workspace_path)?;
        let session = rmux_conv::session_name(segment, &ws_name);
        if !rmux_conv::session_exists(&session) {
            return Err(anyhow!("No rmux session '{}'", session));
        }

        let live_pane_id = self.find_agent(&session).await?.pane_id;

        let is_current = self
            .tracked
            .read()
            .await
            .get(ancillary_id)
            .is_some_and(|t| t.pane_id == live_pane_id && !t.recorder.is_finished());
        if is_current {
            return Ok(session);
        }

        self.track(ancillary_id, &session, transcript_name).await?;
        Ok(session)
    }

    /// Attach a recorder to the session's agent pane, replacing any previous one.
    async fn track(&self, ancillary_id: &str, session: &str, transcript_name: &str) -> Result<()> {
        let discovered = self.find_agent(session).await?;
        let (pane, pane_id) = (discovered.pane, discovered.pane_id);

        let transcript = self.transcript_path(ancillary_id, transcript_name);
        if let Some(parent) = transcript.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }

        // Without this a browser attaching after a restart, or to an exited agent, sees a blank
        // terminal: rmux has no scrollback left for it.
        let (seed, truncated) = read_transcript_tail(&transcript, REPLAY_CAP_BYTES).await;
        let mirror = PaneMirror::new(seed, truncated);

        let cursor = TranscriptCursor::load(&transcript).await;
        let recorder = tokio::spawn(record_pane(
            pane,
            pane_id,
            mirror.clone(),
            transcript,
            cursor,
        ));

        // Clients on the outgoing mirror would otherwise hang on a subscription nothing feeds.
        let previous = self.tracked.write().await.insert(
            ancillary_id.to_string(),
            TrackedPane {
                session: session.to_string(),
                pane_id,
                mirror,
                recorder,
            },
        );
        if let Some(previous) = previous {
            previous.mirror.mark_ended();
        }
        Ok(())
    }

    pub async fn mirror(&self, ancillary_id: &str) -> Option<Arc<PaneMirror>> {
        self.tracked
            .read()
            .await
            .get(ancillary_id)
            .map(|t| t.mirror.clone())
    }

    pub async fn session_of(&self, ancillary_id: &str) -> Option<String> {
        self.tracked
            .read()
            .await
            .get(ancillary_id)
            .map(|t| t.session.clone())
    }

    /// Forward browser keystrokes to the agent pane, verbatim.
    pub async fn send_input(&self, ancillary_id: &str, text: &str) -> Result<()> {
        let session = self
            .session_of(ancillary_id)
            .await
            .ok_or_else(|| anyhow!("No tracked pane for {}", ancillary_id))?;
        self.agent_pane(&session).await?.send_text(text).await?;
        Ok(())
    }

    /// Match the agent's geometry to the browser terminal's.
    ///
    /// Resizes the window, not the pane: a pane can't exceed its window, and a detached window
    /// sits at rmux's 80x24 default, so resizing the pane alone is a silent no-op. `window-size`
    /// is left at its default so an attached human isn't fighting a browser tab for geometry.
    pub async fn resize(&self, ancillary_id: &str, cols: u16, rows: u16) -> Result<()> {
        let session = self
            .session_of(ancillary_id)
            .await
            .ok_or_else(|| anyhow!("No tracked pane for {}", ancillary_id))?;

        self.agent_window(&session)
            .await?
            .resize(Some(cols), Some(rows))
            .await?;
        Ok(())
    }

    /// Liveness of an ancillary's agent, derived from rmux rather than tracked separately.
    pub async fn status(&self, segment: &str, workspace_path: &Path) -> PaneStatus {
        let Ok(ws_name) = workspace_name(workspace_path) else {
            return PaneStatus::Idle;
        };
        let session = rmux_conv::session_name(segment, &ws_name);

        let Ok(pane) = self.agent_pane(&session).await else {
            return PaneStatus::Idle;
        };
        let Ok(info) = pane.info().await else {
            return PaneStatus::Idle;
        };
        let Some(pane_info) = info.panes.first() else {
            return PaneStatus::Idle;
        };

        match pane_info.process {
            PaneProcessState::Running { .. } => PaneStatus::Working,
            PaneProcessState::Exited => PaneStatus::Exited {
                code: pane_info.exit_state.as_ref().and_then(|e| e.code),
            },
            _ => PaneStatus::Idle,
        }
    }

    /// Kill the agent window and stop mirroring, leaving the session's shell alive.
    ///
    /// Works off the workspace rather than what this process tracks, so a `breq do` agent is
    /// equally stoppable. Returns whether an agent was actually running.
    pub async fn stop_agent(
        &self,
        ancillary_id: &str,
        segment: &str,
        workspace_path: &Path,
    ) -> Result<bool> {
        let ws_name = workspace_name(workspace_path)?;
        let session = rmux_conv::session_name(segment, &ws_name);

        if let Some(tracked) = self.tracked.write().await.remove(ancillary_id) {
            // Attached browsers are following a pane that is about to die.
            tracked.mirror.mark_ended();
        }

        if !rmux_conv::agent_is_running(&session) {
            // Still clear out a dead agent window so the next spawn starts from a clean session.
            let _ = rmux_conv::kill_agent(&session);
            return Ok(false);
        }

        rmux_conv::kill_agent(&session)?;
        info!("{}: killed agent window in {}", ancillary_id, session);
        Ok(true)
    }

    /// Resolve the session's `agent` window to a pane handle.
    ///
    /// Matched by window name: indices shift as windows come and go, and pane titles are
    /// unreliable because agents set them via OSC escapes.
    async fn agent_pane(&self, session: &str) -> Result<rmux_sdk::Pane> {
        Ok(self.find_agent(session).await?.pane)
    }

    /// The window containing the session's agent pane.
    async fn agent_window(&self, session: &str) -> Result<rmux_sdk::Window> {
        let window_index = self.find_agent(session).await?.window_index;
        let rmux = self.rmux().await?;
        let name = SessionName::new(session).map_err(|e| anyhow!("{}", e))?;
        Ok(rmux.session(name).await?.window(window_index))
    }

    async fn find_agent(&self, session: &str) -> Result<rmux_sdk::DiscoveredPane> {
        let rmux = self.rmux().await?;
        let panes = rmux
            .find_panes()
            .session(session)
            .all()
            .await
            .with_context(|| format!("Failed to list panes in rmux session '{}'", session))?;

        for discovered in panes {
            let info = discovered.pane.info().await?;
            let is_agent = info.windows.iter().any(|w| {
                w.index == discovered.window_index
                    && w.name.as_deref() == Some(rmux_conv::AGENT_WINDOW)
            });
            if is_agent {
                return Ok(discovered);
            }
        }

        Err(anyhow!(
            "rmux session '{}' has no '{}' window",
            session,
            rmux_conv::AGENT_WINDOW
        ))
    }

    fn transcript_path(&self, ancillary_id: &str, transcript_name: &str) -> PathBuf {
        self.transcript_root
            .join(sanitize_component(ancillary_id))
            .join(format!("{}.raw", sanitize_component(transcript_name)))
    }
}

/// Where a transcript left off, so re-attaching doesn't duplicate history.
///
/// A first attach wants `PaneOutputStart::Oldest`; a re-attach replays output already in the file.
/// rmux gives every chunk a monotonic per-pane sequence, so the last one written resumes exactly.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct TranscriptCursor {
    /// Sequences restart at 0 per pane, so a cursor from another pane must not be used to skip.
    pane_id: Option<u64>,
    last_sequence: Option<u64>,
}

impl TranscriptCursor {
    fn sidecar(transcript: &Path) -> PathBuf {
        transcript.with_extension("raw.cursor")
    }

    async fn load(transcript: &Path) -> Self {
        let Ok(text) = tokio::fs::read_to_string(Self::sidecar(transcript)).await else {
            return Self::default();
        };
        let mut fields = text.trim().split(':');
        let pane_id = fields.next().and_then(|f| f.parse().ok());
        let last_sequence = fields.next().and_then(|f| f.parse().ok());
        Self {
            pane_id,
            last_sequence,
        }
    }

    async fn save(&self, transcript: &Path) {
        let (Some(pane_id), Some(sequence)) = (self.pane_id, self.last_sequence) else {
            return;
        };
        let _ = tokio::fs::write(
            Self::sidecar(transcript),
            format!("{}:{}", pane_id, sequence),
        )
        .await;
    }

    fn already_recorded(&self, pane_id: u64, sequence: u64) -> bool {
        self.pane_id == Some(pane_id) && self.last_sequence.is_some_and(|last| sequence <= last)
    }
}

/// The tail of a transcript, plus whether anything was elided from the front.
async fn read_transcript_tail(transcript: &Path, max_bytes: usize) -> (Vec<u8>, bool) {
    let Ok(bytes) = tokio::fs::read(transcript).await else {
        return (Vec::new(), false);
    };
    if bytes.len() <= max_bytes {
        return (bytes, false);
    }
    let start = bytes.len() - max_bytes;
    let cut = bytes[start..]
        .iter()
        .position(|b| *b == b'\n')
        .map_or(start, |offset| start + offset + 1);
    (bytes[cut..].to_vec(), true)
}

/// Pump one pane's output into its mirror and its transcript file until the stream ends.
async fn record_pane(
    pane: rmux_sdk::Pane,
    pane_id: PaneId,
    mirror: Arc<PaneMirror>,
    transcript: PathBuf,
    mut cursor: TranscriptCursor,
) {
    use tokio::io::AsyncWriteExt;

    let mut stream = match pane
        .output_stream_starting_at(PaneOutputStart::Oldest)
        .await
    {
        Ok(stream) => stream,
        Err(e) => {
            warn!("Failed to subscribe to pane output: {}", e);
            mirror.mark_ended();
            return;
        }
    };

    let pane_key = u64::from(pane_id.as_u32());
    let mut file = match tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&transcript)
        .await
    {
        Ok(file) => Some(file),
        Err(e) => {
            // Degrades history, not the live view — keep streaming.
            warn!("Failed to open transcript {}: {}", transcript.display(), e);
            None
        }
    };

    loop {
        match stream.next().await {
            Ok(Some(PaneOutputChunk::Bytes { sequence, bytes })) => {
                // Already on disk and already in the mirror's seed.
                if cursor.already_recorded(pane_key, sequence) {
                    continue;
                }

                let bytes = Arc::new(bytes);
                if let Some(handle) = file.as_mut() {
                    if let Err(e) = handle.write_all(&bytes).await {
                        warn!("Transcript write failed, dropping transcript: {}", e);
                        file = None;
                    } else {
                        cursor.pane_id = Some(pane_key);
                        cursor.last_sequence = Some(sequence);
                        cursor.save(&transcript).await;
                    }
                }
                mirror.push(bytes).await;
            }
            Ok(Some(PaneOutputChunk::Lag(notice))) => {
                debug!("rmux reported pane output lag: {:?}", notice);
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(e) => {
                debug!("Pane output stream ended: {}", e);
                break;
            }
        }
    }

    if let Some(file) = file.as_mut() {
        let _ = file.flush().await;
    }
    mirror.mark_ended();
}

/// The workspace directory name, which is the ancillary's word-name (`one`, `two`, ...).
fn workspace_name(workspace_path: &Path) -> Result<String> {
    workspace_path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.to_string())
        .ok_or_else(|| {
            anyhow!(
                "Workspace path has no directory name: {}",
                workspace_path.display()
            )
        })
}

/// Make a string safe to use as a single path component.
fn sanitize_component(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_name_is_the_directory_name() {
        let name = workspace_name(Path::new("/ws/toren/one")).unwrap();
        assert_eq!(name, "one");
    }

    #[test]
    fn sanitize_component_replaces_separators() {
        assert_eq!(sanitize_component("Toren One"), "Toren-One");
        assert_eq!(sanitize_component("a/b"), "a-b");
    }

    /// A session created the way `breq do` creates one is mirrored here: backfill, live stream,
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
        let session = rmux_conv::session_name(&segment, "one");
        rmux_conv::ensure_session(&session, &workspace).unwrap();
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

        let runner = PaneRunner::new(dir.path().join("transcripts"));
        wait_for(|| async {
            runner
                .ensure_current("Test One", &segment, &workspace, "assignment")
                .await
                .is_ok()
        })
        .await;

        let mirror = runner.mirror("Test One").await.expect("pane is tracked");

        // Backfill: output produced before we attached.
        wait_for(|| async { contains(&mirror.attach().await.0, "BEFORE-ATTACH") }).await;

        // Live: output produced after, delivered on the subscription taken with the backfill.
        let (_, mut live) = mirror.attach().await;
        runner
            .send_input("Test One", "AFTER-ATTACH\n")
            .await
            .unwrap();

        let mut seen = Vec::new();
        while !contains(&seen, "AFTER-ATTACH") {
            let chunk = tokio::time::timeout(std::time::Duration::from_secs(5), live.recv())
                .await
                .expect("live output arrives")
                .expect("subscription stays open");
            seen.extend_from_slice(&chunk);
        }

        assert_eq!(
            runner.status(&segment, &workspace).await,
            PaneStatus::Working
        );

        // A detached window sits at 80x24, so this guards the window-vs-pane distinction.
        runner.resize("Test One", 100, 30).await.unwrap();
        let snapshot = runner
            .agent_pane(&session)
            .await
            .unwrap()
            .snapshot()
            .await
            .unwrap();
        assert_eq!((snapshot.cols, snapshot.rows), (100, 30));

        assert!(runner
            .stop_agent("Test One", &segment, &workspace)
            .await
            .unwrap());
        // Nothing left to stop, and it must say so rather than report success.
        assert!(!runner
            .stop_agent("Test One", &segment, &workspace)
            .await
            .unwrap());
        rmux_conv::kill_session(&session).unwrap();
    }

    /// Replacing the agent re-points the mirror and releases clients on the old pane; re-adopting
    /// an unchanged pane changes nothing. Needs rmux installed.
    #[tokio::test]
    async fn re_adoption_refreshes_the_mirror_without_duplicating_the_transcript() {
        if !rmux_conv::is_available() {
            eprintln!("skipping: rmux not on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("one");
        std::fs::create_dir(&workspace).unwrap();

        let segment = format!("readopt{}", std::process::id());
        let session = rmux_conv::session_name(&segment, "one");
        let transcripts = dir.path().join("transcripts");
        let runner = PaneRunner::new(transcripts.clone());

        let first_agent = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "echo FIRST-AGENT; sleep 30".to_string(),
        ];
        runner
            .start_agent("Test One", &segment, &workspace, &first_agent, "a-1")
            .await
            .unwrap();

        let first_mirror = runner.mirror("Test One").await.unwrap();
        wait_for(|| async { contains(&first_mirror.attach().await.0, "FIRST-AGENT") }).await;

        runner
            .ensure_current("Test One", &segment, &workspace, "a-1")
            .await
            .unwrap();
        assert!(
            Arc::ptr_eq(&first_mirror, &runner.mirror("Test One").await.unwrap()),
            "re-adopting an unchanged pane should not rebuild the mirror"
        );

        let transcript = transcripts.join("Test-One").join("a-1.raw");
        wait_for(|| async { occurrences(&transcript, "FIRST-AGENT").await == 1 }).await;
        runner
            .ensure_current("Test One", &segment, &workspace, "a-1")
            .await
            .unwrap();
        assert_eq!(
            occurrences(&transcript, "FIRST-AGENT").await,
            1,
            "transcript must not accumulate a second copy of the same output"
        );

        // Replace the agent the way `breq do` does, behind the daemon's back.
        let mut ended = first_mirror.ended();
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
            .ensure_current("Test One", &segment, &workspace, "a-1")
            .await
            .unwrap();

        let second_mirror = runner.mirror("Test One").await.unwrap();
        assert!(
            !Arc::ptr_eq(&first_mirror, &second_mirror),
            "a replaced pane must get a fresh mirror"
        );
        assert!(
            *ended.borrow_and_update(),
            "clients attached to the old pane must be told it ended"
        );
        wait_for(|| async { contains(&second_mirror.attach().await.0, "SECOND-AGENT") }).await;

        runner
            .stop_agent("Test One", &segment, &workspace)
            .await
            .unwrap();
        rmux_conv::kill_session(&session).unwrap();
    }

    /// A pane that has already exited still shows what the run left behind. Needs rmux installed.
    #[tokio::test]
    async fn history_survives_the_pane_it_came_from() {
        if !rmux_conv::is_available() {
            eprintln!("skipping: rmux not on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("one");
        std::fs::create_dir(&workspace).unwrap();

        let segment = format!("history{}", std::process::id());
        let session = rmux_conv::session_name(&segment, "one");
        let transcripts = dir.path().join("transcripts");

        {
            let runner = PaneRunner::new(transcripts.clone());
            runner
                .start_agent(
                    "Test One",
                    &segment,
                    &workspace,
                    &[
                        "/bin/sh".to_string(),
                        "-c".to_string(),
                        "echo WORK-THAT-HAPPENED; sleep 30".to_string(),
                    ],
                    "a-1",
                )
                .await
                .unwrap();

            let mirror = runner.mirror("Test One").await.unwrap();
            wait_for(|| async { contains(&mirror.attach().await.0, "WORK-THAT-HAPPENED") }).await;
        }

        // A fresh PaneRunner stands in for a daemon restart: no in-memory replay buffer left.
        let restarted = PaneRunner::new(transcripts);
        restarted
            .ensure_current("Test One", &segment, &workspace, "a-1")
            .await
            .unwrap();

        let mirror = restarted.mirror("Test One").await.unwrap();
        let (backfill, _) = mirror.attach().await;
        assert!(
            contains(&backfill, "WORK-THAT-HAPPENED"),
            "a restarted daemon should replay the transcript, got {:?}",
            String::from_utf8_lossy(&backfill)
        );

        rmux_conv::kill_session(&session).unwrap();
    }

    async fn occurrences(path: &Path, needle: &str) -> usize {
        let Ok(bytes) = tokio::fs::read(path).await else {
            return 0;
        };
        String::from_utf8_lossy(&bytes).matches(needle).count()
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

    #[tokio::test]
    async fn mirror_replays_then_streams() {
        let mirror = PaneMirror::new(Vec::new(), false);
        mirror.push(Arc::new(b"hello ".to_vec())).await;

        let (replay, mut rx) = mirror.attach().await;
        assert_eq!(replay, b"hello ");

        mirror.push(Arc::new(b"world".to_vec())).await;
        assert_eq!(rx.recv().await.unwrap().as_slice(), b"world");
    }

    #[tokio::test]
    async fn mirror_caps_replay_and_marks_truncation() {
        let mirror = PaneMirror::new(Vec::new(), false);
        let chunk = vec![b'x'; REPLAY_CAP_BYTES / 2 + 1];
        mirror.push(Arc::new(chunk.clone())).await;
        mirror.push(Arc::new(chunk)).await;

        let state = mirror.state.lock().await;
        assert!(state.replay.len() <= REPLAY_CAP_BYTES);
        assert!(state.truncated);
    }

    #[tokio::test]
    async fn mirror_announces_truncation_to_new_clients() {
        let mirror = PaneMirror::new(Vec::new(), false);
        mirror
            .push(Arc::new(vec![b'x'; REPLAY_CAP_BYTES + 1]))
            .await;

        let (backfill, _) = mirror.attach().await;
        assert!(backfill.starts_with(TRUNCATION_NOTICE));
    }

    #[tokio::test]
    async fn mirror_trims_on_a_line_boundary_when_it_can() {
        let mirror = PaneMirror::new(Vec::new(), false);
        let mut first = vec![b'a'; REPLAY_CAP_BYTES];
        first.push(b'\n');
        first.extend_from_slice(b"tail");
        mirror.push(Arc::new(first)).await;

        let replay = mirror.state.lock().await.replay.clone();
        assert_eq!(replay, b"tail", "trim should cut just past the newline");
    }
}
