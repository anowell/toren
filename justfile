# Toren - Justfile

# Load .env file automatically
set dotenv-load

# Default recipe - show available commands
default:
    @just --list

# Build all Rust binaries
build:
    cargo build

# Start the Toren daemon (dev mode, uses toren-test.toml)
daemon:
    bacon run -- --bin toren-daemon -- --config toren-test.toml

# Start the web UI (dev mode)
web:
    cd web && pnpm dev

# Run the breq CLI in an example segment directory (dev mode)
cli SEGMENT *ARGS:
    cd examples/{{SEGMENT}} && cargo run --manifest-path {{justfile_directory()}}/Cargo.toml --bin breq -- --config {{justfile_directory()}}/toren-test.toml {{ARGS}}

# Check daemon health
health:
    curl -s http://localhost:8788/health | jq .

# List available plugins/commands
plugins:
    curl -s http://localhost:8788/api/plugins/commands | jq .

# Run tests
test:
    cargo test

# Format code
fmt:
    cargo fmt

# Clean build artifacts
clean:
    cargo clean
    rm -rf target

# Get a session token (requires pairing token from daemon)
pair PAIRING_TOKEN:
    curl -X POST http://localhost:8788/pair \
        -H "Content-Type: application/json" \
        -d '{"pairing_token": "{{PAIRING_TOKEN}}"}'
