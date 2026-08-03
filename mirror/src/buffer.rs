//! The fan-out half of a mirror: what a pane has shown, plus what it shows next.

use rmux_sdk::PaneExitState;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, watch, Mutex};

use crate::held::{held_status_line, PaneRole};

/// How much output a newly attached client replays.
const REPLAY_CAP_BYTES: usize = 2 * 1024 * 1024;

/// Fan-out depth. The channel needs some bound, but a chunk is anywhere from a keystroke's echo to
/// a megabyte of `cat`, so this is not the limit that matters — [`LAG_BUDGET_BYTES`] is.
const BROADCAST_CAPACITY: usize = 512;

/// How far behind a client may fall before sending it what it missed costs more than putting it
/// back on the pane's current screen, which is a paint of a few tens of KiB whatever happened.
pub const LAG_BUDGET_BYTES: u64 = 256 * 1024;

/// One unit of fan-out: pane bytes, and where they sit in the stream.
#[derive(Debug, Clone)]
pub struct Frame {
    /// Which generation of the pane's screen these bytes belong to.
    ///
    /// A re-seed paints the whole screen, so bytes from before it are not merely old but wrong;
    /// the epoch is how every client downstream tells the two apart and drops them.
    pub epoch: u32,
    /// Total bytes broadcast through this frame, so how far behind a client is can be read off
    /// rather than counted.
    pub position: u64,
    pub bytes: Arc<Vec<u8>>,
}

/// What a client applies before its first live frame: the pane's screen so far, and the epoch that
/// screen belongs to.
#[derive(Debug, Clone, Default)]
pub struct Backfill {
    pub epoch: u32,
    pub bytes: Vec<u8>,
}

/// Whether a mirror is still following a live process, and what became of it if not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum MirrorState {
    #[default]
    Live,
    /// The pane is alive, and this mirror cannot follow it at the moment.
    ///
    /// The distinction from [`Self::Ended`] is the whole point: a mirror that could not open its
    /// output subscription — because rmux caps them per connection — knows nothing about the
    /// pane's process, and saying "ended" there is a lie a client acts on. Degraded says the
    /// screen is stale, not that the work is over, and the pump keeps trying.
    Degraded { reason: String },
    /// Nothing more will arrive. `exit` is what rmux recorded for the pane's process, and is
    /// `None` when the mirror was retired for another reason — the pane was replaced, or rmux
    /// never observed a status for it.
    Ended { exit: Option<PaneExitState> },
}

impl MirrorState {
    pub fn is_ended(&self) -> bool {
        matches!(self, Self::Ended { .. })
    }

    pub fn is_degraded(&self) -> bool {
        matches!(self, Self::Degraded { .. })
    }

    /// The pane's exit code, for a client that reports it onwards (a local mirror exits with it).
    pub fn exit_code(&self) -> Option<i32> {
        match self {
            Self::Ended { exit: Some(exit) } => exit.code,
            _ => None,
        }
    }

    /// The word a client puts on this state.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Live => "attached",
            Self::Degraded { .. } => "degraded",
            Self::Ended { .. } => "ended",
        }
    }
}

/// A live mirror of one pane: the bytes seen so far, plus a subscription to what comes next.
pub struct PaneMirror {
    replay: Mutex<Vec<u8>>,
    tx: broadcast::Sender<Frame>,
    /// Both only ever move forward, and both are written under the replay lock, so a backfill and
    /// the frames that follow it always agree.
    epoch: AtomicU32,
    position: AtomicU64,
    /// Set when the pane's stream ends, so clients hear about it instead of staring at a frame
    /// that will never update.
    state: watch::Sender<MirrorState>,
    /// What the pane runs, which is what its status line offers once it has exited.
    role: PaneRole,
}

impl PaneMirror {
    pub(crate) fn new(role: PaneRole) -> Arc<Self> {
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        let (state, _) = watch::channel(MirrorState::Live);
        Arc::new(Self {
            replay: Mutex::new(Vec::new()),
            tx,
            epoch: AtomicU32::new(0),
            position: AtomicU64::new(0),
            state,
            role,
        })
    }

    /// Backfill plus a live subscription, taken under the lock the pump holds while appending and
    /// broadcasting — so a client can neither miss a chunk nor see one twice.
    pub async fn attach(&self) -> (Backfill, broadcast::Receiver<Frame>) {
        let replay = self.replay.lock().await;
        let rx = self.tx.subscribe();
        let backfill = Backfill {
            epoch: self.epoch.load(Ordering::Relaxed),
            bytes: replay.clone(),
        };
        (backfill, rx)
    }

    /// The screen generation the mirror is on now.
    pub fn epoch(&self) -> u32 {
        self.epoch.load(Ordering::Relaxed)
    }

    /// How many clients are reading this mirror right now.
    ///
    /// What the pump slows down for. A mirror with no readers still has to drain its subscription
    /// — rmux's retained output is a bounded ring, and letting it overflow costs a screen paint —
    /// but there is nobody for it to be quick about it for.
    pub fn viewers(&self) -> usize {
        self.tx.receiver_count()
    }

    /// How many bytes have been broadcast since `frame` — what a client still owes the pane if it
    /// applies it.
    pub fn bytes_behind(&self, frame: &Frame) -> u64 {
        self.position
            .load(Ordering::Relaxed)
            .saturating_sub(frame.position)
    }

    pub fn state(&self) -> watch::Receiver<MirrorState> {
        self.state.subscribe()
    }

    pub(crate) fn role(&self) -> PaneRole {
        self.role
    }

    pub fn has_ended(&self) -> bool {
        self.state.borrow().is_ended()
    }

    /// What rmux recorded for the pane's process, once it has one — the mark of a mirror that
    /// ended because its pane exited, rather than one retired while the pane lived on.
    pub fn exit(&self) -> Option<PaneExitState> {
        match &*self.state.borrow() {
            MirrorState::Ended { exit } => exit.clone(),
            MirrorState::Live | MirrorState::Degraded { .. } => None,
        }
    }

    pub(crate) fn mark_ended(&self, exit: Option<PaneExitState>) {
        // Replaced rather than sent: a mirror nobody is watching yet must still remember that its
        // pane is over, for the client that attaches next.
        self.state.send_replace(MirrorState::Ended { exit });
    }

    /// Say the screen has stopped moving without saying the pane is over.
    ///
    /// Ignored once a mirror has ended: an exit is final, and a late failure to re-subscribe to a
    /// pane that is already gone must not walk that back.
    pub(crate) fn mark_degraded(&self, reason: impl Into<String>) {
        let reason = reason.into();
        self.state.send_if_modified(|state| {
            if state.is_ended()
                || matches!(state, MirrorState::Degraded { reason: held } if *held == reason)
            {
                return false;
            }
            *state = MirrorState::Degraded { reason };
            true
        });
    }

    /// Say the mirror is following the pane again, after a spell of not being able to.
    pub(crate) fn mark_live(&self) {
        self.state.send_if_modified(|state| {
            if !state.is_degraded() {
                return false;
            }
            *state = MirrorState::Live;
            true
        });
    }

    pub(crate) async fn push(&self, bytes: Arc<Vec<u8>>) {
        let mut replay = self.replay.lock().await;
        replay.extend_from_slice(&bytes);
        if replay.len() > REPLAY_CAP_BYTES {
            let drop_to = replay.len() - REPLAY_CAP_BYTES;
            // Cut on a line boundary so clients don't start mid-escape-sequence.
            let cut = replay[drop_to..]
                .iter()
                .position(|b| *b == b'\n')
                .map_or(drop_to, |offset| drop_to + offset + 1);
            replay.drain(..cut);
        }
        // Send while holding the lock so `attach` cannot interleave.
        self.broadcast(bytes);
    }

    /// Replace the backfill with a fresh screen paint and push it to everyone already attached,
    /// returning the epoch it opens.
    ///
    /// A paint describes the whole screen, so what came before it can only contradict it; the new
    /// epoch is what lets clients throw those bytes away instead of applying them over it.
    ///
    /// A held pane's status line is drawn into its stream rather than onto its screen, so a paint
    /// alone would drop it; the paint ends the way the stream did instead.
    pub(crate) async fn reseed(&self, mut paint: Vec<u8>) -> u32 {
        if let Some(exit) = self.exit() {
            paint.extend_from_slice(&held_status_line(&exit, self.role));
        }
        let mut replay = self.replay.lock().await;
        replay.clear();
        replay.extend_from_slice(&paint);
        let epoch = self.epoch.fetch_add(1, Ordering::Relaxed) + 1;
        self.broadcast(Arc::new(paint));
        epoch
    }

    /// Stamp bytes with where they land in the stream and fan them out. Called under the replay
    /// lock, which is what keeps the stamps in order.
    fn broadcast(&self, bytes: Arc<Vec<u8>>) {
        let position = self
            .position
            .fetch_add(bytes.len() as u64, Ordering::Relaxed)
            + bytes.len() as u64;
        let _ = self.tx.send(Frame {
            epoch: self.epoch.load(Ordering::Relaxed),
            position,
            bytes,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn replays_then_streams() {
        let mirror = PaneMirror::new(PaneRole::Shell);
        mirror.push(Arc::new(b"hello ".to_vec())).await;

        let (backfill, mut rx) = mirror.attach().await;
        assert_eq!(backfill.bytes, b"hello ");

        mirror.push(Arc::new(b"world".to_vec())).await;
        let frame = rx.recv().await.unwrap();
        assert_eq!(frame.bytes.as_slice(), b"world");
        assert_eq!(frame.epoch, backfill.epoch, "no re-seed, no new epoch");
    }

    #[tokio::test]
    async fn caps_its_replay_buffer() {
        let mirror = PaneMirror::new(PaneRole::Shell);
        let chunk = vec![b'x'; REPLAY_CAP_BYTES / 2 + 1];
        mirror.push(Arc::new(chunk.clone())).await;
        mirror.push(Arc::new(chunk)).await;

        assert!(mirror.replay.lock().await.len() <= REPLAY_CAP_BYTES);
    }

    #[tokio::test]
    async fn trims_on_a_line_boundary_when_it_can() {
        let mirror = PaneMirror::new(PaneRole::Shell);
        let mut first = vec![b'a'; REPLAY_CAP_BYTES];
        first.push(b'\n');
        first.extend_from_slice(b"tail");
        mirror.push(Arc::new(first)).await;

        let replay = mirror.replay.lock().await.clone();
        assert_eq!(replay, b"tail", "trim should cut just past the newline");
    }

    #[tokio::test]
    async fn a_reseed_replaces_the_backfill_and_reaches_live_clients() {
        let mirror = PaneMirror::new(PaneRole::Shell);
        mirror.push(Arc::new(b"stale frame".to_vec())).await;
        let (backfill, mut rx) = mirror.attach().await;

        let epoch = mirror.reseed(b"fresh paint".to_vec()).await;

        let frame = rx.recv().await.unwrap();
        assert_eq!(frame.bytes.as_slice(), b"fresh paint");
        assert_eq!(frame.epoch, epoch);
        assert!(
            epoch > backfill.epoch,
            "a paint the client has not applied yet must outrank what it has"
        );
        assert_eq!(mirror.attach().await.0.bytes, b"fresh paint");
        assert_eq!(mirror.epoch(), epoch);
    }

    #[tokio::test]
    async fn a_frame_says_how_far_behind_the_client_holding_it_is() {
        let mirror = PaneMirror::new(PaneRole::Shell);
        let (_, mut rx) = mirror.attach().await;
        mirror.push(Arc::new(b"first".to_vec())).await;

        let frame = rx.recv().await.unwrap();
        assert_eq!(mirror.bytes_behind(&frame), 0, "nothing queued behind it");

        mirror.push(Arc::new(vec![b'x'; 4096])).await;
        assert_eq!(mirror.bytes_behind(&frame), 4096);
    }

    #[tokio::test]
    async fn a_paint_of_a_held_pane_still_ends_with_its_status_line() {
        let mirror = PaneMirror::new(PaneRole::Agent);
        mirror.mark_ended(Some(PaneExitState::from_code(0)));

        mirror.reseed(b"fresh paint".to_vec()).await;

        let painted = String::from_utf8(mirror.attach().await.0.bytes).unwrap();
        assert!(painted.starts_with("fresh paint"));
        assert!(
            painted.contains("<ENTER> resume,"),
            "the affordances are not on the pane's screen, so a paint has to re-draw them"
        );
    }

    #[tokio::test]
    async fn a_paint_of_a_live_pane_is_only_the_paint() {
        let mirror = PaneMirror::new(PaneRole::Shell);
        mirror.reseed(b"fresh paint".to_vec()).await;

        assert_eq!(mirror.attach().await.0.bytes, b"fresh paint");
    }

    #[tokio::test]
    async fn ending_carries_the_exit_status() {
        let mirror = PaneMirror::new(PaneRole::Shell);
        let mut state = mirror.state();
        assert!(!mirror.has_ended());

        mirror.mark_ended(Some(PaneExitState::from_code(3)));

        state.changed().await.unwrap();
        assert!(mirror.has_ended());
        assert_eq!(state.borrow().exit_code(), Some(3));
    }
}
