#!/usr/bin/env bash
# G5.d - Structural curate-decay graph e2e logging harness.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
START_SECONDS="$(python3 -c 'import time; print(time.monotonic())')"
epic_setup "g5_curate_decay"
seed_corpus

ee_workspace config set graph.feature.structural_decay.enabled true --json >/dev/null

e2e_log_note "g5_curate_decay_surface=curate disposition structuralAdjustments"
CURATE_JSON=$(ee_workspace curate disposition --json 2>/dev/null || true)
if printf '%s' "$CURATE_JSON" | jq . >/dev/null 2>&1; then
    assert_jq "$CURATE_JSON" '.schema // empty' "ee.response.v1" "g5_curate_decay_envelope_schema"
    assert_jq "$CURATE_JSON" '.success // false' "true" "g5_curate_decay_success"
    assert_jq "$CURATE_JSON" '.data.schema // empty' "ee.curate.disposition.v1" "g5_curate_decay_data_schema"
    assert_jq "$CURATE_JSON" '.data | has("schema") and has("command") and has("version") and has("workspaceId") and has("workspacePath") and has("databasePath") and has("dryRun") and has("apply") and has("durableMutation") and has("summary") and has("policies") and has("decisions") and has("structuralAdjustments") and has("degraded") and has("nextAction")' "true" "g5_curate_decay_required_fields"
    assert_jq "$CURATE_JSON" '.data.command // empty' "curate disposition" "g5_curate_decay_command"
    assert_jq "$CURATE_JSON" '(.data.summary | type)' "object" "g5_curate_decay_summary_object"
    assert_jq "$CURATE_JSON" '(.data.policies | type)' "array" "g5_curate_decay_policies_array"
    assert_jq "$CURATE_JSON" '(.data.decisions | type)' "array" "g5_curate_decay_decisions_array"
    assert_jq "$CURATE_JSON" '.data.structuralAdjustments | type' "array" "g5_curate_decay_structural_adjustments_array"
    assert_jq "$CURATE_JSON" '(.data.degraded | type)' "array" "g5_curate_decay_degraded_array"
    STRUCTURAL_COUNT=$(printf '%s' "$CURATE_JSON" | jq '(.data.structuralAdjustments // []) | length' 2>/dev/null || echo 0)
    e2e_log_assert_num "$STRUCTURAL_COUNT" -ge 1 "g5_curate_decay_structural_adjustment_present"
    assert_jq_nonempty "$CURATE_JSON" '.data.structuralAdjustments[0].memoryId // empty' "g5_curate_decay_adjustment_memory_id"
    assert_jq_nonempty "$CURATE_JSON" '.data.structuralAdjustments[0].rationale // empty' "g5_curate_decay_adjustment_rationale"
    assert_jq_nonempty "$CURATE_JSON" '.data.structuralAdjustments[0].adjustedDecay // empty' "g5_curate_decay_adjusted_decay"
    SNAPSHOT_VERSION=$(printf '%s' "$CURATE_JSON" | jq -r '.. | objects | .snapshotVersion? // .snapshot_version? // empty' 2>/dev/null | head -n 1)

    CURATE_NO_STRUCTURAL_JSON=$(ee_workspace curate disposition --json --no-structural-decay 2>/dev/null || true)
    if printf '%s' "$CURATE_NO_STRUCTURAL_JSON" | jq . >/dev/null 2>&1; then
        assert_jq "$CURATE_NO_STRUCTURAL_JSON" '.schema // empty' "ee.response.v1" "g5_curate_decay_opt_out_envelope_schema"
        assert_jq "$CURATE_NO_STRUCTURAL_JSON" '.success // false' "true" "g5_curate_decay_opt_out_success"
        assert_jq "$CURATE_NO_STRUCTURAL_JSON" '.data.command // empty' "curate disposition" "g5_curate_decay_opt_out_command"
        assert_jq "$CURATE_NO_STRUCTURAL_JSON" '.data.structuralAdjustments | type' "array" "g5_curate_decay_opt_out_structural_adjustments_array"
        STRUCTURAL_OPT_OUT_COUNT=$(printf '%s' "$CURATE_NO_STRUCTURAL_JSON" | jq '(.data.structuralAdjustments // []) | length' 2>/dev/null || echo 0)
        e2e_log_assert_num "$STRUCTURAL_OPT_OUT_COUNT" -eq 0 "g5_curate_decay_opt_out_structural_adjustments_absent"
    else
        e2e_log_assert_eq "parseable-json" "invalid-json" "g5_curate_decay_opt_out_parseable_json" || true
    fi
else
    todo_assert "g5_curate_decay_surface_available" "bd-mvld.4" "ee curate disposition structural decay output is not fully available yet."
    SNAPSHOT_VERSION="unavailable"
fi

if [ "${EE_GRAPH_E2E_INJECT_FAILURE:-0}" = "1" ]; then
    e2e_log_assert_eq "actual-curate-decay" "expected-curate-decay" "g5_curate_decay_injected_failure_diff" || true
fi

ELAPSED_MS="$(python3 -c "import time; print(int((time.monotonic() - float('$START_SECONDS')) * 1000))")"
e2e_log_note "g5_curate_decay_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} elapsed_ms=${ELAPSED_MS} snapshot_version=${SNAPSHOT_VERSION:-unavailable}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
