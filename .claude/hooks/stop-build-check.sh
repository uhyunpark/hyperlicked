#!/bin/bash
set -e

# Stop event hook that runs Rust build/test checks
# This runs when Claude Code finishes responding
# Exit 0 = success, Exit 2 = send feedback to Claude

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Project directory
PROJECT_DIR="${CLAUDE_PROJECT_DIR:-.}"

# Check if this is a Rust project
if [[ ! -f "$PROJECT_DIR/Cargo.toml" ]]; then
    exit 0
fi

# Check if any Rust files were modified in this session
# Look for recent modifications to .rs files
rs_files_changed=$(git -C "$PROJECT_DIR" diff --name-only HEAD 2>/dev/null | grep '\.rs$' || true)
rs_files_staged=$(git -C "$PROJECT_DIR" diff --cached --name-only 2>/dev/null | grep '\.rs$' || true)
rs_files_untracked=$(git -C "$PROJECT_DIR" ls-files --others --exclude-standard 2>/dev/null | grep '\.rs$' || true)

# Combine all changed Rust files
all_rs_changes=$(echo -e "$rs_files_changed\n$rs_files_staged\n$rs_files_untracked" | grep -v '^$' | sort -u)

# If no Rust files changed, skip checks
if [[ -z "$all_rs_changes" ]]; then
    exit 0
fi

# Count changed files
file_count=$(echo "$all_rs_changes" | wc -l | tr -d ' ')

echo "" >&2
echo -e "${YELLOW}[HOOK]${NC} Rust files changed ($file_count), running build check..." >&2

# Run cargo build and capture output
cd "$PROJECT_DIR"
build_output=$(cargo build 2>&1) || build_failed=true

if [[ "$build_failed" == "true" ]]; then
    # Count errors
    error_count=$(echo "$build_output" | grep -c "^error\[E" || echo "0")

    echo "" >&2
    echo -e "${RED}## Rust Build Failed${NC}" >&2
    echo "" >&2
    echo "Found $error_count compilation error(s):" >&2
    echo "" >&2

    # Show errors (limit output)
    echo "$build_output" | grep -A 5 "^error\[E" | head -50 >&2

    echo "" >&2
    echo "Please fix these compilation errors." >&2

    # Exit 2 to send feedback to Claude
    exit 2
fi

echo -e "${GREEN}[HOOK]${NC} Build passed!" >&2

# Run cargo test and capture output
test_output=$(cargo test 2>&1) || test_failed=true

if [[ "$test_failed" == "true" ]]; then
    # Count test failures
    fail_count=$(echo "$test_output" | grep -c "^test .* FAILED" || echo "0")

    echo "" >&2
    echo -e "${RED}## Rust Tests Failed${NC}" >&2
    echo "" >&2
    echo "Found $fail_count test failure(s):" >&2
    echo "" >&2

    # Show failed tests
    echo "$test_output" | grep -B 2 "^test .* FAILED" >&2
    echo "" >&2

    # Show failure details (limited)
    echo "Failure details:" >&2
    echo "$test_output" | grep -A 10 "failures:" | head -30 >&2

    echo "" >&2
    echo "Please fix these test failures." >&2

    # Exit 2 to send feedback to Claude
    exit 2
fi

# Extract test summary
test_summary=$(echo "$test_output" | grep "^test result:" | tail -1)
echo -e "${GREEN}[HOOK]${NC} Tests passed! $test_summary" >&2

# Show change summary
echo "" >&2
echo "════════════════════════════════════════" >&2
echo "  Changed Rust files ($file_count):" >&2
echo "════════════════════════════════════════" >&2
echo "$all_rs_changes" | head -10 | sed 's/^/  /' >&2
if [[ $file_count -gt 10 ]]; then
    echo "  ... and $((file_count - 10)) more" >&2
fi
echo "" >&2

exit 0
