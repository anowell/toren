# Toren

> *"I am Toren. I am continuity."*

Toren orchestrates AI-assisted development by managing:
- **Agent sessions** - Claude Code sessions with persistent context
- **Issues** - Beads for task tracking and handoff
- **Isolated workspaces** - jj workspaces with per-workspace config, hooks, and copy-on-write content

## Interfaces

Toren provides multiple ways to interact with this functionality:

- **Breq** - CLI using Claude's TUI for interactive sessions
- **Daemon API** - REST + WebSocket interface using Claude Code SDK
- **Web** - Browser-based interface connecting to the daemon
- **Mobile** - Coming soon (tunnels to the daemon)

## Installation

```bash
# Install the CLI and daemon
cargo install --path breq
cargo install --path daemon
```

## Configuration

Create `toren.toml` in your project root (or `~/.config/toren/config.toml` for global config):

```toml
[server]
host = "127.0.0.1"
port = 8787

[segments]
# Directories containing your projects
roots = ["~/projects"]

[ancillary]
workspace_root = "~/.toren/workspaces"
pool_size = 10
```

### Claude Authentication

The daemon spawns Claude Code sessions, which require authentication. Either:

```bash
# Option A: Anthropic API key
export ANTHROPIC_API_KEY=<your-key>

# Option B: Claude Max/Pro subscription (OAuth token via `claude setup-token`)
export CLAUDE_CODE_OAUTH_TOKEN=<token>
```

## Breq Usage

```bash
# Start the daemon (optional - for web UI and remote access)
toren-daemon

# Assign work to Claude
breq assign <bead-id>              # Assign a bead task
breq assign --prompt "Build X"    # Quick task from prompt

# Manage assignments
breq list                          # Show active assignments
breq resume <ref>                  # Continue work on assignment
breq approve <ref>                 # Accept completed work
breq abort <ref>                   # Discard and cleanup

# Navigate to workspaces
breq go <workspace>                # Spawn shell in workspace
breq go <workspace> -- <cmd>       # Run command in workspace
```

## Web Interface

The web UI provides mobile-friendly access to your Toren sessions. Note: significant functionality is still missing.

```bash
cd web && pnpm install && pnpm dev
# Open http://localhost:5173
```

## Workspace Hooks

Toren uses `.toren.kdl` to customize setup and teardown of workspaces. Generate a starter config with:

```bash
breq init             # Auto-discovers common patterns (.beads, node_modules, target, etc.)
breq init --stealth   # Same, but adds .toren.kdl to .git/info/exclude
```

### Manual Configuration

Add a `.toren.kdl` file to your repo root:

```kdl
setup {
    template src=".env.breq" dest=".env"
    copy src="config.example.json" dest="config.json"
    copy src="node_modules"
    share src=".beads"
    run "pnpm install"
    run "just migrate"
}

destroy {
    run "just destroy-db"
}
```

**Actions:**
- `template src="..." dest="..."` - Copy and render with workspace context
- `copy src="..." [dest="..."]` - Copy file/directory using CoW when available
- `share src="..."` - Symlink to shared content (e.g., `.beads` directory)
- `run "command"` - Execute shell command

For `copy`, `dest` defaults to `src` (or basename if `src` is absolute).

**Template variables:**
```
{{ ws.name }}    # Workspace name (e.g., "one", "two")
{{ ws.num }}     # Ancillary number (1, 2, etc.)
{{ ws.path }}    # Full workspace path
{{ repo.root }}  # Repository root path
{{ repo.name }}  # Repository name
```

Example `.env.breq` template:
```env
PORT={{ 5173 + ws.num }}
DATABASE_URL=postgres://localhost/myapp_{{ ws.name }}
```

## Documentation

- [docs/CONCEPTS.md](docs/CONCEPTS.md) - Naming and metaphor
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) - Technical design
- [docs/SEGMENTS.md](docs/SEGMENTS.md) - Segment configuration

