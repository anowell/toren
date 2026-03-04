> _I had once had twenty bodies, twenty pairs of eyes, and hundreds of others that I could access if I needed or desired it._ -Breq, Justice of Toren ([Ancillary Justice](https://goodreads.com/book/show/17333324-ancillary-justice))

# Toren

Toren is a set of composable tools to orchestrate workspaces for agentic development.

- Manage work in git worktrees or jj workspaces
- Easily spawn agents (Claude, Codex, Gemini, or OpenCode) in workspaces

Built-in support for:

- Configurable workspace setup and destruction (isolate and/or share components between workspaces)
- Per-workspace local domains (i.e. reverse proxying via Caddy)

Bring your own work-tracking system (e.g. Linear, GH Issues, beads, etc). 

## Introduction

- **Breq** - CLI for assigning work and managing agent sessions
- **Toren Daemon** - REST + WebSocket workspace API (experimental)
- **Station** - Manage reverse proxy configuration (e.g. proxies per workspace)
- **Web** - Browser-based interface connecting to the daemon (experimental)
- **Mobile** - Coming soon (tunnels to the daemon)

**Mental Model**:

- Breq (CLI) or Toren (API) assign work to ancillaries
- A single ancillary (e.g. App One) manages
  - a single workspace ("@one") or worktree (branch "one")
  - one or more agents (e.g. claude)
- Services in the workspace become accessible via one.app.lvh.me (or other local-resolving domain)


## Installation

```bash
cargo install --git https://github.com/anowell/toren breq
```

## Getting Started

```bash
cd ~/projects/app

# Initialize toren.kdl - git ignored by .git/info/exclude
breq init --stealth
```

This does two things:
1. Creates `toren.kdl` in your repo with auto-discovered workspace hooks (e.g. copying `node_modules`, sharing `.beads`, etc.)
2. Ensures the repo is registered as an ancillary in `~/.toren/config.toml`

Then start an agent session:

```bash
breq do -p "Add input validation to the signup form"
```

Breq creates a workspace (git worktree or jj workspace), runs your setup hooks, and launches Claude Code with your prompt. Each ancillary gets a named workspace ("one", "two", etc.).

## Breq CLI

```bash
# Assign work to a coding agent
breq do -p <prompt>                # Launch agent in a new workspace
breq do <workspace> -p <prompt>    # Launch agent in an existing workspace
breq do -i <intent>                # Use a configured prompt template
bd show proj-123 | breq do         # Prompt from stdin

# Manage active sessions
breq list                          # Show active assignments
breq clean <workspace>             # Teardown workspace and cleanup

# Work in a workspace directly
breq shell <workspace>             # Open shell in workspace
breq shell <workspace> -- <cmd>    # Run command in workspace
```

The plugin system makes it trivial to integrate these primitives with any work-tracking workflow. `contrib/` contains example plugins
that can be copied to `~/.toren/plugins/` and trivially modified to your workflow: 

```bash
breq assign <task_id>              # Runs `breq do` with --prompt, --task-id, and --task-title
breq complete <ws>                 # Runs `breq clean` and closes the bead associated with the workspace
```


## Workspace Hooks (toren.kdl)

The `toren.kdl` file in your repo root configures workspace setup and teardown:

```kdl
vars {
    web_port expr="30000 + {{ ws.num }}"
}

setup {
    // Copy-on-write into workspace
    copy src="node_modules"
    // Symlink into workspace
    share src=".beads"
    // Execute arbitrary workspace setup commands:w
    run "pnpm install"

    // Configure reverse proxy from `{{ws.name}}.{{repo.name}}.lvh.me` to your web_port
    // Short for: run "station proxy {{ws.name}} --port 80 --upstream {{vars.web_port}}"
    proxy "http" upstream="{{vars.web_port}}"
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
- `proxy` - Register a reverse proxy route via [Station](station/README.md) - basically a shorthand for `run "station proxy {{ws.name}} --port <port> --upstream <upstream>"`

All string arguments support `{{ ... }}` template variables.

**Template variables:** `{{ ws.name }}`, `{{ ws.num }}`, `{{ ws.path }}`, `{{ repo.root }}`, `{{ repo.name }}`, `{{ task.id }}`, `{{ task.title }}`, `{{ vars.<name> }}`

## More

- [Configuration](docs/configuration.md) - Global config, proxy, intents, and aliases
- [Toren Daemon](daemon/README.md) - REST + WebSocket API for programmatic workspace and agent management
- [Station](station/README.md) - Reverse proxy management for per-workspace local domains
- [docs/CONCEPTS.md](docs/CONCEPTS.md) - Naming and metaphor
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) - Technical design

