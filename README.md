# Toren

> *"I am Toren. I am continuity."*

Mobile-first distributed development intelligence - control Claude Code from any device.

## Quick Start

```bash
# Setup (one time)
just setup
cp .env.example .env  # Add your ANTHROPIC_API_KEY

# Run
just daemon           # Start daemon
just prompt examples/calculator  # Start building
```

## Usage

```bash
# Interactive mode
just prompt examples/myapp
# → Myapp One awaiting instructions:

# Piped mode
echo "Build a REST API" | just prompt examples/api
```

## Key Commands

```bash
just daemon          # Start the daemon
just prompt <dir>    # Send prompt to Claude in a segment
just health          # Check daemon status
just --list          # Show all commands
```

## Architecture

```
┌─────────────────┐
│  Your Device    │  Any device (phone, laptop, CLI)
└────────┬────────┘
         │ WebSocket
┌────────▼────────┐
│     TOREN       │  The Ship - persistent daemon
│  (Rust Daemon)  │  File system, commands, VCS
└────────┬────────┘
    ┌────▼────────────────┐
    │ Ancillaries         │  Claude sessions per segment
    └─────────────────────┘
```

## Configuration

All config in `.env`:
```bash
ANTHROPIC_API_KEY=sk-ant-...  # Required
PAIRING_TOKEN=123456          # Optional (defaults to random)
```

## Documentation

- [docs/CONCEPTS.md](docs/CONCEPTS.md) - Naming and metaphor
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) - Technical design
- [docs/SEGMENTS.md](docs/SEGMENTS.md) - Segment configuration
- [docs/JUSTFILE_GUIDE.md](docs/JUSTFILE_GUIDE.md) - Command reference

## Status

**Core: ~40% complete**

Working: Daemon, tool calling, VCS, CLI, sessions
Not yet: Android app, diff workflow, multi-ancillary

## License

MIT
