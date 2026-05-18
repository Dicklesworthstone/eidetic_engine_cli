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

seed_structural_decay_fixture() {
    local bridge_json core_b_json core_c_json leaf_json
    local bridge_id core_b_id core_c_id leaf_id

    bridge_json=$(ee_workspace remember "G5 structural decay bridge memory protects connected release knowledge." --level procedural --kind rule --confidence 0.91 --json)
    core_b_json=$(ee_workspace remember "G5 structural decay core B memory supports bridge topology." --level episodic --kind observation --confidence 0.86 --json)
    core_c_json=$(ee_workspace remember "G5 structural decay core C memory supports bridge topology." --level episodic --kind observation --confidence 0.86 --json)
    leaf_json=$(ee_workspace remember "G5 structural decay leaf memory can decay faster than bridge knowledge." --level episodic --kind observation --confidence 0.84 --json)

    bridge_id=$(printf '%s' "$bridge_json" | jq -r '.data.memory_id // .data.memoryId // empty')
    core_b_id=$(printf '%s' "$core_b_json" | jq -r '.data.memory_id // .data.memoryId // empty')
    core_c_id=$(printf '%s' "$core_c_json" | jq -r '.data.memory_id // .data.memoryId // empty')
    leaf_id=$(printf '%s' "$leaf_json" | jq -r '.data.memory_id // .data.memoryId // empty')

    if [ -z "$bridge_id" ] || [ -z "$core_b_id" ] || [ -z "$core_c_id" ] || [ -z "$leaf_id" ]; then
        e2e_log_assert_eq "memory-ids" "missing" "g5_curate_decay_fixture_memory_ids" || true
        return 1
    fi

    ee_workspace memory link "$bridge_id" "$core_b_id" --relation related --undirected --source agent --json >/dev/null
    ee_workspace memory link "$core_b_id" "$core_c_id" --relation related --undirected --source agent --json >/dev/null
    ee_workspace memory link "$bridge_id" "$core_c_id" --relation related --undirected --source agent --json >/dev/null
    ee_workspace memory link "$bridge_id" "$leaf_id" --relation related --undirected --source agent --json >/dev/null

    ee_workspace diag curation-candidate \
        --candidate-id curate_g5structuralbridge00000000 \
        --candidate-type promote \
        --target-memory-id "$bridge_id" \
        --source-type rule_engine \
        --source-id g5_structural_decay_e2e \
        --reason "G5 e2e bridge candidate for structural decay adjustment." \
        --created-at 2026-05-02T00:00:00Z \
        --review-state new \
        --state-entered-at 2026-05-02T00:00:00Z \
        --json >/dev/null
    ee_workspace diag curation-candidate \
        --candidate-id curate_g5structuralleaf0000000000 \
        --candidate-type promote \
        --target-memory-id "$leaf_id" \
        --source-type rule_engine \
        --source-id g5_structural_decay_e2e \
        --reason "G5 e2e leaf candidate for structural decay adjustment." \
        --created-at 2026-05-02T00:00:00Z \
        --review-state new \
        --state-entered-at 2026-05-02T00:00:00Z \
        --json >/dev/null

    e2e_log_note "g5_curate_decay_fixture bridge=${bridge_id} leaf=${leaf_id}"
}

seed_structural_decay_fixture

ee_workspace config set graph.feature.structural_decay.enabled true --json >/dev/null
CONFIG_JSON=$(ee_workspace config get graph.feature.structural_decay.enabled --json)
assert_jq "$CONFIG_JSON" '.data.value // empty' "true" "g5_curate_decay_config_enabled"

e2e_log_note "g5_curate_decay_surface=curate disposition structuralAdjustments"
CURATE_JSON=$(ee_workspace curate disposition --json 2>/dev/null || true)
DEFAULT_PROBE="unavailable"
if printf '%s' "$CURATE_JSON" | jq . >/dev/null 2>&1; then
    DEFAULT_PROBE=$(printf '%s' "$CURATE_JSON" | jq -c '{decisionCount: ((.data.decisions // []) | length), structuralCount: ((.data.structuralAdjustments // []) | length), degradedCodes: [(.data.degraded // [])[]?.code]}' 2>/dev/null || echo "unavailable")
    e2e_log_note "g5_curate_decay_default_probe=${DEFAULT_PROBE}"
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
    assert_jq_nonempty "$CURATE_JSON" '(.data.structuralAdjustments[] | select(.candidateId == "curate_g5structuralbridge00000000") | .memoryId) // empty' "g5_curate_decay_adjustment_memory_id"
    assert_jq_nonempty "$CURATE_JSON" '(.data.structuralAdjustments[] | select(.candidateId == "curate_g5structuralbridge00000000") | .rationale) // empty' "g5_curate_decay_adjustment_rationale"
    assert_jq_nonempty "$CURATE_JSON" '(.data.structuralAdjustments[] | select(.candidateId == "curate_g5structuralbridge00000000") | .adjustedDecay) // empty' "g5_curate_decay_adjusted_decay"
    assert_jq "$CURATE_JSON" '(.data.structuralAdjustments[] | select(.candidateId == "curate_g5structuralbridge00000000") | .isArticulationPoint) // false' "true" "g5_curate_decay_bridge_articulation"
    assert_jq "$CURATE_JSON" '[(.data.structuralAdjustments[] | select(.candidateId == "curate_g5structuralbridge00000000") | .structuralMultiplier < 1.0)] | any' "true" "g5_curate_decay_bridge_protected"
    SNAPSHOT_VERSION=$(printf '%s' "$CURATE_JSON" | jq -r '.. | objects | .snapshotVersion? // .snapshot_version? // empty' 2>/dev/null | head -n 1)

    CURATE_NO_STRUCTURAL_JSON=$(ee_workspace curate disposition --json --no-structural-decay 2>/dev/null || true)
    if printf '%s' "$CURATE_NO_STRUCTURAL_JSON" | jq . >/dev/null 2>&1; then
        assert_jq "$CURATE_NO_STRUCTURAL_JSON" '.schema // empty' "ee.response.v1" "g5_curate_decay_opt_out_envelope_schema"
        assert_jq "$CURATE_NO_STRUCTURAL_JSON" '.success // false' "true" "g5_curate_decay_opt_out_success"
        assert_jq "$CURATE_NO_STRUCTURAL_JSON" '.data.command // empty' "curate disposition" "g5_curate_decay_opt_out_command"
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
e2e_log_note "g5_curate_decay_final_probe=${DEFAULT_PROBE}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
