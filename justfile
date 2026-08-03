# Toren - Justfile

# Load .env file automatically
set dotenv-load

# Default recipe - show available commands
default:
    @just --list

# Build all Rust binaries
build:
    cargo build

# Start the Toren daemon (dev: rebuilds on change, debug build)
daemon:
    bacon run -- --bin toren-daemon

# Start the Toren daemon as a release build
#
# A debug build costs several times the CPU per byte mirrored, which is what a workspace full of
# panes actually notices. Use this when running toren rather than working on it.
daemon-release:
    cargo run --release --bin toren-daemon

# Measure this machine's rmux against the assumptions the mirror is built on:
# subscription cap, snapshot fidelity and cost, lifecycle push, size ownership.
rmux-spike WHAT="caps" *ARGS:
    cargo run -p toren-mirror --example rmux_spike -- {{WHAT}} {{ARGS}}

# Start the web UI (dev mode)
web:
    cd web && bun run dev

# Run the breq CLI in an example segment directory (dev mode)
cli SEGMENT *ARGS:
    cd examples/{{SEGMENT}} && cargo run --manifest-path {{justfile_directory()}}/Cargo.toml --bin breq -- --config {{justfile_directory()}}/toren-test.kdl {{ARGS}}

# Check daemon health
health:
    curl -s http://localhost:8788/health | jq .

# Run all checks (cargo check, clippy, biome, svelte-check)
check:
    cargo check
    cargo clippy -- -D warnings
    cd web && bun run check
    cd web && bun run lint

# Run tests
test:
    cargo test
    cd web && bun run test

# Format code
fmt:
    cargo fmt
    cd web && bun run format

# Clean build artifacts
clean:
    cargo clean
    rm -rf target

# Get a session token (requires pairing token from daemon)
pair PAIRING_TOKEN:
    curl -X POST http://localhost:8788/pair \
        -H "Content-Type: application/json" \
        -d '{"pairing_token": "{{PAIRING_TOKEN}}"}'
