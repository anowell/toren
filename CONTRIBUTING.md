# Contributing to Toren

## Prerequisites

- Rust toolchain (`rustup`)
- [bun](https://bun.sh) (for the web UI)
- jj (Jujutsu VCS)

## Setup

```bash
git clone <repo> && cd toren
cargo build
cd web && bun install
```

## Development Commands

```bash
# Run the daemon (with hot reload via cargo)
just daemon

# Run the web UI (Vite dev server)
just web

# Run the CLI during development
cargo run --bin breq -- <args>
# Or after building:
just cli <args>

# Everything CI would run: cargo check/clippy, svelte-check, biome
just check
just test
```

## Project Structure

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
