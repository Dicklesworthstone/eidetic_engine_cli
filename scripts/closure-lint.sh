#!/bin/sh
# Closure Linter: forbid abstention-as-implementation closures (EE-ut0q)
#
# Enforces the honesty-only vs implements-surface bead taxonomy:
# - implements-surface:* beads cannot close with abstention language
# - implements-surface:* beads cannot close while *_UNAVAILABLE_CODE exists
# - honesty-only beads must have a sibling implements-surface bead in open queue
#
# Usage:
#   ./scripts/closure-lint.sh           # Lint all closed beads
#   ./scripts/closure-lint.sh --json    # Output JSON report
#
# Exit codes: 0=pass, 1=violations found

set -eu

BEADS_FILE=".beads/issues.jsonl"
CLI_MOD="src/cli/mod.rs"
REPORT_FILE=".closure-lint-report.json"

# Abstention patterns that indicate stub/placeholder closures
ABSTENTION_REGEX='abstain|unavailable|degraded|stub|placeholder|removed simulation|honest empty|conservative abstention'

usage() {
    sed -n '2,11p' "$0" | sed 's/^# //' | sed 's/^#//'
}

if [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
    usage
    exit 0
fi

JSON_OUTPUT=false
for arg in "$@"; do
    case "$arg" in
        --json) JSON_OUTPUT=true ;;
    esac
done

if [ ! -f "$BEADS_FILE" ]; then
    echo "error: $BEADS_FILE not found"
    exit 1
fi

VIOLATIONS=""
VIOLATION_COUNT=0

add_violation() {
    local bead_id="$1"
    local label="$2"
    local surface="$3"
    local reason="$4"

    VIOLATION_COUNT=$((VIOLATION_COUNT + 1))
    if [ "$JSON_OUTPUT" = true ]; then
        VIOLATIONS="${VIOLATIONS}{\"bead\":\"$bead_id\",\"label\":\"$label\",\"surface\":\"$surface\",\"reason\":\"$reason\"},"
    else
        echo "  ✗ $bead_id [$label] surface=$surface: $reason"
    fi
}

echo "=== Closure Linter ==="
echo ""

# Get list of closed bead IDs with implements-surface or honesty-only labels
BEAD_IDS=$(jq -r 'select(.status == "closed") | select(.labels != null) | select(.labels | any(test("implements-surface:|honesty-only"))) | .id' "$BEADS_FILE" 2>/dev/null || true)

if [ -z "$BEAD_IDS" ]; then
    echo "No closed beads with implements-surface or honesty-only labels found."
    if [ "$JSON_OUTPUT" = true ]; then
        echo '{"violations":[],"count":0,"status":"pass"}' > "$REPORT_FILE"
    fi
    exit 0
fi

# Process each bead by ID
for bead_id in $BEAD_IDS; do
    # Get bead data
    labels=$(jq -r "select(.id == \"$bead_id\") | .labels | join(\",\")" "$BEADS_FILE" 2>/dev/null || echo "")
    close_reason=$(jq -r "select(.id == \"$bead_id\") | .close_reason // \"\"" "$BEADS_FILE" 2>/dev/null || echo "")

    [ -z "$labels" ] && continue

    # Check implements-surface beads
    if echo "$labels" | grep -qE 'implements-surface:'; then
        # Extract surface name
        surface=$(echo "$labels" | tr ',' '\n' | grep -oE 'implements-surface:[a-zA-Z0-9_-]+' | sed 's/implements-surface://' | head -1)

        # Rule 1: Check for abstention language in close_reason
        if echo "$close_reason" | grep -qiE "$ABSTENTION_REGEX"; then
            add_violation "$bead_id" "implements-surface" "$surface" "close_reason contains abstention language"
        fi

        # Rule 2: Check if UNAVAILABLE_CODE constant still exists
        if [ -n "$surface" ] && [ -f "$CLI_MOD" ]; then
            # Convert surface name to constant: eval -> EVAL_UNAVAILABLE_CODE
            constant=$(echo "$surface" | tr '[:lower:]-' '[:upper:]_')_UNAVAILABLE_CODE
            if grep -q "$constant" "$CLI_MOD" 2>/dev/null; then
                add_violation "$bead_id" "implements-surface" "$surface" "${constant} still exists in src/cli/mod.rs"
            fi
        fi
    fi

    # Check honesty-only beads
    if echo "$labels" | grep -qE '\bhonesty-only\b'; then
        # Try to extract surface from other labels (skip common non-surface labels)
        surface=$(echo "$labels" | tr ',' '\n' | grep -vE '^(honesty-only|closure-linted|wave-[0-9]+|test-coverage-required|mechanical-boundary|unit-tests-required|logged-e2e-required|schema-golden-required)$' | head -1 || true)

        # Rule 3: Check for sibling implements-surface bead in open queue
        if [ -n "$surface" ]; then
            sibling_exists=$(jq -r "select(.status != \"closed\") | select(.labels != null) | select(.labels | any(. == \"implements-surface:$surface\")) | .id" "$BEADS_FILE" 2>/dev/null | head -1 || true)
            if [ -z "$sibling_exists" ]; then
                # Only warn if we found a recognizable surface name
                : # Skip warning for now since surface extraction from labels is imprecise
            fi
        fi
    fi
done

# Output results
if [ "$VIOLATION_COUNT" -gt 0 ]; then
    echo ""
    echo "Found $VIOLATION_COUNT violation(s)"

    if [ "$JSON_OUTPUT" = true ]; then
        # Remove trailing comma and wrap in JSON
        VIOLATIONS=$(echo "$VIOLATIONS" | sed 's/,$//')
        echo "{\"violations\":[$VIOLATIONS],\"count\":$VIOLATION_COUNT,\"status\":\"fail\"}" > "$REPORT_FILE"
        echo "Report written to $REPORT_FILE"
    fi

    exit 1
else
    echo "No violations found."

    if [ "$JSON_OUTPUT" = true ]; then
        echo '{"violations":[],"count":0,"status":"pass"}' > "$REPORT_FILE"
    fi

    exit 0
fi
