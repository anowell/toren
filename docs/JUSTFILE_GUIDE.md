# Justfile Commands Reference

All Toren commands in one place using `just`.

## Quick Reference

```bash
just --list          # Show all available commands
```

## Setup & Build

```bash
just setup           # Full setup: install deps and build everything
just install         # Install dependencies only
just build           # Build Rust daemon
just build-ancillary # Build TypeScript ancillary
just build-all       # Build both daemon and ancillary
```

## Running Toren

```bash
just daemon          # Start the Toren daemon
just pair 714697     # Get session token (use pairing token from daemon)
just prompt "..."    # Send a prompt to Claude
just test-calculator # Run the calculator kata test
```

## Daemon Interaction

```bash
just health          # Check daemon health
just plugins         # List available plugins
just vcs-status      # Get VCS status for current directory
just vcs-status path # Get VCS status for specific path
```

## Examples

```bash
just run-calculator "3 + 2 * 4"     # Run the generated calculator
just test-calculator-output         # Test the generated calculator
```

## Maintenance

```bash
just fmt             # Format code (Rust and TypeScript)
just clean           # Clean build artifacts
just clean-examples  # Clean generated examples
```

## Complete Workflow

### First Time Setup

```bash
# 1. Install and build everything
just setup

# Output: ✅ Toren setup complete!
```

### Daily Usage

```bash
# Terminal 1: Start daemon
just daemon
# → Pairing token: 714697

# Terminal 2: Get session (first time only)
just pair 714697
# → Save the session_token

# Set environment
export ANTHROPIC_API_KEY=sk-ant-...
export SESSION_TOKEN=<from-pair-command>

# Build something!
just prompt "Build a CLI calculator with order of operations"

# Or from a file
just prompt < my-prompt.txt

# Or from stdin
echo "Create hello.js" | just prompt
```

## Tips

### Check Status
```bash
# Is daemon running?
just health

# What plugins are loaded?
just plugins

# What's the git/jj status?
just vcs-status
```

### Test the System
```bash
# Run the full calculator kata (proves it works)
just test-calculator

# This will:
# 1. Start a fresh daemon
# 2. Authenticate
# 3. Have Claude build a calculator
# 4. Verify all tests pass
# 5. Check git commit exists
```

### Multiple Prompts
```bash
# Prompt 1
just prompt "Create package.json"

# Prompt 2 (same session)
just prompt "Add Express server"

# Prompt 3
just prompt "Write tests for the server"
```

### Save Output
```bash
# Save just Claude's response
just prompt "..." 2>/dev/null > response.txt

# Save all output including progress
just prompt "..." 2>&1 | tee full-log.txt
```

## Environment Variables

These are read by the `prompt` command:

```bash
export ANTHROPIC_API_KEY=sk-ant-...  # Required
export SESSION_TOKEN=2a5f7066...     # Required (from just pair)
export WORKING_DIR=/path/to/project  # Optional (defaults to pwd)
export SHIP_URL=ws://localhost:8787  # Optional (defaults to localhost)
```

## Script Locations

The justfile wraps scripts in `scripts/`:

- `scripts/toren-cli.ts` - Main CLI (used by `just prompt`)
- `scripts/test-calculator-kata.ts` - Test suite (used by `just test-calculator`)

Don't run these directly - use the justfile commands.

## Common Issues

### "just: command not found"
Install just: https://github.com/casey/just#installation

macOS: `brew install just`

### "curl: Connection refused"
Daemon not running. Start it: `just daemon`

### "SESSION_TOKEN required"
Get a session token: `just pair <pairing-token>`

### "ANTHROPIC_API_KEY required"
Set your API key: `export ANTHROPIC_API_KEY=sk-ant-...`

## See Also

- `just --list` - List all commands
- `README.md` - Project overview
- `QUICK_START.md` - Getting started guide
- `HOW_TO_USE.md` - Simple usage guide
