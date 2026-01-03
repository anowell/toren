# Changelog

## [Unreleased] - 2026-01-07

### Multi-Ancillary Support

**Implemented concurrent ancillary sessions:**
- ✅ Multiple independent Claude sessions with separate session tokens
- ✅ Each ancillary registers with unique ID and segment name
- ✅ Real-time status tracking (Connected, Executing, Idle, Disconnected)
- ✅ New API endpoint: `GET /api/ancillaries/list` to query all ancillaries
- ✅ Proper lifecycle management (register on connect, unregister on disconnect)
- ✅ Status transitions during execution

**Architecture:**
- Created `daemon/src/ancillary.rs` with `AncillaryManager` for tracking
- Modified WebSocket auth to accept `ancillary_id` and `segment` parameters
- Updated status when commands execute (Connected → Executing → Idle)
- Added timestamp tracking for connection and last activity

**Proven with test:**
- Two concurrent sessions (Calculator One + Fizzbuzz One)
- Both executed directives simultaneously
- Both operated in separate segments (working directories)
- Files created successfully in respective segments
- Status correctly tracked throughout lifecycle

**Example API response:**
```json
{
  "ancillaries": [
    {
      "id": "Calculator One",
      "segment": "Calculator",
      "session_token": "3R4Qx7AyyH...",
      "status": "connected",
      "connected_at": "2026-01-07T06:11:05.360203+00:00"
    },
    {
      "id": "Fizzbuzz One",
      "segment": "Fizzbuzz",
      "session_token": "XFKbr4Jrc8...",
      "status": "executing",
      "connected_at": "2026-01-07T06:11:06.979668+00:00"
    }
  ],
  "count": 2
}
```

### Naming Clarification

**Clarified the metaphor:**
- **Ancillary** = Claude session working on a segment (e.g., "Calculator One")
- **Segment** = Working directory/project (e.g., examples/calculator)
- **Device/Interface** = Phone, laptop, CLI - just viewports (no special metaphor name)
- **Toren/Ship** = The daemon providing persistent capabilities

**Previously confused:** Devices were sometimes called "ancillaries" in docs
**Now clear:** Ancillaries are the Claude sessions doing the thinking/work, not the devices

**Updated:**
- README.md - Clarified "How It Works" diagram and "The Name" section
- docs/TOREN.md - Fixed all references to match correct metaphor
- docs/NAMING.md - Already correct (no changes needed)

### Documentation Cleanup

**Reorganized:**
- Streamlined README.md to essentials only (quick start, usage, examples)
- Moved technical docs to `docs/`:
  - ARCHITECTURE.md
  - CALCULATOR_KATA_RESULTS.md
  - JUSTFILE_GUIDE.md
  - TOREN.md (naming explanation)
  - NAMING.md (conventions)
- Created docs/README.md as guide to documentation
- Moved example-prompt.txt → examples/hello-world-prompt.txt

**Removed redundant/outdated:**
- BUILD_SUMMARY.md (outdated)
- HOW_TO_USE.md (redundant with README)
- IMPLEMENTATION.md (outdated)
- MANUAL_API_USAGE.md (not needed for basic usage)
- PROJECT_STRUCTURE.md (redundant)
- QUICK_START.md (redundant with README)
- README_CLI.md (redundant)
- start.md (original vision, outdated)
- STATUS.md (outdated)
- SUMMARY.md (outdated)
- WORKFLOW_SIMPLIFIED.md (redundant)
- test_client.sh (replaced by `just health` and `just pair`)
- android/ (empty placeholder directory)

**Result:** Clean root with just README.md and CHANGELOG.md. All technical docs in docs/.

### Interactive Mode, Auto-Pairing & Session Persistence

**Major UX Improvements:**
- **Interactive mode**: `just prompt examples/calculator` prompts for input if stdin not provided
  - Shows: "🤖 Calculator One awaiting instructions:"
  - Multi-line input supported (press Enter twice to submit)
- **Auto-pairing**: `just prompt` automatically pairs if SESSION_TOKEN not set
- **Dotenv loading**: `.env` file loaded automatically by justfile
- **Directory creation**: `just prompt examples/foo` creates dir if it doesn't exist
- Session tokens persist across daemon restarts in `~/.toren/sessions.json`
- Support for `PAIRING_TOKEN` environment variable
- Changed `just prompt` to accept directory argument instead of prompt text
- Prompts can come from stdin (piped) or interactive mode
- Automatic ancillary naming based on directory (e.g., `examples/todo` → "Todo One")

**Before (5 steps):**
```bash
just daemon  # Note random token: 714697
just pair 714697
export ANTHROPIC_API_KEY=sk-ant-...
export SESSION_TOKEN=<token>
just prompt "Build something"
```

**After (2 steps):**
```bash
just daemon  # Uses PAIRING_TOKEN from .env
echo "Build something" | just prompt examples/foo
# → 🔐 Auto-pairing with PAIRING_TOKEN...
# → ✅ Paired successfully!
# → Creates examples/foo if needed
```

**Workflow now:**
1. Create `.env` with `ANTHROPIC_API_KEY` and `PAIRING_TOKEN=123456`
2. `just daemon`
3. `echo "Build X" | just prompt path/to/project`
4. Done!

## [2026-01-06] - Major Reorganization: Justfile-based Commands

**What Changed:**
- Moved all scripts to `scripts/` directory
- Created comprehensive `justfile` for all commands
- Removed cluttered shell scripts from root
- Cleaner, more organized project structure

**Before:**
```bash
./toren "..."
./toren-cli.ts "..."
npx tsx test-calculator-kata.ts
./target/release/toren-daemon
```

**After:**
```bash
just prompt "..."
just test-calculator
just daemon
just --list
```

### Added

#### Commands
- `just setup` - Full first-time setup
- `just daemon` - Start the daemon
- `just pair <token>` - Get session token
- `just prompt "..."` - Send prompt to Claude
- `just test-calculator` - Run calculator kata
- `just health` - Check daemon status
- `just plugins` - List available plugins
- `just vcs-status` - Get VCS status
- `just build`, `just build-all`, etc. - Build commands
- `just clean`, `just clean-examples` - Cleanup commands

#### Documentation
- `JUSTFILE_GUIDE.md` - Complete justfile reference
- `PROJECT_STRUCTURE.md` - Codebase layout guide
- `HOW_TO_USE.md` - Simplest usage guide
- Updated all docs to use `just` commands

#### Structure
- `scripts/` directory for internal scripts
- `scripts/README.md` explaining not to run directly
- Updated `.gitignore` for cleaner repo

### Changed
- All documentation now references `just` commands
- Simplified README with justfile emphasis
- Better organized docs section in README
- Cleaner command invocation patterns

### Removed
- `toren` wrapper script (replaced by `just prompt`)
- Root-level `toren-cli.ts` (moved to `scripts/`)
- Root-level `test-calculator-kata.ts` (moved to `scripts/`)

### Benefits

1. **Single Command Interface** - Everything through `just`
2. **Self-Documenting** - `just --list` shows all commands
3. **No Clutter** - Scripts hidden in `scripts/`
4. **Easier Discovery** - Clear, memorable command names
5. **Better Organization** - Logical grouping of functionality

---

## [2026-01-05] - Calculator Kata Success

### Added
- Complete working calculator built by Claude
- 31 comprehensive tests, all passing
- End-to-end test suite proving the system works
- Tool calling support in AncillaryRuntime
- Proper authentication flow

### Proven
- ✅ Daemon API works end-to-end
- ✅ Claude can build complete applications
- ✅ Authentication flow is solid
- ✅ Tool execution works correctly
- ✅ VCS integration functional

---

## [2026-01-04] - Initial Toren Implementation

### Added
- Rust daemon with WebSocket + REST API
- TypeScript ancillary runtime
- VCS integration (Git + Jujutsu)
- Plugin system with YAML definitions
- Security with token-based authentication
- Segment/ancillary naming system
- Comprehensive documentation

### Components Built
- Filesystem service
- Command execution service
- VCS service
- Plugin manager
- WebSocket API
- REST endpoints

---

## See Also

- `SUMMARY.md` - Complete project overview
- `STATUS.md` - Current implementation status
- `CALCULATOR_KATA_RESULTS.md` - Proof of working system
