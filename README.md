# Toren

> *"I am Toren. I am continuity."*

Distributed development intelligence - orchestrate Claude Code sessions across workspaces.

## Usage

### Installation

```bash
# Install the CLI and daemon
cargo install --path breq
cargo install --path daemon

# Or build locally
cargo build --release
# Binaries at target/release/breq and target/release/toren-daemon
```

### Configuration

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

### Basic Workflow

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
```

### Workspace Setup Hooks

Add a `.toren.kdl` file to your repo root to automate workspace initialization:

```kdl
setup {
    template src=".env.breq" dest=".env"
    copy src="config.example.json" dest="config.json"
    run "pnpm install"
    run "just migrate"
}

destroy {
    run "just destroy-db"
}
```

**Actions:**
- `template src="..." dest="..."` - Copy and render with workspace context
- `copy src="..." dest="..."` - Copy file verbatim
- `run "command"` - Execute shell command

**Template variables:**
```
{{ ws.name }}    # Workspace name (e.g., "one")
{{ ws.slug }}    # Filesystem-safe name
{{ ws.idx }}     # Stable integer index (1, 2, 3...)
{{ ws.id }}      # Unique ID (e.g., "one-1")
{{ ws.path }}    # Full workspace path
{{ repo.root }}  # Repository root path
{{ repo.name }}  # Repository name
```

Example `.env.breq` template:
```env
PORT={{ 3000 + ws.idx }}
DATABASE_URL=postgres://localhost/myapp_{{ ws.slug }}
```

**Manual execution:**
```bash
breq ws setup     # Run setup in current workspace
breq ws destroy   # Run destroy in current workspace
```

### Web Interface

The web UI provides mobile-friendly access to your Toren sessions:

```bash
cd web && pnpm install && pnpm dev
# Open http://localhost:5173
```

## Developer Setup

### Prerequisites

- Rust toolchain (`rustup`)
- Node.js and pnpm (for web UI)
- jj (Jujutsu VCS)

### Setup

```bash
git clone <repo> && cd toren
cargo build
cd web && pnpm install
```

### Development Commands

```bash
# Run the daemon (with hot reload via cargo)
just daemon

# Run the web UI (Vite dev server)
just web

# Run the CLI during development
cargo run --bin breq -- <args>
# Or after building:
just cli <args>
```

### Project Structure

```
toren/
├── breq/           # CLI for assignment management
├── daemon/         # Rust daemon (Axum + WebSocket)
├── lib/            # Shared library (toren-lib)
├── web/            # SvelteKit web interface
├── docs/           # Architecture documentation
└── examples/       # Sample segments
```

## Documentation

- [docs/CONCEPTS.md](docs/CONCEPTS.md) - Naming and metaphor
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) - Technical design
- [docs/SEGMENTS.md](docs/SEGMENTS.md) - Segment configuration

## License

MIT
