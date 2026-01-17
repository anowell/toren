#!/usr/bin/env bash
# End-to-end test for ancillary work via daemon API
#
# Prerequisites:
# - ANTHROPIC_API_KEY set in environment
# - toren daemon running with a configured segment
# - A bead exists in the segment (or use prompt-based assignment)

set -euo pipefail

DAEMON_URL="${DAEMON_URL:-http://localhost:8787}"
SEGMENT="${SEGMENT:-toren}"
PAIRING_TOKEN="${PAIRING_TOKEN:-}"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log() { echo -e "${GREEN}[TEST]${NC} $*"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*"; }

# Check if daemon is running
log "Checking daemon health..."
if ! curl -sf "${DAEMON_URL}/health" > /dev/null; then
    error "Daemon not running at ${DAEMON_URL}"
    echo "Start the daemon with: cargo run --package toren-daemon"
    exit 1
fi
log "Daemon is healthy"

# Pair to get session token (if not already set)
if [[ -z "$PAIRING_TOKEN" ]]; then
    error "PAIRING_TOKEN not set. Get it from daemon startup logs."
    exit 1
fi

log "Pairing with daemon..."
PAIR_RESPONSE=$(curl -sf -X POST "${DAEMON_URL}/pair" \
    -H "Content-Type: application/json" \
    -d "{\"pairing_token\": \"${PAIRING_TOKEN}\"}")
SESSION_TOKEN=$(echo "$PAIR_RESPONSE" | jq -r '.session_token')

if [[ -z "$SESSION_TOKEN" || "$SESSION_TOKEN" == "null" ]]; then
    error "Failed to pair: $PAIR_RESPONSE"
    exit 1
fi
log "Paired successfully, got session token"

# List segments
log "Listing segments..."
SEGMENTS=$(curl -sf "${DAEMON_URL}/api/segments/list")
echo "$SEGMENTS" | jq .

# Create an assignment with a simple prompt
log "Creating assignment with prompt..."
ASSIGNMENT_RESPONSE=$(curl -sf -X POST "${DAEMON_URL}/api/assignments" \
    -H "Content-Type: application/json" \
    -d "{
        \"segment\": \"${SEGMENT}\",
        \"prompt\": \"Create a file called hello.txt with the content 'Hello from ancillary!'\"
    }")

echo "$ASSIGNMENT_RESPONSE" | jq .

ASSIGNMENT_ID=$(echo "$ASSIGNMENT_RESPONSE" | jq -r '.assignment.id')
ANCILLARY_ID=$(echo "$ASSIGNMENT_RESPONSE" | jq -r '.assignment.ancillary_id')
BEAD_ID=$(echo "$ASSIGNMENT_RESPONSE" | jq -r '.assignment.bead_id')

if [[ -z "$ASSIGNMENT_ID" || "$ASSIGNMENT_ID" == "null" ]]; then
    error "Failed to create assignment"
    exit 1
fi

log "Created assignment: $ASSIGNMENT_ID"
log "Ancillary: $ANCILLARY_ID"
log "Bead: $BEAD_ID"

# URL encode the ancillary ID (replace spaces with %20)
ANCILLARY_ID_ENCODED=$(echo "$ANCILLARY_ID" | sed 's/ /%20/g')

# Start work on the ancillary
log "Starting work on ancillary..."
START_RESPONSE=$(curl -sf -X POST "${DAEMON_URL}/api/ancillaries/${ANCILLARY_ID_ENCODED}/start" \
    -H "Content-Type: application/json" \
    -d "{\"assignment_id\": \"${ASSIGNMENT_ID}\"}")

echo "$START_RESPONSE" | jq .

if [[ $(echo "$START_RESPONSE" | jq -r '.success') != "true" ]]; then
    error "Failed to start work"
    exit 1
fi
log "Work started!"

# Connect to WebSocket and stream events
log "Connecting to WebSocket to stream work events..."
log "WebSocket URL: ws://localhost:8787/ws/ancillaries/${ANCILLARY_ID_ENCODED}?from_seq=0"
echo ""
warn "To see live events, run in another terminal:"
echo "  websocat \"ws://localhost:8787/ws/ancillaries/${ANCILLARY_ID_ENCODED}?from_seq=0\""
echo ""

# Wait a bit then check the work log
log "Waiting 5 seconds for work to progress..."
sleep 5

# List ancillaries to see status
log "Checking ancillary status..."
curl -sf "${DAEMON_URL}/api/ancillaries/list" | jq .

log "Test complete!"
echo ""
echo "Next steps:"
echo "1. Monitor the WebSocket for live events"
echo "2. Check the work log at ~/.toren/ancillaries/$(echo "$ANCILLARY_ID" | tr ' ' '-' | tr '[:upper:]' '[:lower:]')/work/${BEAD_ID}.jsonl"
echo "3. Stop work with: curl -X POST ${DAEMON_URL}/api/ancillaries/${ANCILLARY_ID_ENCODED}/stop"
