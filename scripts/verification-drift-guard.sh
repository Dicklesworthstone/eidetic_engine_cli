#!/bin/bash
# Verification Drift Guard (EE-eism)
#
# Detects when verify.sh gates are red without a corresponding open bead.
# Prevents "invisible drift" where failing gates become normalized background noise.
#
# Usage:
#   ./scripts/verification-drift-guard.sh                # Check all gates
#   ./scripts/verification-drift-guard.sh --json         # Write JSON report
#   ./scripts/verification-drift-guard.sh --gate <name>  # Check specific gate
#
# Gates tracked:
#   - closure-lint: closure discipline violations
#   - cargo-test: test suite failures
#   - forbidden-deps: banned dependency violations
#
# Exit codes: 0=pass (all red gates have beads), 1=drift detected

set -eu

BEADS_FILE=".beads/issues.jsonl"
REPORT_FILE=".verification-drift-report.json"
CLOSURE_REPORT=".closure-lint-report.json"

JSON_OUTPUT=false
CHECK_GATE=""

for arg in "$@"; do
    case "$arg" in
        --json) JSON_OUTPUT=true ;;
        --gate=*) CHECK_GATE="${arg#--gate=}" ;;
        --help|-h)
            sed -n '2,16p' "$0" | sed 's/^# //' | sed 's/^#//'
            exit 0
            ;;
    esac
done

DRIFT_VIOLATIONS=""
DRIFT_COUNT=0

add_drift() {
    local gate="$1"
    local reason="$2"

    DRIFT_COUNT=$((DRIFT_COUNT + 1))
    local obj
    obj=$(jq -cn \
        --arg gate "$gate" \
        --arg reason "$reason" \
        '{gate:$gate,reason:$reason}')
    DRIFT_VIOLATIONS="${DRIFT_VIOLATIONS}${obj}
"

    if [ "$JSON_OUTPUT" != true ]; then
        echo "  DRIFT: $gate - $reason"
    fi
}

write_drift_report() {
    local status="$1"
    if [ -n "$DRIFT_VIOLATIONS" ]; then
        printf "%s" "$DRIFT_VIOLATIONS" |
            jq -s --arg status "$status" '{driftViolations:.,count:length,status:$status}' > "$REPORT_FILE"
    else
        jq -cn --arg status "$status" '{driftViolations:[],count:0,status:$status}' > "$REPORT_FILE"
    fi
}

# Check if an open bead exists matching keywords
has_open_bead_for() {
    local keywords="$1"
    if [ ! -f "$BEADS_FILE" ]; then
        return 1
    fi

    # Build jq filter for keywords (case-insensitive match in title or labels)
    jq -e --arg kw "$keywords" '
        select(.status != "closed")
        | select(
            (.title | ascii_downcase | test($kw | ascii_downcase))
            or ((.labels // []) | any(. | ascii_downcase | test($kw | ascii_downcase)))
            or ((.description // "") | ascii_downcase | test($kw | ascii_downcase))
          )
    ' "$BEADS_FILE" >/dev/null 2>&1
}

# Gate: closure-lint
check_closure_lint_drift() {
    if [ -n "$CHECK_GATE" ] && [ "$CHECK_GATE" != "closure-lint" ]; then
        return 0
    fi

    # Run closure-lint if report doesn't exist
    if [ ! -f "$CLOSURE_REPORT" ]; then
        ./scripts/closure-lint.sh --audit --json >/dev/null 2>&1 || true
    fi

    if [ ! -f "$CLOSURE_REPORT" ]; then
        return 0
    fi

    local violation_count
    violation_count=$(jq -r '.count // 0' "$CLOSURE_REPORT" 2>/dev/null || echo "0")

    if [ "$violation_count" -gt 0 ]; then
        if ! has_open_bead_for "closure.*lint\|lint.*closure\|closure.*violat"; then
            add_drift "closure-lint" "Gate has $violation_count violations but no open bead tracking them"
        fi
    fi
}

# Gate: test failures (checks for test-related open beads when tests last failed)
check_test_drift() {
    if [ -n "$CHECK_GATE" ] && [ "$CHECK_GATE" != "cargo-test" ]; then
        return 0
    fi

    # Quick check: does .vision-coverage-report.json indicate test issues?
    if [ -f ".vision-coverage-report.json" ]; then
        local missing_count
        missing_count=$(jq -r '.surfaces | to_entries | map(select(.value.status == "missing")) | length' .vision-coverage-report.json 2>/dev/null || echo "0")
        if [ "$missing_count" -gt 5 ]; then
            if ! has_open_bead_for "test.*fail\|fail.*test\|walking.*skeleton\|core.*job"; then
                add_drift "cargo-test" "Vision coverage shows $missing_count missing surfaces but no open bead tracking core functionality gaps"
            fi
        fi
    fi
}

# Gate: forbidden dependencies
check_forbidden_deps_drift() {
    if [ -n "$CHECK_GATE" ] && [ "$CHECK_GATE" != "forbidden-deps" ]; then
        return 0
    fi

    # Quick check via cargo tree
    local forbidden_hits
    forbidden_hits=$(cargo tree -e features 2>/dev/null | grep -cE '(^|\s)(tokio|rusqlite|petgraph|sqlx|diesel)\s' || echo "0")

    if [ "$forbidden_hits" -gt 0 ]; then
        if ! has_open_bead_for "forbidden.*dep\|dep.*forbidden\|tokio\|rusqlite\|petgraph"; then
            add_drift "forbidden-deps" "Found $forbidden_hits forbidden dependency references but no open bead"
        fi
    fi
}

# Main
if [ "$JSON_OUTPUT" != true ]; then
    echo "=== Verification Drift Guard ==="
    echo ""
fi

check_closure_lint_drift
check_test_drift
# Skip forbidden-deps in normal runs (cargo tree is slow); enable via --gate=forbidden-deps
if [ "$CHECK_GATE" = "forbidden-deps" ]; then
    check_forbidden_deps_drift
fi

if [ "$DRIFT_COUNT" -gt 0 ]; then
    if [ "$JSON_OUTPUT" != true ]; then
        echo ""
        echo "Found $DRIFT_COUNT drift violation(s)"
        echo ""
        echo "Fix: Create an open bead for each red gate, or resolve the underlying issue."
        echo "Example: br create --title \"[verify] Fix closure-lint violations\" --priority 1"
    fi

    if [ "$JSON_OUTPUT" = true ]; then
        write_drift_report "fail"
        echo "Report written to $REPORT_FILE"
    fi

    exit 1
else
    if [ "$JSON_OUTPUT" != true ]; then
        echo "No drift detected - all red gates have tracking beads."
    fi

    if [ "$JSON_OUTPUT" = true ]; then
        write_drift_report "pass"
    fi

    exit 0
fi
