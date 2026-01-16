# Justfile Commands

All Toren commands via `just`. Run `just --list` for full list.

## Essential Commands

```bash
just setup           # First-time setup
just daemon          # Start daemon
just prompt <dir>    # Send prompt to Claude
just health          # Check daemon status
```

## Build Commands

```bash
just build           # Build Rust daemon
just build-ancillary # Build TypeScript ancillary
just build-all       # Build everything
```

## Testing

```bash
just test-calculator # Run end-to-end calculator test
```

## Maintenance

```bash
just fmt             # Format code
just clean           # Clean build artifacts
```

## Workflow

```bash
# Terminal 1
just daemon

# Terminal 2
just prompt examples/calculator
# → Calculator One awaiting instructions:
# Type prompt, press Enter twice
```

## Environment Variables

Set in `.env`:
```bash
ANTHROPIC_API_KEY=sk-ant-...  # Required
PAIRING_TOKEN=123456          # Optional
```

Or export directly:
```bash
export ANTHROPIC_API_KEY=sk-ant-...
export SESSION_TOKEN=...  # From auto-pairing
```
