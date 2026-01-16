#!/bin/bash
set -e

# Post-work verification hook for Claude Code
# Runs Rust checks and outputs change summary
# Does NOT auto-commit - use /smart-commit skill for intelligent commits

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

PROJECT_DIR="${CLAUDE_PROJECT_DIR:-.}"
RUST_DIR="$PROJECT_DIR/src"

log_info() { echo -e "${GREEN}[HOOK]${NC} $1"; }
log_warn() { echo -e "${YELLOW}[HOOK]${NC} $1"; }
log_error() { echo -e "${RED}[HOOK]${NC} $1" >&2; }
log_hint() { echo -e "${BLUE}[HINT]${NC} $1"; }

# Check if rust directory exists
check_rust_project() {
    if [ ! -d "$RUST_DIR" ] || [ ! -f "$RUST_DIR/Cargo.toml" ]; then
        log_warn "No Rust project found at $RUST_DIR, skipping Rust checks"
        return 1
    fi
    return 0
}

# Run cargo build
run_build() {
    log_info "Running cargo build..."
    cd "$RUST_DIR"
    if ! cargo build 2>&1; then
        log_error "Cargo build failed!"
        return 1
    fi
    log_info "Build passed!"
    return 0
}

# Run cargo test
run_tests() {
    log_info "Running cargo test..."
    cd "$RUST_DIR"
    if ! cargo test 2>&1; then
        log_error "Cargo tests failed!"
        return 1
    fi
    log_info "Tests passed!"
    return 0
}

# Output change summary (no commit)
show_change_summary() {
    cd "$PROJECT_DIR"

    echo ""
    echo "════════════════════════════════════════════════════════════"
    echo "                    CHANGE SUMMARY"
    echo "════════════════════════════════════════════════════════════"

    # Get list of changed files (unstaged + staged)
    UNSTAGED=$(git diff --name-only 2>/dev/null)
    STAGED=$(git diff --cached --name-only 2>/dev/null)
    UNTRACKED=$(git ls-files --others --exclude-standard 2>/dev/null)

    # Combine all changes
    ALL_CHANGES=$(echo -e "$UNSTAGED\n$STAGED\n$UNTRACKED" | grep -v '^$' | sort -u)

    if [ -z "$ALL_CHANGES" ]; then
        log_info "No changes detected"
        return 0
    fi

    FILE_COUNT=$(echo "$ALL_CHANGES" | wc -l | tr -d ' ')

    echo ""
    echo "Files changed: $FILE_COUNT"
    echo ""

    # Group by directory
    echo "By directory:"
    echo "$ALL_CHANGES" | cut -d'/' -f1-2 | sort | uniq -c | sort -rn | head -10 | while read -r count dir; do
        echo "  $count  $dir"
    done

    echo ""
    echo "Files:"
    echo "$ALL_CHANGES" | head -15 | while read -r f; do
        if echo "$STAGED" | grep -q "^$f$"; then
            echo -e "  ${GREEN}[staged]${NC} $f"
        elif echo "$UNTRACKED" | grep -q "^$f$"; then
            echo -e "  ${YELLOW}[new]${NC} $f"
        else
            echo -e "  [modified] $f"
        fi
    done

    if [ "$FILE_COUNT" -gt 15 ]; then
        echo "  ... and $((FILE_COUNT - 15)) more files"
    fi

    # Show diff stats
    echo ""
    STATS=$(git diff --stat 2>/dev/null | tail -1)
    if [ -n "$STATS" ]; then
        echo "Stats: $STATS"
    fi

    echo ""
    echo "════════════════════════════════════════════════════════════"
    echo ""
    log_hint "Run /doc-sync to update CLAUDE.md"
    log_hint "Run /smart-commit to create intelligent commits"
    echo ""

    return 0
}

# Main execution
main() {
    log_info "Starting post-work verification..."

    # Run Rust checks if project exists
    if check_rust_project; then
        if ! run_build; then
            log_error "Build verification failed"
            exit 2  # Block Claude from stopping
        fi

        if ! run_tests; then
            log_error "Test verification failed"
            exit 2  # Block Claude from stopping
        fi
    fi

    # Show change summary (don't commit)
    show_change_summary

    log_info "Verification completed! Use /doc-sync and /smart-commit when ready."
    exit 0
}

main "$@"
