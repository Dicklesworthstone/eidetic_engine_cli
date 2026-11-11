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
#   ./scripts/verification-drift-guard.sh --self-test    # Run deterministic self-test arms
#
# Gates tracked:
#   - closure-lint: closure discipline violations
#   - cargo-test: test suite failures
#   - forbidden-deps: banned dependency violations
#
# Exit codes: 0=pass (all red gates have beads), 1=drift detected, 2=input error

set -eu

BEADS_FILE=".beads/issues.jsonl"
REPORT_FILE=".verification-drift-report.json"
CLOSURE_REPORT=".closure-lint-report.json"

JSON_OUTPUT=false
CHECK_GATE=""
SELF_TEST=false

for arg in "$@"; do
    case "$arg" in
        --json) JSON_OUTPUT=true ;;
        --gate=*) CHECK_GATE="${arg#--gate=}" ;;
        --self-test) SELF_TEST=true ;;
        --help|-h)
            sed -n '2,17p' "$0" | sed 's/^# //' | sed 's/^#//'
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

# Check if an open bead exists matching keywords in title or labels.
#
# bd-reality-core-convergence-1azkt.73: deliberately excludes `.description`
# prose so an unrelated bead that mentions a gate name in passing (for example
# "run closure lint and drift guard") cannot become a blanket excuse for a
# failing gate.
has_open_bead_for() {
    local keywords="$1"
    if [ ! -f "$BEADS_FILE" ]; then
        return 1
    fi

    jq -e --arg kw "$keywords" '
        select(.status != "closed")
        | select(
            ((.title // "") | ascii_downcase | test($kw | ascii_downcase))
            or ((.labels // []) | any(. | ascii_downcase | test($kw | ascii_downcase)))
          )
    ' "$BEADS_FILE" >/dev/null 2>&1
}

# Check if an open closure-lint tracking bead explicitly tracks a specific
# violating bead ID or surface (bd-reality-core-convergence-1azkt.73).
has_open_closure_lint_bead_for_violation() {
    local target_bead="$1"
    local target_surface="$2"
    if [ ! -f "$BEADS_FILE" ]; then
        return 1
    fi

    jq -e \
        --arg kw "closure[-_ ]?lint|lint.*closure|closure.*violat" \
        --arg target_bead "$target_bead" \
        --arg target_surface "$target_surface" '
        select(.status != "closed")
        | select(
            ((.title // "") | ascii_downcase | test($kw))
            or ((.labels // []) | any(. | ascii_downcase | test($kw)))
          )
        | select(
            (
                ($target_bead != "")
                and (
                    ((.title // "") | contains($target_bead))
                    or ((.description // "") | contains($target_bead))
                    or ((.acceptance_criteria // "") | contains($target_bead))
                    or ((.notes // "") | contains($target_bead))
                )
            )
            or (
                ($target_surface != "")
                and (
                    ((.title // "") | contains($target_surface))
                    or ((.description // "") | contains($target_surface))
                    or ((.acceptance_criteria // "") | contains($target_surface))
                    or ((.notes // "") | contains($target_surface))
                )
            )
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
        local array_len
        read_json_metric "$CLOSURE_REPORT" '(.violations // []) | length' "closure-lint.violations"
        array_len="$JSON_METRIC"
        if [ "$array_len" -lt "$violation_count" ]; then
            add_drift "closure-lint" "Gate reports count=$violation_count but .violations array holds only $array_len entry/entries"
            return 0
        fi

        local untracked_list=""
        local untracked_count=0
        local row=""
        while IFS= read -r row; do
            [ -n "$row" ] || continue
            local v_bead="" v_surface=""
            v_bead=$(printf '%s' "$row" | jq -r '.bead // ""')
            v_surface=$(printf '%s' "$row" | jq -r '.surface // ""')
            if [ -z "$v_bead" ] && [ -z "$v_surface" ]; then
                untracked_count=$((untracked_count + 1))
                untracked_list="${untracked_list:+${untracked_list}, }<anonymous-violation>"
                continue
            fi
            if ! has_open_closure_lint_bead_for_violation "$v_bead" "$v_surface"; then
                untracked_count=$((untracked_count + 1))
                local label="${v_bead:-$v_surface}"
                if [ -n "$v_bead" ] && [ -n "$v_surface" ]; then
                    label="${v_bead} (${v_surface})"
                fi
                untracked_list="${untracked_list:+${untracked_list}, }${label}"
            fi
        done <<EOF
$(jq -c '(.violations // [])[]' "$CLOSURE_REPORT")
EOF

        if [ "$untracked_count" -gt 0 ]; then
            add_drift "closure-lint" "Gate has $untracked_count untracked violation(s) of $violation_count total (${untracked_list}) with no open closure-lint bead naming them"
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

run_self_test() {
    local script_path
    script_path="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"
    local tmp_base="${TMPDIR:-/tmp}"
    if [ -d /Volumes/USBNVME16TB/temp_agent_space ]; then
        tmp_base="/Volumes/USBNVME16TB/temp_agent_space"
    fi
    local scratch
    scratch=$(mktemp -d "${tmp_base%/}/ee-drift-guard-selftest.XXXXXX")
    trap 'rm -rf "$scratch"' EXIT

    local total=0
    local failed=0
    arm() {
        local label="$1"
        local ok="$2"
        total=$((total + 1))
        if [ "$ok" = "true" ]; then
            printf '  [ok  ] %s\n' "$label"
        else
            printf '  [FAIL] %s\n' "$label" >&2
            failed=$((failed + 1))
        fi
    }

    # Arm 1 (Positive): open closure-lint tracking bead that explicitly names the violating bead passes.
    mkdir -p "$scratch/a1/.beads"
    cat >"$scratch/a1/.closure-lint-report.json" <<'EOF'
{"violations":[{"bead":"closed-demo","label":"implements-surface","surface":"demo","reason":"missing tests/golden/demo.snap"}],"count":1,"status":"fail"}
EOF
    cat >"$scratch/a1/.beads/issues.jsonl" <<'EOF'
{"id":"bd-track","title":"closure-lint verifier blocker","status":"open","labels":["verify"],"description":"Tracks closure linter violations on closed-demo until remediation lands."}
EOF
    local rc=0
    (cd "$scratch/a1" && "$script_path" --gate=closure-lint --json >/dev/null 2>&1) || rc=$?
    local status_a1=""
    status_a1=$(jq -r '.status // ""' "$scratch/a1/.verification-drift-report.json" 2>/dev/null || true)
    if [ "$rc" -eq 0 ] && [ "$status_a1" = "pass" ]; then
        arm "tracked closure-lint violation with explicit bead reference exits 0 (pass)" "true"
    else
        arm "tracked closure-lint violation with explicit bead reference exits 0 (pass) (got rc=$rc status=$status_a1)" "false"
    fi

    # Arm 2 (Negative Twin): unrelated open bead mentioning 'closure lint' only in description is REFUSED.
    mkdir -p "$scratch/a2/.beads"
    cp "$scratch/a1/.closure-lint-report.json" "$scratch/a2/.closure-lint-report.json"
    cat >"$scratch/a2/.beads/issues.jsonl" <<'EOF'
{"id":"bd-unrelated","title":"Unify readiness across status and doctor","status":"open","labels":["status"],"description":"Verification gates: run closure lint and drift guard on closed-demo."}
EOF
    rc=0
    (cd "$scratch/a2" && "$script_path" --gate=closure-lint --json >/dev/null 2>&1) || rc=$?
    local status_a2=""
    status_a2=$(jq -r '.status // ""' "$scratch/a2/.verification-drift-report.json" 2>/dev/null || true)
    if [ "$rc" -eq 1 ] && [ "$status_a2" = "fail" ]; then
        arm "TWIN: unrelated open bead mentioning 'closure lint' only in description exits 1 (fail)" "true"
    else
        arm "TWIN: unrelated open bead mentioning 'closure lint' only in description exits 1 (fail) (got rc=$rc status=$status_a2)" "false"
    fi

    # Arm 3 (Negative Twin): open closure-lint bead tracking bead-A does NOT excuse untracked bead-B.
    mkdir -p "$scratch/a3/.beads"
    cat >"$scratch/a3/.closure-lint-report.json" <<'EOF'
{"violations":[{"bead":"closed-demo","label":"implements-surface","surface":"demo","reason":"missing demo.snap"},{"bead":"closed-untracked","label":"implements-surface","surface":"untracked-surf","reason":"missing untracked-surf.snap"}],"count":2,"status":"fail"}
EOF
    cp "$scratch/a1/.beads/issues.jsonl" "$scratch/a3/.beads/issues.jsonl"
    rc=0
    (cd "$scratch/a3" && "$script_path" --gate=closure-lint --json >/dev/null 2>&1) || rc=$?
    local reason_a3=""
    reason_a3=$(jq -r '.driftViolations[0].reason // ""' "$scratch/a3/.verification-drift-report.json" 2>/dev/null || true)
    if [ "$rc" -eq 1 ] && printf '%s' "$reason_a3" | grep -q 'closed-untracked'; then
        arm "TWIN: open closure-lint bead for closed-demo does not excuse closed-untracked" "true"
    else
        arm "TWIN: open closure-lint bead for closed-demo does not excuse closed-untracked (got rc=$rc reason=$reason_a3)" "false"
    fi

    # Arm 4 (Negative Twin): count > 0 with empty .violations array fails closed.
    mkdir -p "$scratch/a4/.beads"
    cat >"$scratch/a4/.closure-lint-report.json" <<'EOF'
{"violations":[],"count":1,"status":"fail"}
EOF
    cp "$scratch/a1/.beads/issues.jsonl" "$scratch/a4/.beads/issues.jsonl"
    rc=0
    (cd "$scratch/a4" && "$script_path" --gate=closure-lint --json >/dev/null 2>&1) || rc=$?
    if [ "$rc" -eq 1 ]; then
        arm "TWIN: count=1 with empty .violations array fails closed (rc=1)" "true"
    else
        arm "TWIN: count=1 with empty .violations array fails closed (got rc=$rc)" "false"
    fi

    # Arm 5 (Input Error Twin): unknown --gate name exits 2 without writing a report.
    mkdir -p "$scratch/a5"
    rc=0
    (cd "$scratch/a5" && "$script_path" --gate=closure_lint --json >/dev/null 2>&1) || rc=$?
    if [ "$rc" -eq 2 ] && [ ! -f "$scratch/a5/.verification-drift-report.json" ]; then
        arm "TWIN: unknown --gate=closure_lint exits 2 and writes no verdict report" "true"
    else
        arm "TWIN: unknown --gate=closure_lint exits 2 and writes no verdict report (got rc=$rc)" "false"
    fi

    echo "[verification-drift-guard] self-test: $((total - failed))/$total arms passed"
    if [ "$failed" -gt 0 ]; then
        exit 1
    fi
    exit 0
}

if [ "$SELF_TEST" = true ]; then
    run_self_test
fi

# Main
if [ "$JSON_OUTPUT" != true ]; then
    echo "=== Verification Drift Guard ==="
    echo ""
fi

# An UNRECOGNIZED --gate name must fail, not report clean.
#
# bd-ry56h one keystroke further out. Every check is written as "return early
# unless CHECK_GATE is mine", so a name matching no check leaves DRIFT_COUNT at
# 0 and exits 0 -- and verify.sh's closure_lint_or_tracked_drift reads exactly
# that 0 as "the closure linter's violations are tracked, continue". So
# `--gate=closure_lint` with an underscore, or a gate later renamed in one file
# and not the other, would silently excuse every closure violation forever,
# with no output to notice it by.
#
# Measured before this check existed: `--gate=this-gate-does-not-exist --json`
# exited 0 having written zero bytes -- byte-identical to the answer for a
# genuinely tracked gate, which is the property that made it dangerous.
case "$CHECK_GATE" in
    "" | closure-lint | cargo-test | forbidden-deps) ;;
    *)
        guard_input_error \
            "unknown --gate=$CHECK_GATE; known gates are closure-lint, cargo-test, forbidden-deps"
        ;;
esac

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
