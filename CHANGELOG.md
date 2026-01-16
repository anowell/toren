# Changelog

## [Unreleased] - 2026-01-07

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
