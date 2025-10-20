#!/bin/bash
# View transaction log with pretty-printing

TX_LOG_FILE=${TX_LOG_FILE:-"data/transactions.log"}

if [ ! -f "$TX_LOG_FILE" ]; then
    echo "Transaction log not found: $TX_LOG_FILE"
    echo "Submit some orders from the frontend first!"
    exit 1
fi

echo "=== Transaction Log: $TX_LOG_FILE ==="
echo ""

# Check if jq is installed for pretty-printing
if command -v jq &> /dev/null; then
    # Pretty-print with jq
    cat "$TX_LOG_FILE" | jq -C '.'
else
    # Fallback: just cat the file
    cat "$TX_LOG_FILE"
    echo ""
    echo "Tip: Install 'jq' for prettier output: brew install jq"
fi
