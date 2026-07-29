> _I had once had twenty bodies, twenty pairs of eyes, and hundreds of others that I could access if I needed or desired it._ -Breq, Justice of Toren ([Ancillary Justice](https://goodreads.com/book/show/17333324-ancillary-justice))

# Toren

Toren is a set of composable tools to orchestrate workspaces for agentic development.

- Manage work in git worktrees or jj workspaces
- Easily spawn agents (Claude, Codex, Gemini, OpenCode, or Pi) in workspaces

Built-in support for:

- Configurable workspace setup and destruction (isolate and/or share components between workspaces)
- Per-workspace local domains (i.e. reverse proxying via Caddy)

Bring your own work-tracking system (e.g. Linear, GH Issues, [runes](https://github.com/anowell/runes), beads, etc). 

## Introduction

- **Breq** - CLI for managing workspaces and the agent sessions inside them
- **Toren Daemon** - REST + WebSocket workspace API (experimental)
- **Station** - Manage reverse proxy configuration (e.g. proxies per workspace)
- **Web** - Browser-based interface connecting to the daemon (experimental)
- **Mobile** - Coming soon (tunnels to the daemon)

**Mental Model**:

A **workspace is a place** — a working copy + VCS state + an rmux session + its own state. Agents
run *in* a place; tasks and delivery (a PR, a CI run) are *facts about* a place.

- A segment (e.g. `app`) is a repo; its places are named `one`, `two`, ... (branch/workspace `one`)
- A place holds one or more agents (e.g. claude) and the shell alongside them
- Managing the place and updating the tracker are separate axes: tearing a workspace down is not the
  same act as marking its task done. `breq list` shows when they've diverged.
- Services in a place become accessible via `one.app.lvh.me` (or another local-resolving domain)

See [docs/CONCEPTS.md](docs/CONCEPTS.md) for the full model.


## Installation

```bash
cargo install --git https://github.com/anowell/toren breq
```

Optionally install [rmux](https://rmux.io/) as well. Agents then run inside persistent rmux
sessions, so closing the terminal leaves them running and the same pane is mirrored in the toren
web UI. Without rmux, `breq` execs the agent directly as before. See
[docs/terminals.md](docs/terminals.md).

## Getting Started

```bash
cd ~/projects/app

# Initialize toren.kdl - git ignored by .git/info/exclude
breq init --stealth
```

`breq init` does the out-of-box setup:
1. Creates `toren.kdl` in your repo with auto-discovered workspace hooks (e.g. copying `node_modules`)
2. Offers to register the repo as a segment in `~/.toren/config.kdl`
3. Installs the shipped workflow scripts (`breq-complete`, `breq-abort`) into `~/.toren/bin`, plus
   `breq-submit` when it detects a GitHub remote with `gh` installed

Then start an agent session:

```bash
breq do -p "Add input validation to the signup form"
```

Breq creates a workspace (git worktree or jj workspace), runs your setup hooks, and launches Claude
Code with your prompt. Each workspace gets a name ("one", "two", etc.).

## Breq CLI

`breq`'s verbs fall into two families. **Place verbs** manage the workspace; **task writes** update
your tracker and never touch the workspace. The only crossing point inside breq is `breq do <task>`,
which claims the task it starts on. The workflow scripts below compose both on purpose.

```bash
# Run a coding agent in a place (needs a task or a prompt)
breq do -p <prompt>                # New (or cwd-inferred) workspace, from a prompt
breq do -w <workspace> -p <prompt> # A specific workspace
breq do <task-id>                  # Claim a task, compose its context into the prompt
breq do <task-id> --agent codex    # Choose the agent; --model overrides the model
breq do --resume                   # Resume the workspace's most recent agent session
breq do --resume=<session-id>      # ...or a specific one (`breq get <ws> agent.sessions` lists them)
runes show tor-123 | breq do       # Prompt from stdin
breq do -w <workspace> --force     # Replace an agent already running there
breq do -p <prompt> --no-rmux      # Skip rmux; exec the agent directly

# Create and tear down places
breq setup [workspace]             # Create a workspace (no task, no agent)
breq setup --from <workspace>      # Stack a child workspace on another
breq setup <name>                  # Adopt an existing working copy in place
breq destroy <workspace>           # Delete a workspace (no status changes, no push)
breq destroy <workspace> --kill    # ...also stop live panes
breq destroy <workspace> --no-delete  # ...keep the working copy, drop only breq's state

# Read and annotate
breq list                          # One row per workspace: agents, changes, delivery, tasks
breq list --all --refresh          # Every segment; refresh remote metadata (the only networked path)
breq get <workspace>               # Full detail for one place (--json for scripts)
breq get <workspace> <key>         # One value, e.g. workspace.path, session, task.status
breq get <workspace> cache.<key>   # A cached read, with its age on stderr
breq set <workspace> title "..."   # Write a state field
breq set <workspace> +task runes:tor-1   # Link a task (+/- for list keys)
breq set <workspace> task.status done    # Write a task field (pass-through to the tracker)

# Work in a workspace directly
breq sh <workspace>                # Open a shell in the place
breq sh <workspace> -- <cmd>       # Run a command there

# Housekeeping
breq doctor --fix                  # Detect and repair known-bad state (migrates old assignments)
breq cleanup --all                 # Remove orphaned workspace directories
```

With [rmux](https://rmux.io/) installed, `breq do` runs the agent inside a persistent session and
mirrors that pane in your terminal — no multiplexer chrome, and closing the terminal leaves the
agent running, showing the same pane in the toren web UI. See
[docs/terminals.md](docs/terminals.md).

### Workflow verbs are scripts

Anything that isn't a built-in verb is dispatched git-style to a `breq-<name>` script on your `PATH`
(or in `~/.toren/bin`). The shipped defaults are task verbs — they update your tracker over the
place/task surface above, and are meant to be edited:

```bash
breq complete <ws>                 # Mark the workspace's linked tasks done, then destroy it
breq abort <ws>                    # Hand the tasks back as work-in-progress, then destroy it
breq submit <ws>                   # Push, open a PR, mark tasks in-review (github + runes flavour)
```

### Plugins

Trackers, agents, and forges are Rhai resolver plugins under `~/.toren/plugins/`, in three families:
`tasks/`, `agents/`, and `delivery/`. Adding a new one is a single `.rhai` file — no release.

```bash
breq plugin list                   # Browse installed and available plugins
breq plugin install tasks/linear   # Fetch a tracker resolver from contrib
breq plugin install agents/codex   # ...or an agent, or delivery/<forge>
```

See [docs/plugins.md](docs/plugins.md).


## Workspace Hooks (toren.kdl)

The `toren.kdl` file in your repo root configures workspace setup and destroy:

```kdl
var web_port="{{ 30000 + ws.num }}"
env LOG_LEVEL="info"
env ".env.shared"

setup {
    // Copy-on-write into workspace
    copy src="node_modules"
    // Symlink into workspace
    share src=".claude"
    // Execute arbitrary workspace setup commands
    env NODE_ENV="development"
    run "pnpm install"

    // Configure reverse proxy from `{{ws.name}}.{{repo.name}}.lvh.me` to your web_port
    // Short for: run "station proxy {{ws.name}} --port 80 --upstream {{vars.web_port}}"
    proxy "http" upstream="{{vars.web_port}}"
}

// Runs instead of `setup` for a child created with `breq setup --from <ws>`.
// {{ parent.path }} is the workspace being forked, so runtime state can be
// cloned rather than rebuilt. Falls back to `setup` if omitted.
fork {
    copy src="data" from="{{ parent.path }}"
}

destroy {
    run "just destroy-db"
}
```

**Directives:**
- `var NAME=VALUE ...` - Define template variables (top-level)
- `env NAME=VALUE ...` or `env "FILE" ...` - Set environment variables for `run` commands. Procedural and last-wins. See [docs/env.md](docs/env.md).
- `copy src="..."` - Copy file/directory using CoW when available
- `share src="..."` - Symlink to shared content
- `template src="..." dest="..."` - Copy and render with workspace template variables
- `run "command"` - Execute shell command. Supports `{ env ... }` children for command-scoped env.
- `proxy` - Register a reverse proxy route via [Station](station/README.md) - basically a shorthand for `run "station proxy {{ws.name}} --port <port> --upstream <upstream>"`

All string arguments support `{{ ... }}` template variables.

**Template variables:** `{{ ws.name }}`, `{{ ws.num }}`, `{{ ws.path }}`, `{{ repo.root }}`, `{{ repo.name }}`, `{{ task.id }}`, `{{ task.title }}`, `{{ vars.<name> }}`, and `{{ parent.path }}` inside a `fork` block

## More

- [docs/CONCEPTS.md](docs/CONCEPTS.md) - The model: places, the two verb families, the extension census
- [Configuration](docs/configuration.md) - Global config, proxy, delivery, tasks, and aliases
- [Plugins](docs/plugins.md) - The task / agent / delivery resolver families and the shipped scripts
- [Terminals](docs/terminals.md) - rmux sessions and zellij interop
- [Toren Daemon](daemon/README.md) - REST + WebSocket API for programmatic workspace and agent management
- [Station](station/README.md) - Reverse proxy management for per-workspace local domains
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) - Technical design

