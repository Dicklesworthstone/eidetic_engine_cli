#!/usr/bin/env bash
# Logged E2E driver for the source-derived candidate lifecycle.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
# shellcheck source=scripts/e2e_overhaul/lib/derivation_reflection.sh
source "$SCRIPT_DIR/lib/derivation_reflection.sh"

require_jq
epic_setup "external_derivation_lifecycle"
drh_seed_derivation_sources "external_derivation_lifecycle"

CURATE_JSON=$(ee_workspace curate candidates --json 2>/dev/null || true)
drh_extract_degraded_codes "$CURATE_JSON"
drh_extract_recovery_actions "$CURATE_JSON"
drh_capture_candidate_id "$CURATE_JSON"

if printf '%s' "$CURATE_JSON" | jq . >/dev/null 2>&1; then
    assert_jq "$CURATE_JSON" '.success // false' "true" "external_derivation_curate_candidates_success"
    DERIVED_COUNT=$(printf '%s' "$CURATE_JSON" | jq '[.data.candidates[]? | select((.candidateType // .candidate_type // .kind // "") == "create_derived_memory")] | length' 2>/dev/null || echo 0)
    if [ "$DERIVED_COUNT" -gt 0 ]; then
        e2e_log_assert_num "$DERIVED_COUNT" -ge 1 "external_derivation_candidate_visible"
    else
        todo_assert "external_derivation_candidate_visible" "bd-17bob" "No create_derived_memory candidate is expected until the external derivation producer surface lands."
    fi
else
    e2e_log_assert_eq "parseable-json" "invalid-json" "external_derivation_curate_candidates_parseable"
fi

drh_log_state "external_derivation_lifecycle_probe" "candidate_lifecycle_probe" "recorded"
drh_assert_test_log_contract "external_derivation_lifecycle_log_contract"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
