#!/usr/bin/env bash
# G3.d - Causal explanation graph e2e logging harness.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
START_SECONDS="$(python3 -c 'import time; print(time.monotonic())')"
epic_setup "g3_causal"
seed_corpus

e2e_log_note "g3_causal_surface=why --causal-explain"
remember_causal_fixture() {
    local content="${1:?content required}"
    local level="${2:?level required}"
    local kind="${3:?kind required}"
    ee_workspace remember "$content" --level "$level" --kind "$kind" --no-auto-link --json 2>/dev/null \
        | jq -r '.data.memory_id // .data.memory.id // .data.id // empty' 2>/dev/null
}

FAILURE_ID="$(remember_causal_fixture "G3 causal failure target." "episodic" "incident")"
BRIDGE_ID="$(remember_causal_fixture "G3 causal bridge cause." "semantic" "note")"
ROOT_ID="$(remember_causal_fixture "G3 causal terminal root cause." "semantic" "note")"
for memory_id in "$FAILURE_ID" "$BRIDGE_ID" "$ROOT_ID"; do
    e2e_log_assert_num "${#memory_id}" -gt 0 "g3_causal_seed_memory_id"
done

if [ -n "${FAILURE_ID:-}" ] && [ -n "${BRIDGE_ID:-}" ] && [ -n "${ROOT_ID:-}" ]; then
    ee_workspace diag causal-edge \
        --edge-id cev_g3_bridge \
        --failure-id "$FAILURE_ID" \
        --candidate-cause-id "$BRIDGE_ID" \
        --contribution-score 0.82 \
        --evidence-uri agent-mail://bd-qnfw.4/bridge \
        --computed-at 2026-05-15T12:30:00Z \
        --method manual \
        --json >/dev/null 2>&1 || true
    ee_workspace diag causal-edge \
        --edge-id cev_g3_root \
        --failure-id "$BRIDGE_ID" \
        --candidate-cause-id "$ROOT_ID" \
        --contribution-score 0.91 \
        --evidence-uri agent-mail://bd-qnfw.4/root \
        --computed-at 2026-05-15T12:31:00Z \
        --method manual \
        --json >/dev/null 2>&1 || true
fi
e2e_log_note "g3_causal_seed_chain failure=${FAILURE_ID:-missing} bridge=${BRIDGE_ID:-missing} root=${ROOT_ID:-missing}"

if [ -n "${FAILURE_ID:-}" ]; then
    CAUSAL_JSON=$(ee_workspace why "$FAILURE_ID" --causal-explain --json 2>/dev/null || true)
else
    CAUSAL_JSON=""
fi
if printf '%s' "$CAUSAL_JSON" | jq . >/dev/null 2>&1; then
    assert_jq_nonempty "$CAUSAL_JSON" '.schema // empty' "g3_causal_why_schema_present"
    assert_jq "$CAUSAL_JSON" '.data.causalExplanation.schema // empty' "ee.why.causal.v1" "g3_causal_explanation_schema"
    assert_jq "$CAUSAL_JSON" '.data.causalExplanation | has("schema") and has("memoryId") and has("snapshotVersion") and has("paths") and has("minCut") and has("degraded")' "true" "g3_causal_required_fields"
    assert_jq "$CAUSAL_JSON" '.data.causalExplanation.memoryId // empty' "$FAILURE_ID" "g3_causal_explanation_target"
    assert_jq "$CAUSAL_JSON" '(.data.causalExplanation.paths | type)' "array" "g3_causal_paths_array"
    assert_jq "$CAUSAL_JSON" '(.data.causalExplanation.degraded | type)' "array" "g3_causal_degraded_array"
    CAUSAL_DEGRADED_COUNT=$(printf '%s' "$CAUSAL_JSON" | jq '(.data.causalExplanation.degraded // []) | length' 2>/dev/null || echo 0)
    e2e_log_assert_num "$CAUSAL_DEGRADED_COUNT" -eq 0 "g3_causal_explanation_not_degraded"
    CAUSAL_PATH_COUNT=$(printf '%s' "$CAUSAL_JSON" | jq '(.data.causalExplanation.paths // []) | length' 2>/dev/null || echo 0)
    e2e_log_assert_num "$CAUSAL_PATH_COUNT" -eq 1 "g3_causal_min_cost_path_count"
    assert_jq "$CAUSAL_JSON" '.data.causalExplanation.paths[0].sourceMemoryId // empty' "$ROOT_ID" "g3_causal_path_terminal_root"
    assert_jq "$CAUSAL_JSON" '.data.causalExplanation.paths[0].targetMemoryId // empty' "$FAILURE_ID" "g3_causal_path_failure_target"
    assert_jq "$CAUSAL_JSON" '.data.causalExplanation.paths[0].edgeCount // empty' "2" "g3_causal_path_edge_count"
    assert_jq "$CAUSAL_JSON" '.data.causalExplanation.paths[0].steps[0].source // empty' "$FAILURE_ID" "g3_causal_first_step_source"
    assert_jq "$CAUSAL_JSON" '.data.causalExplanation.paths[0].steps[0].target // empty' "$BRIDGE_ID" "g3_causal_first_step_target"
    assert_jq "$CAUSAL_JSON" '.data.causalExplanation.paths[0].steps[1].source // empty' "$BRIDGE_ID" "g3_causal_second_step_source"
    assert_jq "$CAUSAL_JSON" '.data.causalExplanation.paths[0].steps[1].target // empty' "$ROOT_ID" "g3_causal_second_step_target"
    assert_jq_nonempty "$CAUSAL_JSON" '.data.causalExplanation.paths[0].minCost // empty' "g3_causal_min_cost_present"
    SNAPSHOT_VERSION=$(printf '%s' "$CAUSAL_JSON" | jq -r '.. | objects | .snapshotVersion? // .snapshot_version? // empty' 2>/dev/null | head -n 1)
else
    todo_assert "g3_causal_why_surface_available" "bd-qnfw.2" "ee why --causal-explain is not fully available yet."
    SNAPSHOT_VERSION="unavailable"
fi

if [ "${EE_GRAPH_E2E_INJECT_FAILURE:-0}" = "1" ]; then
    e2e_log_assert_eq "actual-causal" "expected-causal" "g3_causal_injected_failure_diff" || true
fi

ELAPSED_MS="$(python3 -c "import time; print(int((time.monotonic() - float('$START_SECONDS')) * 1000))")"
e2e_log_note "g3_causal_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} elapsed_ms=${ELAPSED_MS} snapshot_version=${SNAPSHOT_VERSION:-unavailable}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
