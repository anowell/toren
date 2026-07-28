//! One rmux pane, mirrored for as many clients as care to watch it.
//!
//! The daemon streams a mirror to browser terminals and `breq` renders one in the local tty. Both
//! read the same bytes from the same place, so a pane looks the same everywhere it is shown, and
//! anything drawn into the stream here (a held pane's status line, a re-seed after a lag) reaches
//! every surface without either of them implementing it.
//!
//! The surface is small on purpose: bytes out ([`PaneMirror`]), input in
//! ([`MirroredPane::send_text`]), geometry ([`MirroredPane::resize`]), and a screen-paint seed
//! ([`screen_paint`]).
//!
//! Every fanned-out chunk is stamped ([`Frame`]) with the screen generation it belongs to and how
//! far into the stream it lands. A re-seed opens a new generation, so a client can drop what was
//! queued behind a paint rather than apply it on top, and can measure how far behind it has fallen
//! in bytes rather than in chunks.
//!
//! Panes are addressed by [`PaneId`] throughout. Window and pane indices shift as windows come and
//! go, so nothing here holds one: every targeted call re-resolves the window index from the pane's
//! own window id.

mod buffer;
mod filter;
mod held;
mod pane;
mod seed;

pub use buffer::{Backfill, Frame, MirrorState, PaneMirror, LAG_BUDGET_BYTES};
pub use filter::QueryFilter;
pub use held::{held_status_line, PaneRole};
pub use pane::{
    connect, find_window_pane, liveness, transport_is_dead, MirroredPane, PaneLiveness,
};
pub use seed::{paint, screen_paint, PaneModes};

pub use rmux_sdk::{PaneExitState, PaneId, Rmux, SessionName};
