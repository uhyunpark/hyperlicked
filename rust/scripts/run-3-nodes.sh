#!/bin/bash
# Run 3 nodes for multi-node consensus testing
#
# Usage:
#   ./scripts/run-3-nodes.sh       # Run all 3 nodes
#   ./scripts/run-3-nodes.sh stop  # Stop all nodes

set -e

cd "$(dirname "$0")/.."

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

if [ "$1" == "stop" ]; then
    echo -e "${YELLOW}Stopping all nodes...${NC}"
    pkill -f "multinode" 2>/dev/null || true
    echo -e "${GREEN}Done${NC}"
    exit 0
fi

# Build first
echo -e "${BLUE}Building...${NC}"
cargo build --bin multinode

echo -e "${GREEN}Build complete${NC}"
echo ""

# Kill any existing nodes
pkill -f "multinode" 2>/dev/null || true
sleep 1

# Start nodes in background
echo -e "${BLUE}Starting 3 nodes...${NC}"
echo ""

# Node 0 (will be leader for view 0)
echo -e "${GREEN}Starting Node 0 (port 9000)...${NC}"
RUST_LOG=info cargo run --bin multinode -- --node 0 2>&1 | sed 's/^/[Node 0] /' &
sleep 0.5

# Node 1
echo -e "${GREEN}Starting Node 1 (port 9001)...${NC}"
RUST_LOG=info cargo run --bin multinode -- --node 1 2>&1 | sed 's/^/[Node 1] /' &
sleep 0.5

# Node 2
echo -e "${GREEN}Starting Node 2 (port 9002)...${NC}"
RUST_LOG=info cargo run --bin multinode -- --node 2 2>&1 | sed 's/^/[Node 2] /' &

echo ""
echo -e "${YELLOW}All nodes started. Press Ctrl+C to stop.${NC}"
echo -e "${YELLOW}Logs are prefixed with [Node X]${NC}"
echo ""

# Wait for all background jobs
wait
