//! Seeding a client from a screen paint rather than a byte replay.
//!
//! rmux caps its retained output at a few hundred KiB, so replaying it can drop the `ESC[?1049h`
//! that put the pane into the alternate screen. The grid then looks right until the app leaves alt
//! screen, at which point the client freezes on a stale frame. A paint carries no such history:
//! read the pane's mode flags, re-enter the alternate screen if it is on, paint `capture-pane -e`,
//! then re-assert the modes.
//!
//! Two modes cannot survive this and are known gaps: bracketed paste (`?2004`) and the DECSC saved
//! cursor, neither of which rmux exposes.

use anyhow::{Context, Result};
use rmux_sdk::{Pane, PaneId, Rmux};

/// Every mode flag rmux answers for a pane, in one round trip.
const MODE_FORMAT: &str = "#{alternate_on}\t#{cursor_x}\t#{cursor_y}\t#{cursor_flag}\
    \t#{scroll_region_upper}\t#{scroll_region_lower}\t#{wrap_flag}\t#{origin_flag}\
    \t#{insert_flag}\t#{keypad_flag}\t#{keypad_cursor_flag}\t#{mouse_standard_flag}\
    \t#{mouse_button_flag}\t#{mouse_any_flag}\t#{mouse_sgr_flag}";

/// The terminal state a pane is in, as far as rmux will say.
///
/// Every field is optional because a mode rmux does not answer is one this cannot re-assert, and
/// guessing wrong is worse than leaving the client on its own default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaneModes {
    pub alternate: bool,
    pub cursor: Option<(u16, u16)>,
    pub cursor_visible: Option<bool>,
    pub scroll_region: Option<(u16, u16)>,
    pub wrap: Option<bool>,
    pub origin: Option<bool>,
    pub insert: Option<bool>,
    pub keypad_application: Option<bool>,
    pub cursor_keys_application: Option<bool>,
    pub mouse_standard: Option<bool>,
    pub mouse_button: Option<bool>,
    pub mouse_any: Option<bool>,
    pub mouse_sgr: Option<bool>,
}

impl PaneModes {
    /// Parse one `display-message -p` line of [`MODE_FORMAT`].
    ///
    /// Unrecognised or absent fields stay unknown rather than defaulting, so an rmux that answers
    /// fewer flags degrades to painting the screen alone.
    pub fn parse(line: &str) -> Self {
        let fields: Vec<&str> = line.trim_end_matches(['\r', '\n']).split('\t').collect();
        let flag = |index: usize| fields.get(index).copied().and_then(parse_flag);
        let number = |index: usize| fields.get(index).and_then(|f| f.trim().parse::<u16>().ok());

        Self {
            alternate: flag(0).unwrap_or(false),
            cursor: number(1).zip(number(2)),
            cursor_visible: flag(3),
            scroll_region: number(4).zip(number(5)),
            wrap: flag(6),
            origin: flag(7),
            insert: flag(8),
            keypad_application: flag(9),
            cursor_keys_application: flag(10),
            mouse_standard: flag(11),
            mouse_button: flag(12),
            mouse_any: flag(13),
            mouse_sgr: flag(14),
        }
    }
}

fn parse_flag(field: &str) -> Option<bool> {
    match field.trim() {
        "1" | "on" | "true" => Some(true),
        "0" | "off" | "false" => Some(false),
        _ => None,
    }
}

/// Read a pane's modes and repaint its screen as bytes any terminal can apply.
pub async fn screen_paint(rmux: &Rmux, pane: &Pane, pane_id: PaneId) -> Result<Vec<u8>> {
    let modes = read_modes(rmux, pane_id).await.unwrap_or_else(|e| {
        tracing::debug!(
            "{}: no mode flags, painting the screen alone: {}",
            pane_id,
            e
        );
        PaneModes::default()
    });
    let screen = pane
        .capture_pane()
        .escape_ansi(true)
        .await
        .with_context(|| format!("Failed to capture pane {}", pane_id))?;
    Ok(paint(&modes, &screen.stdout))
}

async fn read_modes(rmux: &Rmux, pane_id: PaneId) -> Result<PaneModes> {
    let run = rmux
        .cmd([
            "display-message",
            "-p",
            "-t",
            &pane_id.to_string(),
            MODE_FORMAT,
        ])
        .await?;
    Ok(PaneModes::parse(&String::from_utf8_lossy(&run.stdout)))
}

/// The byte sequence that puts a fresh client on the same screen as the pane.
///
/// Order matters: setting a scroll region or origin mode homes the cursor, so both land before the
/// cursor is placed, and the cursor is placed last of all.
pub fn paint(modes: &PaneModes, screen: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(screen.len() + 64);
    if modes.alternate {
        out.extend_from_slice(b"\x1b[?1049h");
    }
    // Reset attributes before painting so a half-applied SGR from before cannot bleed in.
    out.extend_from_slice(b"\x1b[0m\x1b[H\x1b[2J");

    let mut lines: Vec<&[u8]> = screen.split(|b| *b == b'\n').collect();
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(line.strip_suffix(b"\r").unwrap_or(line));
    }

    push_mode(&mut out, modes.wrap, b"\x1b[?7h", b"\x1b[?7l");
    push_mode(&mut out, modes.insert, b"\x1b[4h", b"\x1b[4l");
    push_mode(&mut out, modes.keypad_application, b"\x1b=", b"\x1b>");
    push_mode(
        &mut out,
        modes.cursor_keys_application,
        b"\x1b[?1h",
        b"\x1b[?1l",
    );
    push_mode(
        &mut out,
        modes.mouse_standard,
        b"\x1b[?1000h",
        b"\x1b[?1000l",
    );
    push_mode(&mut out, modes.mouse_button, b"\x1b[?1002h", b"\x1b[?1002l");
    push_mode(&mut out, modes.mouse_any, b"\x1b[?1003h", b"\x1b[?1003l");
    push_mode(&mut out, modes.mouse_sgr, b"\x1b[?1006h", b"\x1b[?1006l");

    if let Some((top, bottom)) = modes.scroll_region {
        out.extend_from_slice(format!("\x1b[{};{}r", top + 1, bottom + 1).as_bytes());
    }
    push_mode(&mut out, modes.origin, b"\x1b[?6h", b"\x1b[?6l");

    if let Some((x, y)) = modes.cursor {
        // Origin mode makes the row relative to the scroll region rmux reported absolutely.
        let top = match (modes.origin, modes.scroll_region) {
            (Some(true), Some((top, _))) => top,
            _ => 0,
        };
        let row = y.saturating_sub(top) + 1;
        out.extend_from_slice(format!("\x1b[{};{}H", row, x + 1).as_bytes());
    }
    push_mode(&mut out, modes.cursor_visible, b"\x1b[?25h", b"\x1b[?25l");

    out
}

fn push_mode(out: &mut Vec<u8>, mode: Option<bool>, set: &[u8], reset: &[u8]) {
    match mode {
        Some(true) => out.extend_from_slice(set),
        Some(false) => out.extend_from_slice(reset),
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(modes: &PaneModes, screen: &[u8]) -> String {
        String::from_utf8(paint(modes, screen)).unwrap()
    }

    #[test]
    fn parses_a_full_mode_line() {
        let modes = PaneModes::parse("1\t7\t3\t1\t0\t23\t1\t0\t0\t1\t1\t0\t0\t0\t1\n");
        assert!(modes.alternate);
        assert_eq!(modes.cursor, Some((7, 3)));
        assert_eq!(modes.cursor_visible, Some(true));
        assert_eq!(modes.scroll_region, Some((0, 23)));
        assert_eq!(modes.wrap, Some(true));
        assert_eq!(modes.origin, Some(false));
        assert_eq!(modes.keypad_application, Some(true));
        assert_eq!(modes.mouse_sgr, Some(true));
    }

    #[test]
    fn leaves_unanswered_flags_unknown() {
        // An rmux that does not know a format key echoes it back rather than answering.
        let modes = PaneModes::parse("0\t0\t0\t#{cursor_flag}");
        assert_eq!(modes.cursor_visible, None);
        assert_eq!(modes.wrap, None, "missing fields are unknown, not false");
        assert_eq!(PaneModes::parse(""), PaneModes::default());
    }

    #[test]
    fn an_unknown_mode_is_not_asserted() {
        let painted = rendered(&PaneModes::default(), b"hi");
        assert_eq!(painted, "\x1b[0m\x1b[H\x1b[2Jhi");
    }

    #[test]
    fn re_enters_the_alternate_screen_first() {
        let modes = PaneModes {
            alternate: true,
            ..PaneModes::default()
        };
        assert!(rendered(&modes, b"vim").starts_with("\x1b[?1049h\x1b[0m\x1b[H\x1b[2J"));
    }

    #[test]
    fn paints_lines_with_carriage_returns_and_no_trailing_scroll() {
        let painted = rendered(&PaneModes::default(), b"one\ntwo\n\n\n");
        assert!(painted.ends_with("one\r\ntwo"), "{:?}", painted);
    }

    #[test]
    fn places_the_cursor_after_the_regions_that_move_it() {
        let modes = PaneModes {
            cursor: Some((4, 9)),
            cursor_visible: Some(true),
            scroll_region: Some((2, 20)),
            origin: Some(false),
            ..PaneModes::default()
        };
        let painted = rendered(&modes, b"screen");
        let region = painted.find("\x1b[3;21r").expect("scroll region");
        let cursor = painted.find("\x1b[10;5H").expect("cursor position");
        assert!(
            region < cursor,
            "the scroll region must be set before the cursor"
        );
        assert!(painted.ends_with("\x1b[?25h"));
    }

    #[test]
    fn origin_mode_makes_the_cursor_row_relative() {
        let modes = PaneModes {
            cursor: Some((0, 9)),
            scroll_region: Some((4, 20)),
            origin: Some(true),
            ..PaneModes::default()
        };
        assert!(rendered(&modes, b"screen").contains("\x1b[6;1H"));
    }
}
