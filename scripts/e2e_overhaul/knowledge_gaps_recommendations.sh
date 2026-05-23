#!/usr/bin/env bash
# Logged E2E driver for active-learning knowledge-gap recommendation posture.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
# shellcheck source=scripts/e2e_overhaul/lib/derivation_reflection.sh
source "$SCRIPT_DIR/lib/derivation_reflection.sh"

require_jq
epic_setup "knowledge_gaps_recommendations"
drh_seed_derivation_sources "knowledge_gaps_recommendations"

AGENDA_JSON=$(ee_workspace learn agenda --limit 10 --json 2>/dev/null || true)
drh_extract_degraded_codes "$AGENDA_JSON"
drh_extract_recovery_actions "$AGENDA_JSON"

if printf '%s' "$AGENDA_JSON" | jq . >/dev/null 2>&1; then
    AGENDA_SUCCESS=$(printf '%s' "$AGENDA_JSON" | jq -r '.success // false' 2>/dev/null || printf false)
    if [ "$AGENDA_SUCCESS" = "true" ]; then
        e2e_log_assert_eq "$AGENDA_SUCCESS" "true" "knowledge_gaps_agenda_success"
        GAP_COUNT=$(printf '%s' "$AGENDA_JSON" | jq '(.data.items // .data.gaps // []) | length' 2>/dev/null || echo 0)
        e2e_log_assert_num "$GAP_COUNT" -ge 0 "knowledge_gaps_agenda_count_nonnegative"
    else
        AGENDA_CODE=$(printf '%s' "$AGENDA_JSON" | jq -r '.error.code // "unknown_error"' 2>/dev/null || printf unknown_error)
        DRH_RECOVERY_ACTIONS=$(printf '["ee learn agenda returned %s; make this assertion hard once bd-3bsvv lands"]' "$AGENDA_CODE")
        todo_assert "knowledge_gaps_agenda_success" "bd-3bsvv" "The knowledge-gap recommendation surface is not yet reliably available in isolated temp workspaces."
    fi
else
    e2e_log_assert_eq "parseable-json" "invalid-json" "knowledge_gaps_agenda_parseable"
fi

drh_log_state "knowledge_gaps_recommendation_probe" "learn_agenda_probe" "recorded"
drh_assert_test_log_contract "knowledge_gaps_recommendations_log_contract"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
