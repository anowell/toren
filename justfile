# Toren - Justfile

# Load .env file automatically
set dotenv-load

# Default recipe - show available commands
default:
    @just --list

# Build all Rust binaries
build:
    cargo build

# Build release binaries
build-release:
    cargo build --release

# Start the Toren daemon (dev mode)
daemon:
    bacon run -- --bin toren-daemon

# Start the web UI (dev mode)
web:
    cd web && pnpm dev

# Run the breq CLI (dev mode)
cli *ARGS:
    cargo run --bin breq -- {{ARGS}}

# Check daemon health
health:
    curl -s http://localhost:8787/health | jq .

# List available plugins/commands
plugins:
    curl -s http://localhost:8787/api/plugins/commands | jq .

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

# Install dependencies (web)
install-web:
    cd web && pnpm install

# Get a session token (requires pairing token from daemon)
pair PAIRING_TOKEN:
    curl -X POST http://localhost:8787/pair \
        -H "Content-Type: application/json" \
        -d '{"pairing_token": "{{PAIRING_TOKEN}}"}'
