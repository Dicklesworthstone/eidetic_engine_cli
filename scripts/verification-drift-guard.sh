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

# Exit code for "the guard could not evaluate its own input". Distinct from
# 1 (drift detected) so a caller can tell a verdict from a broken instrument.
GUARD_INPUT_ERROR_CODE=2

# A gate that cannot read its input must FAIL, never report clean.
#
# bd-ry56h. This is not a stylistic preference. scripts/verify.sh:958 treats
# THIS SCRIPT EXITING 0 as grounds to excuse a failing closure linter:
#
#     if with_beads_read_locks ./scripts/verification-drift-guard.sh \
#            --gate=closure-lint --json; then
#         echo "[!] Closure linter reported tracked violations; continuing ..."
#         return 0
#
# and the comment directly above it already states the intent -- "If the guard
# itself cannot run, the excuse is not established, so the linter's own
# failure stands." A swallowed jq error that defaults to 0 makes this script
# exit 0 while having checked nothing, which converts a red gate into a pass.
guard_input_error() {
    printf '[x] verification-drift-guard: %s\n' "$1" >&2
    printf '    Refusing to report a verdict from an unreadable input.\n' >&2
    exit "$GUARD_INPUT_ERROR_CODE"
}

# Read one non-negative integer out of a JSON file.
#
# Sets JSON_METRIC. Deliberately NOT a command substitution: `x=$(f)` runs f
# in a subshell, where `exit` would terminate only that subshell and let the
# caller continue with an empty value -- reintroducing the very fail-open this
# function exists to remove.
JSON_METRIC=""
read_json_metric() {
    local file="$1" query="$2" label="$3" output=""
    if ! output=$(jq -r "$query" "$file" 2>&1); then
        guard_input_error "$label: jq failed on $file: $output"
    fi
    case "$output" in
        '' | *[!0-9]*)
            guard_input_error \
                "$label: expected a non-negative integer from $file, got: $output"
            ;;
    esac
    JSON_METRIC="$output"
}

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

    # This is the instance verify.sh:958 leans on for its closure-lint
    # excuse, so a swallowed error here does not merely skip a check -- it
    # forgives a red linter. Fail closed (bd-ry56h).
    local violation_count
    read_json_metric "$CLOSURE_REPORT" '.count // 0' "closure-lint"
    violation_count="$JSON_METRIC"

    if [ "$violation_count" -gt 0 ]; then
        if ! has_open_bead_for "closure.*lint|lint.*closure|closure.*violat"; then
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
        # bd-ry56h. The old query was `.surfaces | to_entries |
        # map(select(.value.status == "missing")) | length`, which treats
        # `.surfaces` as a map of per-surface objects. It is not: it is a
        # COUNTS object, `{"total_documented":141,"implemented":141,
        # "stubbed":0,"missing":0,"with_open_implements_bead":0}`. jq exits 5
        # with `Cannot index number with string "status"`, the `|| echo "0"`
        # swallowed it, and 0 never exceeded the threshold -- so this gate had
        # been reporting clean by failing. The value was always one hop away.
        local missing_count
        read_json_metric .vision-coverage-report.json '.surfaces.missing' "vision-coverage"
        missing_count="$JSON_METRIC"
        if [ "$missing_count" -gt 5 ]; then
            if ! has_open_bead_for "test.*fail|fail.*test|walking.*skeleton|core.*job"; then
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

    # Quick check via cargo tree.
    #
    # bd-ry56h, third instance of the same shape. The old line was
    # `cargo tree 2>/dev/null | grep -c ... || echo "0"`, which conflates two
    # very different zeros: grep finding no forbidden dependency (a real
    # verdict) and cargo tree failing so grep read an empty stream (no
    # verdict at all). Only the first may be reported as clean, so the two
    # are now separated -- cargo's own exit status is checked before grep
    # runs, and grep's exit 1 for "no match" stays a legitimate zero.
    local forbidden_hits tree_output=""
    if ! tree_output=$(cargo tree -e features 2>&1); then
        guard_input_error "forbidden-deps: cargo tree failed: $(printf '%s' "$tree_output" | tail -n 3)"
    fi
    forbidden_hits=$(printf '%s\n' "$tree_output" |
        grep -cE '(^|[[:space:]])(tokio|rusqlite|petgraph|sqlx|diesel)[[:space:]]' || true)
    [ -n "$forbidden_hits" ] || forbidden_hits=0

    if [ "$forbidden_hits" -gt 0 ]; then
        if ! has_open_bead_for "forbidden.*dep|dep.*forbidden|tokio|rusqlite|petgraph"; then
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
