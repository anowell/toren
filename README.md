# Toren

> *"I am Toren. I am continuity."*

Toren is a set of composable tools to orchestrate workspaces for agentic development.

- Supports git worktrees and jj workspaces
- Configurable workspace setup and destruction (isolate and/or share components between workspaces)
- Per-workspace local domains (i.e. upstream reverse proxying through local domains with Caddy)
- Agent sessions: currently Claude Code


## Introduction

- **Breq** - CLI using Claude's TUI for interactive sessions
- **Toren Daemon** - REST + WebSocket workspace API (using Claude Code SDK)
- **Station** - Manage reverse proxy configuration (e.g. proxies per workspace)
- **Web** - Browser-based interface connecting to the daemon
- **Mobile** - Coming soon (tunnels to the daemon)

Given a repo named "snake":

- Breq (CLI) or Toren (API) issue commands to ancillaries
- A single ancillary (e.g. Snake One) manages
  - a single workspace ("@one") or worktree (branch "one")
  - one or more agents (e.g. claude)
- Services in the workspace become accessible via one.snake.lvh.me (or other local-resolving domain)


## Installation

```bash
cargo install --git https://github.com/anowell/toren breq
```

## Getting Started

```bash
cd ~/projects/myapp
breq init
```

This does two things:
1. Creates `.toren.kdl` in your repo with auto-discovered workspace hooks (copying `node_modules`, sharing `.beads`, etc.)
2. Offers to register your project directory in `~/.toren/config.toml` so breq can find it

Then start a Claude session:

```bash
breq cmd -p "Add input validation to the signup form"
```

Breq creates a workspace (git worktree or jj workspace), runs your setup hooks, and launches Claude Code with your prompt. Each ancillary gets a named workspace ("one", "two", etc.).

## Breq CLI

```bash
# Start a session with a prompt
breq cmd -p <prompt>              # Launch Claude in a new/reused workspace
breq cmd -i <intent>              # Use a configured prompt template
bd show proj-123 | breq cmd       # Prompt from stdin

# Manage active sessions
breq list                          # Show active assignments
breq clean <workspace>             # Teardown workspace and cleanup

# Work in a workspace directly
breq run <workspace>               # Spawn shell in workspace
breq run <workspace> -- <cmd>      # Run command in workspace
```

## Workspace Hooks (.toren.kdl)

The `.toren.kdl` file in your repo root configures workspace setup and teardown:

```kdl
vars {
    web_port expr="30000 + {{ ws.num }}"
}

setup {
    copy src="node_modules"
    share src=".beads"
    run "pnpm install"
    proxy http upstream.vars="vars.web_port"
}

destroy {
    run "just destroy-db"
}
```

**Actions:**
- `copy src="..."` - Copy file/directory using CoW when available
- `share src="..."` - Symlink to shared content
- `template src="..." dest="..."` - Copy and render with workspace template variables
- `run "command"` - Execute shell command
- `proxy` - Register a reverse proxy route via [Station](station/README.md)

**Template variables:** `{{ ws.name }}`, `{{ ws.num }}`, `{{ ws.path }}`, `{{ repo.root }}`, `{{ repo.name }}`

## More

- [Configuration](docs/configuration.md) - Global config, proxy, intents, and aliases
- [Toren Daemon](daemon/README.md) - REST + WebSocket API for programmatic workspace and agent management
- [Station](station/README.md) - Reverse proxy management for per-workspace local domains
- [docs/CONCEPTS.md](docs/CONCEPTS.md) - Naming and metaphor
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) - Technical design

