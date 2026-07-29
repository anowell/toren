# Changelog

## [Unreleased] - 2026-07-28

### The terminal mirrors a pane instead of attaching a multiplexer
rmux is now used **only as a server**. Nothing runs `rmux attach`: `breq do` and `breq sh` render
one pane in the terminal they were run from — bytes out, keystrokes in, `SIGWINCH` to a resize —
which is the same mirror the browser reads, from the same shared crate.

- **No multiplexer chrome.** No prefix key, no status bar, no window list, and `exit` returns you to
  the shell you came from with the pane's exit code. Closing the terminal leaves the pane running in
  rmux; there is no detach chord, because every key belongs to the pane.
- **Panes hold or close by how they were made** (not by anything inferred afterwards): a shell
  closes when you exit it, an agent or a `breq sh <ws> -- <cmd>` command holds. A held pane grows a
  line — `[exited 3 — <ENTER> re-run, <ESC> drop to shell, <Ctrl-c> close]` — and answers those
  keys; on an agent pane `<ENTER>` *resumes* the session that ran there. `breq sh --hold` /
  `--no-hold` override it in either direction, and `--no-hold` keeps `breq sh <ws> -- <cmd>` usable
  in a pipeline, as does anything without a terminal.
- **`breq sh <ws> --window <name>`** mirrors an existing window of the workspace's session, which is
  how a running agent (`--window agent`) is watched from a terminal.

### The web terminal is kept, and hardened
The browser reads the same mirror the local terminal does — nothing about the streaming path was
rebuilt — but it now recovers instead of asking you to reload.

- **Resync is a repaint.** A client that fell behind used to be told "reload to resync" while the
  daemon streamed on into a corrupted terminal; it now gets a paint of the pane's screen. Every
  frame carries an epoch, and both ends discard anything from before the paint — which is what makes
  that a fix rather than a race with the frames already in flight.
- **Backpressure is counted in bytes**, not in chunks: a chunk is anywhere between a keystroke's echo
  and a megabyte, so "512 chunks behind" measured nothing. A client more than 256 KiB behind is
  repainted rather than sent everything it missed.
- **Keepalive in both directions.** The daemon pings idle sockets and hangs up on a client gone
  silent; the browser sends its own ping as JSON, since its API cannot send a protocol one. A pane
  can go hours without a byte, so nothing else told a quiet agent apart from a connection a proxy or
  a sleeping phone had dropped.
- **Held panes render and act in the browser**: the exit line is drawn into the pane's bytes, so it
  is the same line, and its three keys work there too — with buttons beside them, and a one-click
  dismissal in the window list, since every resume leaves another held pane behind.
- **Starting an agent names it.** "New agent" becomes one action per configured agent, and "Resume
  Previous Session" lists what the workspace recorded — each resume opening a new pane on an old
  session.

### The workspace page is rebuilt around the terminal
Vertical position is scope — app bar, sidebar, ancillary bar, facts, panes, terminal — and the
terminal gets every pixel the bars do not need. The daemon side of that lands first:

- **`GET /api/agents` reports what is installed.** Each agent comes back as
  `{name, installed, default}`, where `installed` is whether the binary the agent's own plugin says
  it launches resolves on the daemon's PATH — so the browser stops offering "New codex agent" on a
  machine without codex. An agent whose plugin cannot say what it launches is listed anyway, since a
  misconfigured agent should fail loudly rather than vanish.
- **A workspace view carries its rmux `session`**, so session identity and attach commands are read
  from the daemon rather than rebuilt in the browser out of segment, name and uid.
- **`POST /api/workspaces/:segment/:name/workflow`** runs the same `breq-complete` / `breq-abort`
  scripts the CLI does, in a held `cmd` pane where the output is read like any other command's — no
  streaming over HTTP. The body is `{"verb": "complete" | "abort"}`, an enum rather than a command
  line: nothing else can be spawned through it.
- **The input row is gone.** Typing happens in the terminal, which takes keystrokes directly; a
  textarea that retyped them into it was a second, worse keyboard. The app bar's interrupt button
  goes with it — `Ctrl-C` reaches the pane, and interrupting is pane scope, not app scope — and the
  workspaces toggle it used to sit beside moves up into the app bar on a phone.
- **Every control sits in the bar whose scope it affects.** `Complete` and `Abort` are workspace
  lifecycle, so they are on the workspace's own bar, each confirming by naming the script it runs
  and then selecting the pane it runs in. `+ Shell` and `+ Agent` make panes, so they are on the
  panes bar, together, with resume folded into the agent menu and only installed agents listed.
  Stopping an agent is on that agent's chip, where `agent_activity` is also shown.
- **The terminal's bottom line is no longer clipped.** The fit measured the box the grid is drawn
  in *plus* its padding — `getComputedStyle().height` reports the border box for a
  `box-sizing: border-box` element — so the terminal was told it had room for a row that then hung
  below the box and was cut off by its `overflow: hidden`. Padding now lives on a frame outside the
  measured element, the grid is re-fit whenever that element resizes or the font finishes loading,
  and the pixels left over below the last whole row stay slack.
- **Banners are for exceptions only.** "Attached to `<session>` — the same pane a local mirror
  shows" is deleted (its content belongs on a chip, not in a sentence), and "Attaching…" appears
  only when attaching takes longer than 300ms. Errors and held panes still announce themselves.
- **What is true of a workspace is a strip of chips.** Task, changes, pull requests, `▣ N runs` and
  the rmux session each show a glyph and a count on one line, and clicking gives the fastest useful
  dive: a popover of rows, or — for a single PR, which has one obvious destination — its url. This
  replaces the sets summary and the panel that unfolded below it, so no detail is a standing panel
  taking height from the terminal; an empty fact does not render at all, except the task chip, whose
  dimmed "no task" is itself a fact. The session chip is where the deleted banner's content went:
  the full session name and the `breq sh <ws>` / `breq sh <ws> --window <w>` commands to attach a
  terminal of your own, copyable — including from a phone on plain http, where the browser hands out
  no clipboard — and still never an `rmux attach`.

### State gets a schema (breaking)
Every file toren persists now carries a `version` as its first key and is written atomically, so a
crash mid-write cannot truncate the `uid` that names a live rmux session.

- **`<ws>/.toren/state.json` replaces `annotations.json`.** The flat key/value store becomes a
  structured document: `agent` and `tasks` are unpacked out of packed strings (`"claude:opus"`,
  `"runes:tor-1"`), `base` records which VCS its revision belongs to, `delivery` gets a real home,
  the write-only `name`/`segment` keys are dropped, and `prompt` is stored beside the mutable
  `title`. Keys `breq set` invents live under `extra`. Migration is automatic on first read; a file
  from a newer breq is refused rather than misparsed, and never overwritten.
- **Agent sessions are recorded per workspace** — id, agent, start/end, exit status, and the agent's
  own title, snapshotted. `breq do --resume` takes the most recent, `--resume <sessionId>` takes the
  one you name. The workspace title resolves down a chain: linked task, then the last agent
  session's title, then the prompt.
- **`~/.toren/config.kdl` replaces `config.toml`**, consolidating on the language `toren.kdl`
  already used per repo. An old file is converted on first load — a copy, leaving the original
  readable — and every setting the current config does not carry is reported rather than swallowed.
  Unknown KDL nodes warn. `config.kdl.example` is checked against the real struct by a test.
- **`~/.toren/completion_history.jsonl` is retired** in favour of a rotating JSON `tracing` log under
  `~/.toren/logs/`, written by both `breq` and the daemon. `breq doctor` reports the old file.
- **Transcripts are gone** (`~/.toren/transcripts/`, `breq cleanup --transcripts`): agents keep their
  own session records, shells keep history, and rmux keeps scrollback. What toren records instead is
  which agent session ran in which workspace incarnation.
- **`cache.json` is write-through**: any command already making a live call refreshes the cached copy
  on its way past, and each entry's age is rendered so a stale value reads as stale. `breq list`
  reads the cache and writes nothing, even with `--refresh`.
- **`breq doctor --fix` finishes the move off `~/.toren/assignments.json`.** An active record whose
  workspace is undecorated now decorates it the way `breq setup` does — adoption, hooks and all —
  then links `<task_source>:<task_id>`, writes the recorded title as the workspace's stored one, and
  pays for the tracker read so `breq list` shows a title rather than an id. A tracker that cannot be
  reached keeps the link and warns. One line per workspace either way; the registry file is dropped
  only once every record it holds has somewhere else to live.

## [Unreleased] - 2026-07-24

### Workspaces as places (breaking)
A workspace is now a **place** — a working copy + VCS state + rmux session + stored metadata — not an
"assignment" welded to a single task. Managing the place and updating the tracker are separated into
two orthogonal verb families, so shipping a piece of work and being done with the workspace are now
independent decisions; `breq list` shows when they've diverged.

- **`Assignment` / `AssignmentManager` removed**, along with the global `~/.toren/assignments.json`
  registry. The VCS enumerates workspaces; each carries its own git-excluded
  `<ws>/.toren/{state.json,cache.json}`. A per-incarnation `uid` (minted at setup) is embedded
  in rmux session names (`toren-<segment>-<ws>-<uid>`).
- **Verbs changed**: `destroy` is now pure deletion — no status changes, no push;
  `--no-delete` keeps the working copy. `assign` folded into `do` (`breq do <task>` claims the task
  and composes its context — the only tracker side effect in any place verb). New `get`/`set` state
  surface (`task.*` keys pass through to the tracker); `setup --from <ws>` stacks a child
  workspace; `breq setup <name>` adopts an existing working copy in place.
- **Workflow verbs are now shell scripts** dispatched git-style (`breq <name>` → `breq-<name>` on
  PATH / `~/.toren/bin`). `complete`, `abort`, and `submit` ship as editable defaults, installed by
  `breq init`. The Rhai `commands/` plugins and the `DeferredAction` protocol are **removed**.
- **Resolvers are a three-family plugin census** under `~/.toren/plugins/{tasks,agents,delivery}/`.
  Agents (claude/codex/gemini/opencode/pi) and delivery (github) are vendored and user-overridable;
  adding a tracker, agent, or forge is one `.rhai` file.
- **Intents removed** (the `intents` config and `breq do -i`): prompt framing belongs to agent
  skills; task context already composes into the prompt via `do`.
- **Config**: `intents` removed; `delivery { source }` added; `tasks` takes `sources` (with the
  old `default_source` string still accepted).
- **`breq doctor`** gains a migration that moves a legacy `~/.toren/assignments.json` into each
  workspace's `.toren/` on `--fix`. Nothing migrates implicitly.

## [Unreleased] - 2026-07-21

### Agents run in rmux sessions
- Coding agents now run inside [rmux](https://rmux.io/) sessions named `toren-<segment>-<workspace>`,
  so `breq do` and the web UI attach to the *same* agent process rather than each spawning their own
- `breq do` execs `rmux attach`; detaching leaves the agent running. `--no-rmux` restores the old
  direct-exec behaviour, which is also the automatic fallback when rmux isn't installed
- `breq shell <ws>` selects the session's shell window instead of spawning a standalone subprocess
- The web `[unit]` route renders an xterm.js terminal over `/ws/ancillaries/:id`, which now carries
  raw pane bytes both ways instead of typed chat events
- Attaching a browser to a pane replays what rmux has retained for it, then switches to the live
  stream, so a finished run still shows what it did
- `breq do` refuses to replace an agent already running in the workspace unless given `--force`;
  `breq complete`/`abort` refuse to tear down a session with live work unless given `--kill`
- Removed the embedded Claude Agent SDK runtime, the per-turn `WorkLog`, and the `ancillary/`
  TypeScript package
- See [docs/terminals.md](docs/terminals.md) for the session layout and zellij interop

## [2026-01-07] - Multi-Ancillary

### Multi-Ancillary Support
- Multiple concurrent Claude sessions with independent tokens
- Real-time status tracking (Connected, Executing, Idle)
- `GET /api/ancillaries/list` endpoint

### Segments & Web Interface
- TOML-based segment discovery (`toren.toml`)
- Mobile-first web interface (SvelteKit)
- Segment selector with touch-friendly UI

### Interactive CLI
- `just prompt <dir>` with interactive mode
- Auto-pairing with stored credentials
- Session persistence across restarts

## [2026-01-06] - Justfile Commands
- Moved all commands to justfile
- `just setup`, `just daemon`, `just prompt`, etc.
- Scripts organized in `scripts/`

## [2026-01-05] - Calculator Kata Success
- End-to-end test: Claude built working calculator (31 tests passing)
- Tool calling, authentication, command streaming all working

## [2026-01-04] - Initial Implementation
- Rust daemon with WebSocket + REST API
- TypeScript ancillary runtime
- VCS integration (Git + Jujutsu)
- Plugin system with YAML definitions
