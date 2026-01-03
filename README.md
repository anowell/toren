# Toren

> *"I am Toren. I am continuity."*

**Mobile-First Distributed Development Intelligence** - Control Claude Code from any device.

---

## Quick Start

```bash
# 1. Setup (one time)
just setup
cp .env.example .env
# Edit .env: Add your ANTHROPIC_API_KEY

# 2. Start daemon (keep running)
just daemon

# 3. Build something!
just prompt examples/calculator
# → Calculator One awaiting instructions:
# Type your prompt, press Enter twice
```

**That's it!** Two commands and you're building apps.

---

## Usage

### Interactive Mode
```bash
just prompt examples/myapp
# → Myapp One awaiting instructions:
# Type your prompt, press Enter twice when done
```

### Piped Mode
```bash
echo "Build a REST API for tasks with CRUD operations" | just prompt examples/api

# Multi-line
cat <<EOF | just prompt examples/webapp
Build an Express web server:
- GET / returns "Hello World"
- GET /api/time returns current time as JSON
- Include tests
EOF

# From file
just prompt examples/myapp < prompt.txt
```

### What It Does
- ✅ Auto-pairs on first use (uses `PAIRING_TOKEN` from `.env`)
- ✅ Creates project directory if it doesn't exist
- ✅ Derives ancillary name from path (`examples/todo` → "Todo One")
- ✅ Sessions persist across daemon restarts (`~/.toren/sessions.json`)

---

## Key Commands

```bash
just setup           # First-time setup
just daemon          # Start the daemon
just prompt <dir>    # Interactive mode or pipe your prompt
just test-calculator # Run end-to-end test (proves it works!)
just health          # Check daemon status
just --list          # Show all commands
```

---

## How It Works

```
┌─────────────────┐
│  Your Device    │  (Android/CLI/Future: iOS)
│  (Interface)    │  Just the viewport - use any device
└────────┬────────┘
         │ WebSocket
         │
┌────────▼────────┐
│     TOREN       │  The Ship - Persistent Intelligence
│  (Rust Daemon)  │  - File system access
│                 │  - Command execution
│                 │  - VCS integration (Git/Jujutsu)
│                 │  - Session/ancillary management
└────────┬────────┘
         │
    ┌────▼────────────────┐
    │ Ancillaries         │  Claude sessions working on segments
    │ (Claude Sessions)   │  "Calculator One", "Todo Two", etc.
    └─────────────────────┘
```

**The Concept**: Toren is your persistent development intelligence. The daemon runs on your dev machine. You control it from any device - laptop, phone, tablet. Ancillaries (Claude sessions) do the thinking and work on segments (your projects). Your sessions persist. Your continuity follows you.

---

## Configuration

All config in `.env` file:

```bash
ANTHROPIC_API_KEY=sk-ant-...   # Required: Your API key
PAIRING_TOKEN=123456           # Optional: Fixed token for testing (defaults to random)
SESSION_TOKEN=...              # Optional: Reuse existing session (auto-generated)
SHIP_URL=ws://localhost:8787   # Optional: Ship location (defaults to localhost)
```

---

## Examples

```bash
# Calculator (proven working - 31 tests passing)
just prompt examples/calculator
# Type: Build a CLI calculator with +, -, *, / and order of operations

# Todo app
echo "Build a todo CLI app with SQLite, CRUD operations, and tests" | just prompt examples/todo

# Web API
cat api-spec.txt | just prompt examples/api

# Test the system works
just test-calculator
```

---

## Documentation

- **[CHANGELOG.md](CHANGELOG.md)** - What's new
- **[docs/CALCULATOR_KATA_RESULTS.md](docs/CALCULATOR_KATA_RESULTS.md)** - Proof it works (31 passing tests)
- **[docs/JUSTFILE_GUIDE.md](docs/JUSTFILE_GUIDE.md)** - Complete command reference
- **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** - Technical design
- **[docs/TOREN.md](docs/TOREN.md)** - Why "Toren"? (Ancillary Justice inspired)

---

## Status

**Core Intelligence: Online** (~40%)

✅ **Working:**
- Rust daemon with WebSocket + REST API
- Tool calling (read_file, write_file, execute_command, list_directory)
- VCS integration (Git + Jujutsu)
- Plugin system (25+ commands)
- CLI interface with auto-pairing
- Session persistence
- **Proven:** Calculator kata - 31 tests passing

❌ **Not Yet:**
- Android mobile interface (main deliverable - the device viewport)
- Diff generation/application
- Command approval workflow
- Multi-ancillary support (multiple Claude sessions simultaneously)

---

## The Name

"Toren" is inspired by Ann Leckie's *Ancillary Justice* series. The ship "Justice of Toren" has many ancillaries (bodies expressing its consciousness). Here:

- **Toren** = The daemon (persistent ship intelligence)
- **Ancillaries** = Claude sessions working on segments ("Calculator One", "Todo Two")
- **Segments** = Your projects/directories
- **Devices** = Just interfaces (phone, laptop, CLI) - viewports into the continuity

**Devices are interchangeable. Ancillaries persist. Toren endures.**

---

## Technology

- **Daemon (Toren):** Rust, Tokio, Axum, WebSocket
- **Ancillary Runtime:** TypeScript, Anthropic SDK (Claude sessions)
- **Mobile Interface:** Kotlin, Jetpack Compose (future)
- **VCS:** Git, Jujutsu
- **Protocol:** JSON over WebSocket

---

## License

MIT

---

**Last Updated:** 2026-01-07
**Status:** Core online, mobile interface in development
