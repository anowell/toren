#!/bin/bash
# Test script for multiple concurrent ancillaries

set -e

BASE_URL="http://localhost:8787"
PAIRING_TOKEN="${PAIRING_TOKEN:-123456}"

echo "=== Multi-Ancillary Test ==="
echo

# Helper to check ancillaries list
check_ancillaries() {
    echo "📋 Current ancillaries:"
    curl -s "$BASE_URL/api/ancillaries/list" | jq '.'
    echo
}

# Get two session tokens
echo "1. Creating two sessions..."
SESSION1=$(curl -s -X POST "$BASE_URL/pair" \
    -H "Content-Type: application/json" \
    -d "{\"pairing_token\": \"$PAIRING_TOKEN\"}" | jq -r '.session_token')

SESSION2=$(curl -s -X POST "$BASE_URL/pair" \
    -H "Content-Type: application/json" \
    -d "{\"pairing_token\": \"$PAIRING_TOKEN\"}" | jq -r '.session_token')

echo "✅ Session 1: ${SESSION1:0:10}..."
echo "✅ Session 2: ${SESSION2:0:10}..."
echo

# Check initial state (should be empty)
echo "2. Initial state (no ancillaries connected):"
check_ancillaries

# Start first ancillary in background
echo "3. Starting Calculator One..."
cat <<'EOF' > /tmp/calculator-prompt.txt
Create a file called calculator.txt with the text:
Calculator implementation: add(a, b) = a + b
EOF

export SESSION_TOKEN="$SESSION1"
(cat /tmp/calculator-prompt.txt | just prompt examples/calculator 2>&1 | sed 's/^/  [Calculator] /') &
CALC_PID=$!

sleep 2
echo "✅ Calculator One started (PID: $CALC_PID)"
echo

# Check ancillaries (should show Calculator One connected)
echo "4. After Calculator One connects:"
check_ancillaries

# Start second ancillary in background
echo "5. Starting Fizzbuzz One..."
cat <<'EOF' > /tmp/fizzbuzz-prompt.txt
Create a file called fizzbuzz.txt with the text:
FizzBuzz implementation: for i in 1..100, print Fizz if i%3==0, Buzz if i%5==0, FizzBuzz if both
EOF

export SESSION_TOKEN="$SESSION2"
(cat /tmp/fizzbuzz-prompt.txt | just prompt examples/fizzbuzz 2>&1 | sed 's/^/  [Fizzbuzz] /') &
FIZZ_PID=$!

sleep 2
echo "✅ Fizzbuzz One started (PID: $FIZZ_PID)"
echo

# Check ancillaries (should show both)
echo "6. After both ancillaries connect:"
check_ancillaries

# Wait a bit for them to start executing
echo "7. Waiting for ancillaries to execute instructions..."
sleep 5

# Check during execution
echo "8. During execution:"
check_ancillaries

# Wait for both to complete
echo "9. Waiting for ancillaries to complete..."
wait $CALC_PID
wait $FIZZ_PID
echo "✅ Both ancillaries completed"
echo

# Give them a moment to disconnect
sleep 2

# Check final state (should be empty again)
echo "10. Final state (after disconnect):"
check_ancillaries

# Verify files were created
echo "11. Verifying output files:"
if [ -f examples/calculator/calculator.txt ]; then
    echo "✅ Calculator output:"
    cat examples/calculator/calculator.txt | sed 's/^/   /'
else
    echo "❌ Calculator output not found"
fi

if [ -f examples/fizzbuzz/fizzbuzz.txt ]; then
    echo "✅ Fizzbuzz output:"
    cat examples/fizzbuzz/fizzbuzz.txt | sed 's/^/   /'
else
    echo "❌ Fizzbuzz output not found"
fi

echo
echo "=== Multi-Ancillary Test Complete ==="
