//! Measurements the mirror's architecture rests on, against a live rmux.
//!
//! Every claim the mirror is built on is a claim about *this* machine's rmux, so each one is
//! asked rather than assumed:
//!
//! * `caps` — how many output subscriptions one connection carries before rmux refuses, and
//!   whether a second connection has a budget of its own. Decides whether the fix for "live pane
//!   shown as exited" is more connections or fewer mirrors.
//! * `lifecycle` — whether a push stream reports a *held* pane's process dying, which is what a
//!   liveness poll would otherwise have to keep asking about.
//! * `ownership` — whether a pane can carry a note saying which viewer is sizing it, somewhere
//!   both `breq` and the daemon can read.
//! * `fidelity` — what a server-side `snapshot()` preserves that a screen paint does not, and
//!   what it loses. Decides whether browser viewers can be moved off raw bytes.
//! * `cost` — what a snapshot costs per update, per pane, against the raw byte stream it would
//!   replace, at the pane counts the goals call for.
//!
//! Run: `just rmux-spike <mode> [panes]`. Every session it makes is named `spike<pid>-*` and
//! killed on the way out.

use anyhow::{anyhow, Context, Result};
use rmux_sdk::{
    EnsureSession, PaneCell, PaneColor, PaneId, PaneOutputChunk, PaneOutputStart, PaneSnapshot,
    Rmux, SessionName, TerminalSizeSpec,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Long enough for a pane to be running and to have drawn something, short enough to wait for.
const SETTLE: Duration = Duration::from_millis(400);

/// How long the cost run watches a firehose. Long enough that a 16ms debounce has hundreds of
/// windows to coalesce, short enough to run repeatedly.
const COST_WINDOW: Duration = Duration::from_secs(5);

/// The subscription hunt stops here rather than pinning a machine that has no cap at all.
const MAX_SUBSCRIPTION_PROBES: usize = 128;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let what = args.next().unwrap_or_else(|| "caps".to_string());
    let panes: usize = args.next().and_then(|arg| arg.parse().ok()).unwrap_or(20);

    match what.as_str() {
        "caps" => caps().await,
        "fidelity" => fidelity().await,
        "cost" => cost(panes).await,
        "lifecycle" => lifecycle().await,
        "ownership" => ownership().await,
        other => Err(anyhow!(
            "unknown spike '{}'; try caps, fidelity, cost, or lifecycle",
            other
        )),
    }
}

/// Two viewers of one pane resize it to two sizes and the last write wins, which is how a
/// terminal ends up drawing a screen laid out for a browser. Arbitrating that needs somewhere to
/// record who the writer is — and it has to be somewhere *both* processes can see, because `breq`
/// and the daemon mirror the same pane from different processes. A pane-local user option is the
/// only such place; this checks rmux keeps one.
async fn ownership() -> Result<()> {
    let rmux = connect().await?;
    let session = scratch_session(&rmux, "ownership", "cat").await?;
    let pane_id = first_pane(&rmux, &session).await?;
    let pane = rmux
        .session(session.clone())
        .await?
        .pane_by_id(pane_id)
        .await?;

    println!("== size ownership ==");
    println!(
        "  unset            {:?}",
        pane.option("@toren-size-owner").await?
    );
    pane.set_option("@toren-size-owner", "breq:1234").await?;
    let mine = pane.option("@toren-size-owner").await?;
    println!("  after set        {:?}", mine);

    // A second connection stands in for the other process.
    let other = connect().await?;
    let seen = other
        .session(session.clone())
        .await?
        .pane_by_id(pane_id)
        .await?
        .option("@toren-size-owner")
        .await?;
    println!("  seen from a second connection {:?}", seen);

    pane.unset_option("@toren-size-owner").await?;
    println!(
        "  after unset      {:?}",
        pane.option("@toren-size-owner").await?
    );

    println!();
    println!(
        "verdict: {}",
        if mine.as_deref() == Some("breq:1234") && seen.as_deref() == Some("breq:1234") {
            "a pane carries a size-owner note both processes can read"
        } else {
            "pane options do not carry a cross-process owner — arbitrate in the daemon only"
        }
    );

    kill(&rmux, &session).await;
    Ok(())
}

/// A held pane outlives its process, so nothing about the pane's *existence* changes when it
/// exits. If `state_events` reports that anyway, the 2s liveness fork-poll has a push replacement;
/// if it does not, held panes still need asking.
async fn lifecycle() -> Result<()> {
    let rmux = connect().await?;
    let session = scratch_session(&rmux, "lifecycle", "sleep 2; exit 7").await?;
    let pane_id = first_pane(&rmux, &session).await?;
    let pane = rmux
        .session(session.clone())
        .await?
        .pane_by_id(pane_id)
        .await?;
    // What `breq do` sets on an agent window: the pane outlives the process it was running.
    pane.set_option("remain-on-exit", "on").await?;

    println!("== lifecycle push ==");
    let mut events = pane
        .state_events(rmux_sdk::PaneStateEventsOptions::default())
        .await?;

    let started = Instant::now();
    let terminal = loop {
        if started.elapsed() > Duration::from_secs(15) {
            break None;
        }
        match events.next().await? {
            Some(rmux_sdk::PaneStateEvent::Closed { reason, .. }) => break Some(reason),
            Some(other) => println!(
                "  {:>6.2}s  {}",
                started.elapsed().as_secs_f64(),
                name(&other)
            ),
            None => break None,
        }
    };

    match terminal {
        Some(reason) => println!(
            "  {:>6.2}s  Closed({:?}) — push liveness covers a held pane",
            started.elapsed().as_secs_f64(),
            reason
        ),
        None => println!("  no Closed event within 15s — held panes still need polling"),
    }

    // Whatever the stream said, the exit status itself has to come from somewhere.
    let info = pane.info().await?;
    println!(
        "  info() after exit: process {:?}, exit {:?}",
        info.pane(pane_id).map(|pane| pane.process.clone()),
        info.pane(pane_id).and_then(|pane| pane.exit_state.clone())
    );

    kill(&rmux, &session).await;
    Ok(())
}

fn name(event: &rmux_sdk::PaneStateEvent) -> &'static str {
    match event {
        rmux_sdk::PaneStateEvent::Snapshot { .. } => "Snapshot",
        rmux_sdk::PaneStateEvent::TitleChanged { .. } => "TitleChanged",
        rmux_sdk::PaneStateEvent::OptionSet { .. } => "OptionSet",
        rmux_sdk::PaneStateEvent::OptionUnset { .. } => "OptionUnset",
        rmux_sdk::PaneStateEvent::ForegroundChanged { .. } => "ForegroundChanged",
        rmux_sdk::PaneStateEvent::Lagged { .. } => "Lagged",
        rmux_sdk::PaneStateEvent::Closed { .. } => "Closed",
        _ => "unknown",
    }
}

async fn caps() -> Result<()> {
    let rmux = connect().await?;
    let session = scratch_session(&rmux, "caps", "while :; do sleep 1; done").await?;
    let pane_id = first_pane(&rmux, &session).await?;

    println!("== subscription cap ==");

    // One pane, one connection: how many times can the same pane be subscribed?
    let same_pane = probe_subscriptions(&rmux, &session, &[pane_id]).await?;
    report("one pane, one connection", &same_pane);

    // Distinct panes, one connection: separates a per-pane cap from a per-connection one.
    let mut ids = vec![pane_id];
    for index in 0..MAX_SUBSCRIPTION_PROBES.min(24) {
        let window = rmux
            .session(session.clone())
            .await?
            .new_window_with()
            .name(format!("w{}", index))
            .shell("while :; do sleep 1; done")
            .await?;
        if let Some(pane) = window.panes().await?.first() {
            ids.push(pane.id);
        }
    }
    tokio::time::sleep(SETTLE).await;
    let many_panes = probe_subscriptions(&rmux, &session, &ids).await?;
    report("many panes, one connection", &many_panes);

    // The same probe on a connection of its own. If this succeeds after the first ran out, the
    // budget is per-connection and the fix for exhaustion is more connections.
    let second = connect().await?;
    let fresh = probe_subscriptions(&second, &session, &ids).await?;
    report("many panes, a second connection", &fresh);

    println!();
    println!(
        "verdict: {}",
        if fresh.opened >= many_panes.opened.min(2) && many_panes.limit.is_some() {
            "per-connection budget — a mirror per connection dissolves the cap"
        } else if many_panes.limit.is_none() {
            "no cap reached within the probe range on this daemon"
        } else {
            "a second connection did not help — the cap is not per-connection"
        }
    );

    kill(&rmux, &session).await;
    Ok(())
}

struct Probe {
    opened: usize,
    limit: Option<String>,
}

/// Open output subscriptions on `rmux` round-robin over `panes` until one is refused.
///
/// The streams are held in a vec for the duration: dropping one releases its slot, which is the
/// thing being counted.
async fn probe_subscriptions(
    rmux: &Rmux,
    session: &SessionName,
    panes: &[PaneId],
) -> Result<Probe> {
    let mut held = Vec::new();
    for index in 0..MAX_SUBSCRIPTION_PROBES {
        let pane_id = panes[index % panes.len()];
        let pane = rmux
            .session(session.clone())
            .await?
            .pane_by_id(pane_id)
            .await?;
        match pane.output_stream_starting_at(PaneOutputStart::Now).await {
            Ok(stream) => held.push(stream),
            Err(e) => {
                return Ok(Probe {
                    opened: held.len(),
                    limit: Some(format!("{}", e)),
                })
            }
        }
    }
    Ok(Probe {
        opened: held.len(),
        limit: None,
    })
}

fn report(what: &str, probe: &Probe) {
    match &probe.limit {
        Some(error) => println!("  {:<34} {} opened, then: {}", what, probe.opened, error),
        None => println!(
            "  {:<34} {} opened, no refusal within {} probes",
            what, probe.opened, MAX_SUBSCRIPTION_PROBES
        ),
    }
}

/// Everything a browser viewer would have to survive losing, drawn into one pane.
const FIDELITY_PAYLOAD: &str = concat!(
    // Scrollback: 40 numbered lines, so a 24-row pane keeps only the tail on screen.
    "for i in $(seq 1 40); do echo \"scrollback-line-$i\"; done; ",
    // Colour, in all three encodings.
    "printf '\\033[31mansi-red\\033[0m \\033[38;5;208mindexed-208\\033[0m ",
    "\\033[38;2;10;200;30mtruecolor\\033[0m\\n'; ",
    // Underline variants, italic, strikethrough, and an underline colour.
    "printf '\\033[4msingle\\033[0m \\033[4:3mcurly\\033[0m ",
    "\\033[58;5;93m\\033[4:2mdouble-coloured\\033[0m \\033[3mitalic\\033[0m ",
    "\\033[9mstrike\\033[0m\\n'; ",
    // Wide glyphs and a combining mark.
    "printf '\\346\\227\\245\\346\\234\\254\\350\\252\\236 emoji-\\360\\237\\224\\245 combining-e\\314\\201\\n'; ",
    // A hyperlink (OSC 8) and a title (OSC 0) — out-of-band state a cell grid has no room for.
    "printf '\\033]8;;https://example.invalid\\033\\\\linked\\033]8;;\\033\\\\\\n'; ",
    "printf '\\033]0;spike-title\\007'; ",
    "cat",
);

async fn fidelity() -> Result<()> {
    let rmux = connect().await?;
    let session = scratch_session(&rmux, "fidelity", FIDELITY_PAYLOAD).await?;
    let pane_id = first_pane(&rmux, &session).await?;
    let pane = rmux
        .session(session.clone())
        .await?
        .pane_by_id(pane_id)
        .await?;
    pane.resize(TerminalSizeSpec::new(100, 24)).await?;
    tokio::time::sleep(SETTLE * 3).await;

    let snapshot = pane.snapshot().await?;
    let captured = pane.capture_pane().escape_ansi(true).await?;
    let escaped = String::from_utf8_lossy(&captured.stdout).into_owned();
    let rendered = render_rows(&snapshot);

    println!("== snapshot fidelity ==");
    println!(
        "  grid                 {}x{}, {} cells, revision {}",
        snapshot.cols,
        snapshot.rows,
        snapshot.cells.len(),
        snapshot.revision
    );
    println!(
        "  cursor               row {} col {} visible {} style {}",
        snapshot.cursor.row, snapshot.cursor.col, snapshot.cursor.visible, snapshot.cursor.style
    );

    check(
        "visible text",
        rendered.iter().any(|row| row.contains("truecolor")),
        "the last screen's text is in the grid",
    );
    check(
        "scrollback",
        rendered
            .iter()
            .any(|row| row.contains("scrollback-line-1 ")),
        "lines scrolled off the visible grid are NOT in a snapshot",
    );
    check(
        "wide glyphs",
        snapshot.cells.iter().any(PaneCell::is_padding),
        "wide-glyph padding cells are preserved",
    );
    check(
        "truecolor",
        snapshot
            .cells
            .iter()
            .any(|cell| matches!(cell.foreground, PaneColor::Rgb { .. })),
        "RGB foregrounds survive as RGB",
    );
    check(
        "indexed colour",
        snapshot
            .cells
            .iter()
            .any(|cell| matches!(cell.foreground, PaneColor::Indexed { .. })),
        "256-colour indices survive",
    );
    check(
        "underline styles",
        snapshot.cells.iter().any(|cell| {
            cell.attributes
                .contains(rmux_sdk::PaneAttributes::CURLY_UNDERLINE)
        }),
        "curly underline survives as its own bit",
    );
    check(
        "underline colour",
        snapshot
            .cells
            .iter()
            .any(|cell| !matches!(cell.underline, PaneColor::Default)),
        "a separate underline colour survives",
    );
    check(
        "hyperlinks",
        rendered.iter().any(|row| row.contains("example.invalid")),
        "OSC 8 targets are NOT in a cell grid (only the label is)",
    );
    check(
        "alt screen / modes",
        false,
        "a snapshot carries no mode flags: alt-screen, bracketed paste, mouse mode are absent",
    );

    println!();
    println!("  == what the seed path (capture-pane -e) keeps by comparison ==");
    check(
        "  -e truecolor",
        escaped.contains("38;2;10;200;30"),
        "RGB survives the escaped capture",
    );
    check(
        "  -e indexed",
        escaped.contains("38;5;208"),
        "256-colour survives the escaped capture",
    );
    check(
        "  -e curly underline",
        escaped.contains("4:3"),
        "underline style survives the escaped capture",
    );
    check(
        "  -e underline colour",
        escaped.contains("58;"),
        "underline colour survives the escaped capture",
    );

    println!();
    println!(
        "  capture-pane -e is {} bytes for the same screen",
        escaped.len()
    );
    println!(
        "  snapshot is ~{} bytes of cell payload for the same screen",
        cell_payload_bytes(&snapshot)
    );

    kill(&rmux, &session).await;
    Ok(())
}

fn check(what: &str, holds: bool, note: &str) {
    println!(
        "  {:<20} {}  {}",
        what,
        if holds { "yes" } else { "no " },
        note
    );
}

fn render_rows(snapshot: &PaneSnapshot) -> Vec<String> {
    (0..snapshot.rows)
        .map(|row| {
            (0..snapshot.cols)
                .filter_map(|col| snapshot.cell(row, col))
                .filter(|cell| !cell.is_padding())
                .map(|cell| cell.text())
                .collect::<String>()
        })
        .collect()
}

/// What one snapshot would weigh on the wire, counting only the per-cell payload a delta encoder
/// could not avoid sending for a full repaint.
fn cell_payload_bytes(snapshot: &PaneSnapshot) -> usize {
    snapshot
        .cells
        .iter()
        .map(|cell| {
            cell.glyph.text.len()
                + 2 // attribute bits
                + 3 * 4 // three colours, generously encoded
        })
        .sum()
}

/// A pane that never stops printing, which is the case the lag budget exists for.
const FIREHOSE: &str =
    "while :; do echo \"the quick brown fox jumps over the lazy dog $RANDOM\"; done";

async fn cost(panes: usize) -> Result<()> {
    println!("== cost at {} panes ==", panes);

    let raw = measure_raw(panes).await?;
    let snap = measure_snapshots(panes).await?;

    println!();
    println!(
        "  raw output_stream     {:>8} chunks  {:>10} bytes  {:>6.1} ms/poll",
        raw.updates, raw.bytes, raw.per_update_ms
    );
    println!(
        "  render_stream         {:>8} updates {:>10} bytes  {:>6.1} ms/update",
        snap.updates, snap.bytes, snap.per_update_ms
    );
    if raw.bytes > 0 {
        println!(
            "  snapshot path costs   {:.1}x the bytes of the raw path",
            snap.bytes as f64 / raw.bytes as f64
        );
    }
    println!();
    println!("  note: render_stream is output_stream + snapshot() (rmux-sdk events/render.rs),");
    println!("        so it consumes an output subscription per pane on top of its snapshot cost.");
    Ok(())
}

struct Measured {
    updates: u64,
    bytes: u64,
    per_update_ms: f64,
}

async fn measure_raw(panes: usize) -> Result<Measured> {
    let rmux = connect().await?;
    let session = firehose_session(&rmux, "raw", panes).await?;
    let ids = pane_ids(&rmux, &session).await?;

    let started = Instant::now();
    let mut tasks = Vec::new();
    for pane_id in ids {
        let pane = rmux
            .session(session.clone())
            .await?
            .pane_by_id(pane_id)
            .await?;
        tasks.push(tokio::spawn(async move {
            let mut stream = match pane.output_stream_starting_at(PaneOutputStart::Now).await {
                Ok(stream) => stream,
                Err(_) => return (0u64, 0u64),
            };
            let (mut updates, mut bytes) = (0u64, 0u64);
            while started.elapsed() < COST_WINDOW {
                let Ok(chunks) = stream.poll_once().await else {
                    break;
                };
                for chunk in chunks {
                    if let PaneOutputChunk::Bytes { bytes: got, .. } = chunk {
                        updates += 1;
                        bytes += got.len() as u64;
                    }
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            (updates, bytes)
        }));
    }
    let measured = collect(tasks, started).await;
    kill(&rmux, &session).await;
    Ok(measured)
}

async fn measure_snapshots(panes: usize) -> Result<Measured> {
    let rmux = connect().await?;
    let session = firehose_session(&rmux, "snap", panes).await?;
    let ids = pane_ids(&rmux, &session).await?;

    let started = Instant::now();
    let mut tasks = Vec::new();
    for pane_id in ids {
        // One connection per pane, so the subscription cap does not silently truncate the run.
        let client = connect().await?;
        let session = session.clone();
        tasks.push(tokio::spawn(async move {
            let Ok(handle) = client.session(session).await else {
                return (0u64, 0u64);
            };
            let Ok(pane) = handle.pane_by_id(pane_id).await else {
                return (0u64, 0u64);
            };
            let Ok(mut stream) = pane.render_stream().await else {
                return (0u64, 0u64);
            };
            let (mut updates, mut bytes) = (0u64, 0u64);
            while started.elapsed() < COST_WINDOW {
                match stream.next().await {
                    Ok(Some(update)) => {
                        updates += 1;
                        bytes += cell_payload_bytes(update.snapshot()) as u64;
                    }
                    _ => break,
                }
            }
            (updates, bytes)
        }));
    }
    let measured = collect(tasks, started).await;
    kill(&rmux, &session).await;
    Ok(measured)
}

async fn collect(tasks: Vec<tokio::task::JoinHandle<(u64, u64)>>, started: Instant) -> Measured {
    let (mut updates, mut bytes) = (0u64, 0u64);
    for task in tasks {
        if let Ok((got_updates, got_bytes)) = task.await {
            updates += got_updates;
            bytes += got_bytes;
        }
    }
    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
    Measured {
        updates,
        bytes,
        per_update_ms: if updates == 0 {
            0.0
        } else {
            elapsed / updates as f64
        },
    }
}

async fn connect() -> Result<Arc<Rmux>> {
    Rmux::builder()
        .default_timeout(Duration::from_secs(10))
        .connect_or_start()
        .await
        .map(Arc::new)
        .context("rmux daemon unreachable — is `rmux` on PATH?")
}

async fn scratch_session(rmux: &Rmux, what: &str, command: &str) -> Result<SessionName> {
    let name = SessionName::new(format!("spike{}-{}", std::process::id(), what))
        .map_err(|e| anyhow!("{}", e))?;
    rmux.ensure_session(
        EnsureSession::named(name.clone())
            .create_or_reuse()
            .detached(true)
            .size(TerminalSizeSpec::new(100, 24))
            .shell(command.to_string()),
    )
    .await?;
    tokio::time::sleep(SETTLE).await;
    Ok(name)
}

async fn firehose_session(rmux: &Rmux, what: &str, panes: usize) -> Result<SessionName> {
    let name = scratch_session(rmux, what, FIREHOSE).await?;
    let session = rmux.session(name.clone()).await?;
    for index in 1..panes {
        session
            .new_window_with()
            .name(format!("w{}", index))
            .shell(FIREHOSE)
            .await?;
    }
    tokio::time::sleep(SETTLE * 2).await;
    Ok(name)
}

async fn pane_ids(rmux: &Rmux, session: &SessionName) -> Result<Vec<PaneId>> {
    Ok(rmux
        .find_panes()
        .session(session.as_str())
        .all()
        .await?
        .into_iter()
        .map(|found| found.pane_id)
        .collect())
}

async fn first_pane(rmux: &Rmux, session: &SessionName) -> Result<PaneId> {
    pane_ids(rmux, session)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("session '{}' has no panes", session))
}

/// Best-effort teardown: a spike that panics should not leave a firehose running.
async fn kill(rmux: &Rmux, session: &SessionName) {
    if let Ok(handle) = rmux.session(session.clone()).await {
        let _ = handle.kill().await;
    }
}
