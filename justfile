# Toren - Justfile

# Load .env file automatically
set dotenv-load

# Default recipe - show available commands
default:
    @just --list

# Build the Rust daemon
build:
    cargo build

# Build the TypeScript ancillary runtime
build-ancillary:
    cd ancillary && npm install && npm run build

# Build everything
build-all: build build-ancillary

# Start the Toren daemon (dev mode - runs from source, uses local toren.toml)
daemon:
    cargo run --bin toren-daemon

# Start the web server (dev mode)
web:
    cd web && npm run dev

# Send a prompt to Claude through Toren (auto-pairs if needed)
# Interactive mode: just prompt examples/calculator (asks for input)
# Stdin mode: echo "Build X" | just prompt examples/todo
# Usage: just prompt examples/calculator                  (interactive)
#        echo "Build a calculator" | just prompt          (piped)
#        just prompt < prompt.txt                         (redirected)
#        just prompt examples/todo < prompt.txt           (with directory)
#        cat prompt.txt | just prompt my-app              (piped from cat)
prompt DIR="examples/default":
    #!/usr/bin/env bash
    set -euo pipefail
    cd "$(dirname "{{justfile()}}")"

    # Create directory if it doesn't exist
    mkdir -p "{{DIR}}"
    export WORKING_DIR="{{DIR}}"

    # Auto-pair if SESSION_TOKEN is not set
    if [ -z "${SESSION_TOKEN:-}" ]; then
        if [ -z "${PAIRING_TOKEN:-}" ]; then
            echo "❌ Error: Neither SESSION_TOKEN nor PAIRING_TOKEN is set"
            echo "Set PAIRING_TOKEN in .env or export SESSION_TOKEN"
            exit 1
        fi

        echo "🔐 Auto-pairing with PAIRING_TOKEN..."
        PAIR_RESPONSE=$(curl -s -X POST http://localhost:8787/pair \
            -H "Content-Type: application/json" \
            -d "{\"pairing_token\": \"$PAIRING_TOKEN\"}")

        export SESSION_TOKEN=$(echo "$PAIR_RESPONSE" | jq -r '.session_token')

        if [ "$SESSION_TOKEN" = "null" ] || [ -z "$SESSION_TOKEN" ]; then
            echo "❌ Pairing failed: $PAIR_RESPONSE"
            exit 1
        fi

        echo "✅ Paired successfully! Session token: ${SESSION_TOKEN:0:10}..."
    fi

    NODE_PATH=./ancillary/node_modules npx tsx scripts/toren-cli.ts


# Get a session token (requires pairing token from daemon)
# Usage: just pair 714697
pair PAIRING_TOKEN:
    curl -X POST http://localhost:8787/pair \
        -H "Content-Type: application/json" \
        -d '{"pairing_token": "{{PAIRING_TOKEN}}"}'

# Check daemon health
health:
    curl -s http://localhost:8787/health | jq .

# List available plugins/commands
plugins:
    curl -s http://localhost:8787/api/plugins/commands | jq .

# Clean build artifacts
clean:
    cargo clean
    rm -rf ancillary/dist
    rm -rf ancillary/node_modules
    rm -rf target

# Clean example outputs
clean-examples:
    rm -rf examples/calculator
    rm -rf examples/hello-world


# Install dependencies
install:
    cargo build
    cd ancillary && npm install

# Format code
fmt:
    cargo fmt
    cd ancillary && npm run build

