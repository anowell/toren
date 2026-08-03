//! The mirror as the single authority on terminal queries, in both directions.
//!
//! A query and its answer are a conversation between one program and one terminal. A mirror puts
//! N terminals where the one was, so the conversation goes wrong in two ways at once:
//!
//! * **Outbound** (pane → viewers). rmux answers CPR, DA1/2/3, XTVERSION, DSR, DECRQM, OSC colour
//!   queries and XTGETTCAP itself *and* forwards the query on. Every attached viewer's terminal
//!   answers it too, so each mirror adds one duplicate reply: junk in the shell buffer
//!   (`zsh: command not found: 1R`) and protocol desync in anything parsing what comes back.
//! * **Inbound** (viewers → pane). Those duplicate answers travel back as keystrokes. N viewers
//!   means N replies to a question asked once, arriving at a program that read its answer long
//!   ago — so they land on whatever reads stdin next. A reply outliving its querier is exactly
//!   the stray-character symptom.
//!
//! The fix both mosh and tmux settled on is that the thing in the middle answers or drops, and
//! never answers *and* forwards: an app that sends a probe followed by DA1 and takes whichever
//! answers first (the "DA1 fence") gets a coherent answer from one authority, or none at all.
//! Since rmux has already answered outbound, dropping costs nothing — nobody is waiting on a
//! reply that never comes — and dropping inbound costs nothing either, because the only thing
//! upstream ever asked has already been told.
//!
//! What is deliberately *not* filtered: APC, PM and SOS strings, and any DCS that is not a query.
//! Those carry kitty graphics, Sixel and terminfo payloads, which are content rather than
//! conversation. They are parsed here only so their payloads cannot be mistaken for queries.

const ESC: u8 = 0x1B;
const BEL: u8 = 0x07;

/// Longest CSI held while deciding. Past this a run is malformed, and withholding it would be
/// worse than passing it through.
const MAX_SEQUENCE: usize = 32;

/// Longest string-sequence prefix held while deciding whether it is a query.
///
/// Every query in this family is short — `10;?`, `4;1;?`, `+q<hex>`. A long one is a payload
/// (an OSC 52 clipboard write, a Sixel), so exceeding this is itself the answer: pass it.
const MAX_STRING_PREFIX: usize = 64;

/// Which conversation a filter is standing in the middle of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Pane to viewers: drop the questions rmux has already answered.
    FromPane,
    /// Viewers to pane: drop the answers no one is waiting for.
    ToPane,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    /// An `ESC` with nothing after it yet.
    Escape,
    /// Buffering a CSI, deciding.
    Csi,
    /// Buffering an OSC, deciding.
    Osc,
    /// Buffering a DCS, deciding.
    Dcs,
    /// Passing a string sequence through verbatim until its terminator.
    Opaque {
        esc: bool,
    },
    /// Dropping a string sequence until its terminator.
    Swallow {
        esc: bool,
    },
}

/// A streaming filter over one direction of one pane's traffic.
///
/// Chunk boundaries fall wherever the daemon, the socket or the keyboard put them, including
/// inside an escape sequence, so a partially seen sequence is held until the next chunk decides
/// it. One filter per stream: the held bytes are that stream's.
#[derive(Debug)]
pub struct QueryFilter {
    direction: Direction,
    state: State,
    pending: Vec<u8>,
}

impl Default for QueryFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryFilter {
    /// A filter over pane output, which is what most of this crate means by "the filter".
    pub fn new() -> Self {
        Self::for_direction(Direction::FromPane)
    }

    /// A filter over client input.
    pub fn inbound() -> Self {
        Self::for_direction(Direction::ToPane)
    }

    pub fn for_direction(direction: Direction) -> Self {
        Self {
            direction,
            state: State::Ground,
            pending: Vec::new(),
        }
    }

    /// Feed a chunk; returns what should be passed on.
    ///
    /// Outbound, a chunk boundary means nothing — the daemon put it wherever it liked, including
    /// mid-sequence — so an undecided sequence is held for the next chunk.
    ///
    /// Inbound, a chunk boundary is the whole story. A terminal tells `ESC` the key from `ESC`
    /// the introducer by exactly this: a sequence arrives in one read, a pressed Escape arrives
    /// alone. So input is decided within its chunk and never held, which is also what keeps a
    /// keystroke from waiting on the next one.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pending.len() + chunk.len());
        for &byte in chunk {
            self.step(byte, &mut out);
        }
        if self.direction == Direction::ToPane {
            out.append(&mut self.pending);
            self.state = State::Ground;
        }
        out
    }

    /// Filter text, for the input path — rmux takes `&str`, not bytes.
    ///
    /// Dropping whole escape sequences only ever removes ASCII runs, so what is left is still
    /// valid UTF-8; the lossy conversion is belt and braces.
    pub fn push_text(&mut self, text: &str) -> String {
        String::from_utf8_lossy(&self.push(text.as_bytes())).into_owned()
    }

    /// Forget a half-seen sequence, for a stream that just skipped bytes.
    pub fn reset(&mut self) {
        self.state = State::Ground;
        self.pending.clear();
    }

    fn step(&mut self, byte: u8, out: &mut Vec<u8>) {
        match self.state {
            State::Ground => {
                if byte == ESC {
                    self.state = State::Escape;
                    self.pending.push(byte);
                } else {
                    out.push(byte);
                }
            }

            State::Escape => {
                self.pending.push(byte);
                match byte {
                    b'[' => self.state = State::Csi,
                    b']' => self.state = State::Osc,
                    b'P' => self.state = State::Dcs,
                    // APC, PM, SOS: payloads, not conversation. Tracked so a `?` inside one is
                    // never read as a query, then handed over untouched.
                    b'_' | b'^' | b'X' => {
                        self.state = State::Opaque { esc: false };
                        out.append(&mut self.pending);
                    }
                    // A second ESC can only start a new sequence; the first was a lone ESC.
                    ESC => {
                        self.pending.pop();
                        out.append(&mut self.pending);
                        self.pending.push(ESC);
                    }
                    // Two-byte escape (`ESC =`, `ESC >`, `ESC 7`): nothing to decide.
                    _ => {
                        self.state = State::Ground;
                        out.append(&mut self.pending);
                    }
                }
            }

            State::Csi => {
                if byte == ESC {
                    // Whatever was pending was never a complete CSI.
                    out.append(&mut self.pending);
                    self.state = State::Escape;
                    self.pending.push(byte);
                    return;
                }
                self.pending.push(byte);
                match csi_verdict(self.direction, &self.pending) {
                    Verdict::Drop => {
                        self.state = State::Ground;
                        self.pending.clear();
                    }
                    Verdict::Undecided if self.pending.len() < MAX_SEQUENCE => {}
                    _ => {
                        self.state = State::Ground;
                        out.append(&mut self.pending);
                    }
                }
            }

            State::Osc => self.string_sequence(byte, out, osc_verdict),
            State::Dcs => self.string_sequence(byte, out, dcs_verdict),

            State::Opaque { esc } => {
                out.push(byte);
                self.state = match (esc, byte) {
                    (_, BEL) => State::Ground,
                    (true, b'\\') => State::Ground,
                    _ => State::Opaque { esc: byte == ESC },
                };
            }

            State::Swallow { esc } => {
                self.state = match (esc, byte) {
                    (_, BEL) => State::Ground,
                    (true, b'\\') => State::Ground,
                    _ => State::Swallow { esc: byte == ESC },
                };
            }
        }
    }

    /// One byte of an OSC or DCS, buffered until its shape is decidable.
    fn string_sequence(
        &mut self,
        byte: u8,
        out: &mut Vec<u8>,
        verdict: fn(Direction, &[u8], bool) -> Verdict,
    ) {
        // The terminator is BEL, or ST written as `ESC \`. A bare ESC is held back one byte so
        // `ESC \` is not split across the decision.
        let terminated = byte == BEL || (byte == b'\\' && self.pending.last() == Some(&ESC));
        self.pending.push(byte);

        match verdict(self.direction, &self.pending, terminated) {
            Verdict::Drop if terminated => {
                self.state = State::Ground;
                self.pending.clear();
            }
            Verdict::Drop => {
                self.state = State::Swallow { esc: byte == ESC };
                self.pending.clear();
            }
            Verdict::Undecided if !terminated && self.pending.len() < MAX_STRING_PREFIX => {}
            _ => {
                // Pass what is buffered, and the rest of the sequence with it.
                self.state = if terminated {
                    State::Ground
                } else {
                    State::Opaque { esc: byte == ESC }
                };
                out.append(&mut self.pending);
            }
        }
    }
}

enum Verdict {
    /// Not enough bytes yet.
    Undecided,
    /// Forward it verbatim.
    Pass,
    /// One half of a conversation this mirror is standing in for.
    Drop,
}

/// Classify a CSI, which is `ESC [` then parameters, then intermediates, then a final byte.
fn csi_verdict(direction: Direction, sequence: &[u8]) -> Verdict {
    let Some((&final_byte, prefix)) = sequence.get(2..).and_then(<[u8]>::split_last) else {
        return Verdict::Undecided;
    };
    match final_byte {
        // Still in the parameter/intermediate run.
        0x20..=0x3F => Verdict::Undecided,
        0x40..=0x7E => {
            let split = prefix
                .iter()
                .position(|byte| (0x20..=0x2F).contains(byte))
                .unwrap_or(prefix.len());
            let (params, intermediates) = prefix.split_at(split);
            if intermediates
                .iter()
                .any(|byte| !(0x20..=0x2F).contains(byte))
            {
                return Verdict::Pass;
            }
            let query = match direction {
                Direction::FromPane => is_csi_query(params, intermediates, final_byte),
                Direction::ToPane => is_csi_reply(params, intermediates, final_byte),
            };
            if query {
                Verdict::Drop
            } else {
                Verdict::Pass
            }
        }
        // Malformed: a control byte inside a CSI. Hand the whole run over untouched.
        _ => Verdict::Pass,
    }
}

/// A question the pane asked that rmux has already answered.
fn is_csi_query(params: &[u8], intermediates: &[u8], final_byte: u8) -> bool {
    match final_byte {
        // DSR 5, CPR, and DECXCPR.
        b'n' => intermediates.is_empty() && matches!(params, b"5" | b"6" | b"?6"),
        // Primary, secondary, and tertiary device attributes.
        b'c' => {
            intermediates.is_empty()
                && (params.is_empty()
                    || params == b"0"
                    || params.starts_with(b">")
                    || params.starts_with(b"="))
        }
        // XTVERSION. `ESC[1 q` sets the cursor style and shares the final byte, so its
        // intermediate is what tells them apart.
        b'q' => intermediates.is_empty() && params.starts_with(b">"),
        // DECRQM, private and ANSI forms. `ESC[!p` is a soft reset, not a query.
        b'p' => intermediates == b"$",
        // The kitty keyboard protocol's "what flags are set?" — `ESC[?u`. A key *event* in that
        // protocol is `ESC[<code>;<mods>u`, which has no `?` and must survive.
        b'u' => intermediates.is_empty() && params.starts_with(b"?"),
        // XTWINOPS, but only the members that ask for a report back.
        b't' => intermediates.is_empty() && is_winops_report(params),
        _ => false,
    }
}

/// An answer a viewer's terminal gave to a question it should never have seen.
fn is_csi_reply(params: &[u8], intermediates: &[u8], final_byte: u8) -> bool {
    match final_byte {
        // CPR/DECXCPR. No key sends CSI R — `ESC O R` is the SS3 form F3 uses.
        b'R' => intermediates.is_empty(),
        // Device attributes, and DSR status. Never keys.
        b'c' | b'n' => intermediates.is_empty(),
        // DECRPM, the answer to DECRQM.
        b'y' => intermediates == b"$",
        // XTWINOPS reports. `ESC[<params>t` is never a key either.
        b't' => intermediates.is_empty(),
        // Kitty keyboard flags report. Key events have no `?`, so they pass.
        b'u' => intermediates.is_empty() && params.starts_with(b"?"),
        _ => false,
    }
}

/// The XTWINOPS parameters that make the terminal report rather than act.
fn is_winops_report(params: &[u8]) -> bool {
    let head = params
        .split(|byte| *byte == b';')
        .next()
        .unwrap_or_default();
    matches!(
        head,
        b"11" | b"13" | b"14" | b"15" | b"16" | b"18" | b"19" | b"20" | b"21"
    )
}

/// Classify an OSC: `ESC ]` then a numeric parameter and a payload.
///
/// A query is one whose final `;`-separated field is a bare `?` — `10;?` for the foreground,
/// `4;1;?` for a palette entry, `52;c;?` for the clipboard. Everything else is a *set*, and
/// setting a title or a colour is content the viewers should see.
fn osc_verdict(direction: Direction, sequence: &[u8], terminated: bool) -> Verdict {
    // Nothing a viewer types is legitimately an OSC, so inbound the whole family goes.
    if direction == Direction::ToPane {
        return Verdict::Drop;
    }
    if !terminated {
        return Verdict::Undecided;
    }
    let payload = osc_payload(sequence);
    let last = payload
        .rsplit(|byte| *byte == b';')
        .next()
        .unwrap_or_default();
    if last == b"?" {
        Verdict::Drop
    } else {
        Verdict::Pass
    }
}

/// The bytes between `ESC ]` and the terminator.
fn osc_payload(sequence: &[u8]) -> &[u8] {
    let body = &sequence[2..];
    match body.last() {
        Some(&BEL) => &body[..body.len() - 1],
        // `ESC \`
        Some(b'\\') if body.len() >= 2 => &body[..body.len() - 2],
        _ => body,
    }
}

/// Classify a DCS: `ESC P` then parameters and intermediates, then a final byte and a payload.
///
/// Two of them are queries the terminal answers: XTGETTCAP (`+q`, "what does terminfo say about
/// this capability?") and DECRQSS (`$q`, "what is this setting right now?"). Everything else —
/// Sixel, ReGIS, DECUDK — is a payload and passes.
fn dcs_verdict(direction: Direction, sequence: &[u8], terminated: bool) -> Verdict {
    // Nothing a viewer types is legitimately a DCS either; its replies (`1+r…`, `1$r…`, and
    // DA3's `!|…`) all arrive this way.
    if direction == Direction::ToPane {
        return Verdict::Drop;
    }
    let body = &sequence[2..];
    // The introducer is at most a short parameter run then one intermediate and a final byte.
    let split = body
        .iter()
        .position(|byte| (0x20..=0x2F).contains(byte))
        .unwrap_or(body.len());
    let intermediates = &body[split..];
    match intermediates {
        [] if !terminated && body.len() < 8 => Verdict::Undecided,
        [b'+', b'q', ..] | [b'$', b'q', ..] => Verdict::Drop,
        [_] if !terminated => Verdict::Undecided,
        _ => Verdict::Pass,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filtered(chunks: &[&[u8]]) -> Vec<u8> {
        run(Direction::FromPane, chunks)
    }

    fn typed(chunks: &[&[u8]]) -> Vec<u8> {
        run(Direction::ToPane, chunks)
    }

    fn run(direction: Direction, chunks: &[&[u8]]) -> Vec<u8> {
        let mut filter = QueryFilter::for_direction(direction);
        let mut out = Vec::new();
        for chunk in chunks {
            out.extend_from_slice(&filter.push(chunk));
        }
        out
    }

    #[test]
    fn drops_the_csi_queries_rmux_answers() {
        for query in [
            b"\x1b[6n".as_slice(),
            b"\x1b[5n",
            b"\x1b[?6n",
            b"\x1b[c",
            b"\x1b[0c",
            b"\x1b[>c",
            b"\x1b[>0c",
            b"\x1b[=c",
            b"\x1b[>q",
            b"\x1b[>0q",
            b"\x1b[?2026$p",
            b"\x1b[4$p",
            // Kitty keyboard flags query, and window-size reports.
            b"\x1b[?u",
            b"\x1b[14t",
            b"\x1b[18t",
        ] {
            assert_eq!(
                filtered(&[b"a", query, b"b"]),
                b"ab",
                "should have dropped {:?}",
                String::from_utf8_lossy(query)
            );
        }
    }

    #[test]
    fn drops_the_string_queries_rmux_answers() {
        for query in [
            // OSC colour queries, BEL- and ST-terminated.
            b"\x1b]10;?\x07".as_slice(),
            b"\x1b]11;?\x1b\\",
            b"\x1b]4;1;?\x07",
            b"\x1b]12;?\x07",
            // Clipboard read: a question with an answer, so it is the mirror's to refuse.
            b"\x1b]52;c;?\x07",
            // XTGETTCAP and DECRQSS.
            b"\x1bP+q544e\x1b\\",
            b"\x1bP$qm\x1b\\",
        ] {
            assert_eq!(
                filtered(&[b"a", query, b"b"]),
                b"ab",
                "should have dropped {:?}",
                String::from_utf8_lossy(query)
            );
        }
    }

    #[test]
    fn passes_everything_else_through() {
        for sequence in [
            b"\x1b[0m".as_slice(),
            b"\x1b[38;5;196mred\x1b[0m",
            // Set cursor style: same final byte as XTVERSION, different shape.
            b"\x1b[1 q",
            // Soft reset: same final byte as DECRQM, different intermediate.
            b"\x1b[!p",
            // A kitty key event, which shares its final byte with the flags query.
            b"\x1b[97;5u",
            // Setting a colour or a title is content, not a question.
            b"\x1b]0;title\x07",
            b"\x1b]11;rgb:0d0d/0f0f/1212\x1b\\",
            b"\x1b=",
            b"\x1b[?1049h",
            // Resizing the window is an instruction, not a report request.
            b"\x1b[8;24;80t",
        ] {
            assert_eq!(
                filtered(&[sequence]),
                sequence,
                "should have passed {:?}",
                String::from_utf8_lossy(sequence)
            );
        }
    }

    /// Image and terminfo payloads are content. A mirror that swallowed them would break every
    /// terminal that can draw pictures.
    #[test]
    fn passes_payload_sequences_whole() {
        for payload in [
            // Kitty graphics (APC), with a `?` inside that must not read as a query.
            b"\x1b_Ga=T,f=100,q=?;iVBORw0KG\x1b\\".as_slice(),
            // Sixel (DCS), likewise.
            b"\x1bPq#0;2;0;0;0#1~~@@vv@@~~@@~~$\x1b\\",
            // DECUDK.
            b"\x1bP1;1|17/1b5b32347e\x1b\\",
            // PM and SOS.
            b"\x1b^private\x1b\\",
            b"\x1bXstring\x1b\\",
        ] {
            assert_eq!(
                filtered(&[payload]),
                payload,
                "should have passed {:?}",
                String::from_utf8_lossy(payload)
            );
        }
    }

    /// The reply storm: N viewers each answering one question, back through one PTY.
    #[test]
    fn drops_the_replies_viewers_send_back() {
        for reply in [
            b"\x1b[12;7R".as_slice(),
            b"\x1b[?1;2R",
            b"\x1b[0n",
            b"\x1b[?62;1;6c",
            b"\x1b[>0;276;0c",
            b"\x1b[?2026;2$y",
            b"\x1b[?1u",
            b"\x1b[8;24;80t",
            b"\x1b]11;rgb:0d0d/0f0f/1212\x1b\\",
            b"\x1bP1+r544e=787465726d\x1b\\",
            b"\x1bP!|00000000\x1b\\",
        ] {
            assert_eq!(
                typed(&[b"a", reply, b"b"]),
                b"ab",
                "should have dropped {:?}",
                String::from_utf8_lossy(reply)
            );
        }
    }

    /// Everything a person actually types has to survive the same filter.
    #[test]
    fn passes_the_keys_people_press() {
        for keys in [
            b"ls -la\r".as_slice(),
            // Arrows, plain and modified.
            b"\x1b[A",
            b"\x1b[1;5C",
            b"\x1bOB",
            // Function keys, Home/End, Delete.
            b"\x1bOP",
            b"\x1b[15~",
            b"\x1b[3~",
            // Bracketed paste.
            b"\x1b[200~pasted\x1b[201~",
            // Focus in/out.
            b"\x1b[I",
            b"\x1b[O",
            // SGR and X10 mouse reports.
            b"\x1b[<0;12;34M",
            b"\x1b[<0;12;34m",
            b"\x1b[M !!",
            // A bare ESC, which is a key in its own right.
            b"\x1b",
            // Kitty key events.
            b"\x1b[97;5u",
            // Ctrl-C.
            b"\x03",
        ] {
            assert_eq!(
                typed(&[keys]),
                keys,
                "should have passed {:?}",
                String::from_utf8_lossy(keys)
            );
        }
    }

    #[test]
    fn decides_a_query_split_at_every_byte_boundary() {
        for stream in [
            b"before\x1b[6nafter".as_slice(),
            b"before\x1b]10;?\x07after",
            b"before\x1bP+q544e\x1b\\after",
        ] {
            for split in 0..stream.len() {
                let (head, tail) = stream.split_at(split);
                assert_eq!(
                    filtered(&[head, tail]),
                    b"beforeafter",
                    "split at {} leaked {:?}",
                    split,
                    String::from_utf8_lossy(stream)
                );
            }
        }
    }

    #[test]
    fn decides_a_kept_sequence_split_at_every_byte_boundary() {
        for stream in [
            b"a\x1b[38;5;196mred".as_slice(),
            b"a\x1b]0;title\x07red",
            b"a\x1b_Gf=100;payload\x1b\\red",
        ] {
            for split in 0..stream.len() {
                let (head, tail) = stream.split_at(split);
                assert_eq!(
                    filtered(&[head, tail]),
                    stream,
                    "split at {} lost bytes from {:?}",
                    split,
                    String::from_utf8_lossy(stream)
                );
            }
        }
    }

    #[test]
    fn holds_only_the_incomplete_tail() {
        let mut filter = QueryFilter::new();
        assert_eq!(filter.push(b"text\x1b[6"), b"text");
        assert_eq!(filter.push(b"n more"), b" more");
    }

    #[test]
    fn a_second_escape_releases_the_first() {
        // A lone ESC is not a CSI, so it is passed on the moment the next sequence starts.
        assert_eq!(filtered(&[b"\x1b\x1b[6n\x1b[0m"]), b"\x1b\x1b[0m");
    }

    #[test]
    fn releases_a_run_that_never_terminates() {
        let runaway = [b"\x1b[".to_vec(), vec![b'1'; MAX_SEQUENCE]].concat();
        assert_eq!(
            filtered(&[&runaway]).len(),
            runaway.len(),
            "a runaway CSI must not be swallowed"
        );

        // A string sequence long enough to be a payload rather than a question is passed on the
        // strength of its length alone, without waiting for a terminator that may never come.
        let long = [b"\x1b]52;c;".to_vec(), vec![b'A'; MAX_STRING_PREFIX * 2]].concat();
        assert_eq!(
            filtered(&[&long]).len(),
            long.len(),
            "a long OSC payload must not be swallowed"
        );
    }

    #[test]
    fn reset_forgets_a_half_seen_sequence() {
        let mut filter = QueryFilter::new();
        assert_eq!(filter.push(b"\x1b[6"), b"");
        filter.reset();
        assert_eq!(filter.push(b"n"), b"n");
    }

    #[test]
    fn filters_text_for_the_input_path() {
        let mut filter = QueryFilter::inbound();
        assert_eq!(filter.push_text("echo héllo\x1b[12;7R\r"), "echo héllo\r");
    }
}
