# Changelog

## [Unreleased] - 2026-07-24

### Workspaces as places (breaking)
A workspace is now a **place** — a working copy + VCS state + rmux session + annotations — not an
"assignment" welded to a single task. Managing the place and updating the tracker are separated into
two orthogonal verb families, so shipping a piece of work and being done with the workspace are now
independent decisions; `breq list` shows when they've diverged.

- **`Assignment` / `AssignmentManager` removed**, along with the global `~/.toren/assignments.json`
  registry. The VCS enumerates workspaces; each carries its own git-excluded
  `<ws>/.toren/{annotations.json,cache.json}`. A per-incarnation `uid` (minted at setup) is embedded
  in rmux session names (`toren-<segment>-<ws>-<uid>`) and transcript paths.
- **Verbs changed**: `destroy` → `teardown` (pure deletion — no status changes, no push;
  `--no-delete` keeps the working copy). `assign` folded into `do` (`breq do <task>` claims the task
  and composes its context — the only tracker side effect in any place verb). New `get`/`set`
  annotation surface (`task.*` keys pass through to the tracker); `setup --from <ws>` stacks a child
  workspace; `breq setup <name>` adopts an existing working copy in place.
- **Workflow verbs are now shell scripts** dispatched git-style (`breq <name>` → `breq-<name>` on
  PATH / `~/.toren/bin`). `complete`, `abort`, and `submit` ship as editable defaults, installed by
  `breq init`. The Rhai `commands/` plugins and the `DeferredAction` protocol are **removed**.
- **Resolvers are a three-family plugin census** under `~/.toren/plugins/{tasks,agents,delivery}/`.
  Agents (claude/codex/gemini/opencode/pi) and delivery (github) are vendored and user-overridable;
  adding a tracker, agent, or forge is one `.rhai` file.
- **Intents removed** (`[intents]` config and `breq do -i`): prompt framing belongs to agent skills;
  task context already composes into the prompt via `do`.
- **Config**: `[intents]` removed; `[delivery] source` added; `[tasks]` takes `sources` (with the
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
- Every mirrored pane is recorded to `~/.toren/transcripts/<ancillary>/<assignment>.raw`, which the
  browser replays on attach so finished runs and post-restart sessions still show their history
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
