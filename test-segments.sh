#!/bin/bash
set -e

echo "Testing Toren Segment Discovery..."

# Start daemon
echo "Starting daemon..."
cargo run &
DAEMON_PID=$!
sleep 4

# Test segments API
echo -e "\n=== Available Segments ==="
curl -s http://localhost:8787/api/segments/list | jq '.segments[] | {name, source}'

echo -e "\n=== Segment Roots ==="
curl -s http://localhost:8787/api/segments/list | jq '.roots'

echo -e "\n=== Create New Segment ==="
curl -s -X POST http://localhost:8787/api/segments/create \
  -H "Content-Type: application/json" \
  -d '{"name":"test-project","root":"'$(pwd)'/examples"}' | jq .

echo -e "\n=== Verify New Segment ==="
curl -s http://localhost:8787/api/segments/list | jq '.segments[] | select(.name=="test-project")'

# Cleanup
kill $DAEMON_PID
rm -rf examples/test-project
echo -e "\n✅ All tests passed!"
