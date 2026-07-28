//! Strips the terminal queries rmux answers on the pane's behalf.
//!
//! rmux replies to CPR, DA1/DA2/DA3, XTVERSION, DSR and DECRQM itself *and* forwards the query on
//! to every subscriber. A passthrough client's terminal answers it too, so each attached mirror
//! adds one duplicate reply: junk in the shell buffer (`zsh: command not found: 1R`) and protocol
//! desync in anything that parses the answer it gets back. Seeding replays historical queries as
//! well, so a fresh attach fires a burst of them.
//!
//! Dropping the queries before any client sees them fixes every surface at once, and costs
//! nothing: rmux has already answered, so nobody is waiting on a reply that never comes.

const ESC: u8 = 0x1B;

/// Longest escape sequence held while deciding. Past this a run is malformed, and withholding it
/// would be worse than passing it through.
const MAX_SEQUENCE: usize = 32;

/// A streaming filter over one pane's output.
///
/// Chunk boundaries fall wherever the daemon put them, including inside an escape sequence, so a
/// partially seen sequence is held until the next chunk decides it.
#[derive(Debug, Default)]
pub struct QueryFilter {
    pending: Vec<u8>,
}

impl QueryFilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of pane output; returns what clients should see.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pending.len() + chunk.len());
        for &byte in chunk {
            if self.pending.is_empty() {
                if byte == ESC {
                    self.pending.push(byte);
                } else {
                    out.push(byte);
                }
                continue;
            }

            if byte == ESC {
                // Whatever was pending was never a query, and this byte can only start a new one.
                out.append(&mut self.pending);
                self.pending.push(byte);
                continue;
            }

            self.pending.push(byte);
            match verdict(&self.pending) {
                Verdict::Query => self.pending.clear(),
                Verdict::Undecided if self.pending.len() < MAX_SEQUENCE => {}
                _ => out.append(&mut self.pending),
            }
        }
        out
    }

    /// Forget a half-seen sequence, for a stream that just skipped bytes.
    pub fn reset(&mut self) {
        self.pending.clear();
    }
}

enum Verdict {
    /// Not enough bytes yet.
    Undecided,
    /// Forward it verbatim.
    Pass,
    /// One of the queries rmux has already answered.
    Query,
}

/// Classify a run starting at `ESC`.
fn verdict(sequence: &[u8]) -> Verdict {
    let Some(&introducer) = sequence.get(1) else {
        return Verdict::Undecided;
    };
    // Only CSI carries the queries rmux answers; everything else passes without inspection.
    if introducer != b'[' {
        return Verdict::Pass;
    }
    let Some((&final_byte, prefix)) = sequence[2..].split_last() else {
        return Verdict::Undecided;
    };
    match final_byte {
        0x20..=0x3F => Verdict::Undecided,
        0x40..=0x7E if is_query(prefix, final_byte) => Verdict::Query,
        0x40..=0x7E => Verdict::Pass,
        // Malformed: a control byte inside a CSI. Hand the whole run over untouched.
        _ => Verdict::Pass,
    }
}

fn is_query(prefix: &[u8], final_byte: u8) -> bool {
    let split = prefix
        .iter()
        .position(|b| (0x20..=0x2F).contains(b))
        .unwrap_or(prefix.len());
    let (params, intermediates) = prefix.split_at(split);
    if intermediates.iter().any(|b| !(0x20..=0x2F).contains(b)) {
        return false;
    }

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
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filtered(chunks: &[&[u8]]) -> Vec<u8> {
        let mut filter = QueryFilter::new();
        let mut out = Vec::new();
        for chunk in chunks {
            out.extend_from_slice(&filter.push(chunk));
        }
        out
    }

    #[test]
    fn drops_the_queries_rmux_answers() {
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
            // Replies, which flow the other way and must never be mistaken for queries.
            b"\x1b[12;7R",
            b"\x1b[?62;c",
            b"\x1b[?2026;2$y",
            b"\x1b]0;title\x07",
            b"\x1b=",
            b"\x1b[?1049h",
        ] {
            assert_eq!(filtered(&[sequence]), sequence);
        }
    }

    #[test]
    fn decides_a_query_split_at_every_byte_boundary() {
        let stream = b"before\x1b[6nafter";
        for split in 0..stream.len() {
            let (head, tail) = stream.split_at(split);
            assert_eq!(
                filtered(&[head, tail]),
                b"beforeafter",
                "split at {} leaked the query",
                split
            );
        }
    }

    #[test]
    fn decides_a_kept_sequence_split_at_every_byte_boundary() {
        let stream = b"a\x1b[38;5;196mred";
        for split in 0..stream.len() {
            let (head, tail) = stream.split_at(split);
            assert_eq!(
                filtered(&[head, tail]),
                stream,
                "split at {} lost bytes",
                split
            );
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
        let out = filtered(&[&runaway]);
        assert_eq!(
            out.len(),
            runaway.len(),
            "a runaway sequence must not be swallowed"
        );
    }

    #[test]
    fn reset_forgets_a_half_seen_sequence() {
        let mut filter = QueryFilter::new();
        assert_eq!(filter.push(b"\x1b[6"), b"");
        filter.reset();
        assert_eq!(filter.push(b"n"), b"n");
    }
}
