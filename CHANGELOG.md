# Changelog

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
