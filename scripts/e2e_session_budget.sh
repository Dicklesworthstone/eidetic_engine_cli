#!/usr/bin/env bash
# bd-1clqr.4 - Session-budget planner real-binary E2E.
#
# Seeds an opt-in session-budget ledger from redaction-safe fixture rows and
# proves the public planner recommendation before and after a costly pack row,
# plus the local-Cargo refusal path when RCH is degraded.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_lib.sh
# shellcheck disable=SC1091
source "$E2E_DIR/e2e_lib.sh"

export EE_E2E_KEEP_ARTIFACTS="${EE_E2E_KEEP_ARTIFACTS:-1}"

harness_init "session_budget"

FIXTURE_DIR="$REPO_ROOT/tests/fixtures/session_budget"
LEDGER="$LOG_DIR/session-budget-ledger.jsonl"
: >"$LEDGER"

json_value() {
    local json="$1" filter="$2"
    printf '%s' "$json" | jq -r "$filter" 2>/dev/null || true
}

append_fixture() {
    local name="$1"
    jq -c . "$FIXTURE_DIR/${name}.json" >>"$LEDGER"
    e2e_log_note "session_budget_ledger_append fixture=$name ledger=$LEDGER"
}

run_plan() {
    local label="$1"
    shift
    LAST_STDOUT_FILE="$LOG_DIR/${label}.stdout.json"
    if e2e_log_command "$EE_BIN" --json session-budget plan \
        --ledger-path "$LEDGER" \
        --workspace-fingerprint d37f1e828e51 \
        "$@" >"$LAST_STDOUT_FILE"
    then
        LAST_RC=0
    else
        LAST_RC=$?
    fi
    LAST_JSON="$(cat "$LAST_STDOUT_FILE")"
    e2e_log_note "command_label=$label rc=$LAST_RC stdout=$LAST_STDOUT_FILE"
}

assert_success() {
    local label="$1"
    assert_eq "${LAST_RC:-999}" "0" "$label exits zero" || true
    assert_jq "$LAST_JSON" '.success == true and .data.schema == "ee.session_budget.plan.v1"' \
        "$label emits plan schema"
}

step "plan after cheap recall row"
append_fixture cheap_recall
run_plan "01-plan-cheap-recall" --task-hint "prepare release"
assert_success "cheap recall plan"
assert_jq "$LAST_JSON" '.data.recommendation.surface == "primer"' \
    "cheap recall recommends primer"
assert_jq "$LAST_JSON" '.data.ledgerSummary.rowCount == 1' \
    "cheap recall ledger count is one"
before_total="$(json_value "$LAST_JSON" '.data.ledgerSummary.totalWallClockMs')"

step "plan after costly pack row"
append_fixture large_pack
run_plan "02-plan-after-pack" --task-hint "prepare release"
assert_success "after costly pack plan"
assert_jq "$LAST_JSON" '.data.recommendation.surface == "primer"' \
    "costly pack keeps primer recommendation"
assert_jq "$LAST_JSON" '.data.ledgerSummary.rowCount == 2' \
    "after pack ledger count is two"
assert_jq "$LAST_JSON" '.data.ledgerSummary.mostRecentSurface == "pack"' \
    "after pack most recent surface is pack"
after_total="$(json_value "$LAST_JSON" '.data.ledgerSummary.totalWallClockMs')"
if [ "${after_total:-0}" -gt "${before_total:-0}" ]; then
    _harness_pass "costly pack increases total wall-clock summary"
else
    _harness_fail "costly pack did not increase total wall-clock summary"
fi

step "plan with RCH-blocked proof row and cargo hint"
append_fixture rch_blocked_proof
run_plan "03-plan-rch-blocked" \
    --degraded-sources rch \
    --task-hint "cargo test --test session_budget_plan_golden"
assert_success "rch blocked plan"
assert_jq "$LAST_JSON" '.data.refusals | length == 1' \
    "rch blocked plan refuses local cargo"
assert_jq "$LAST_JSON" '.data.refusals[0].alternative | contains("scripts/rch_verify.sh")' \
    "rch blocked plan suggests rch_verify"
assert_jq "$LAST_JSON" '.data.fallbacks[] | select(.surface == "proof-skip")' \
    "rch blocked plan retains proof-skip fallback"

harness_summary
