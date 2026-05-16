#!/usr/bin/env bash
# G4.d - Structural health graph e2e logging harness.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
START_SECONDS="$(python3 -c 'import time; print(time.monotonic())')"
epic_setup "g4_health_structural"
seed_corpus
ee_workspace config set graph.feature.structural_health.enabled true --json >/dev/null

remember_health_fixture() {
    local content="${1:?content required}"
    ee_workspace remember --level procedural --kind rule --no-auto-link "$content" --json 2>/dev/null \
        | jq -r '.data.memory.id // .data.memory_id // .data.id // empty' 2>/dev/null
}

link_health_fixture() {
    local left="${1:?left memory id required}"
    local right="${2:?right memory id required}"
    local relation="${3:?relation required}"
    ee_workspace link "$left" "$right" --relation "$relation" --json >/dev/null 2>&1
}

seed_structural_health_fixture() {
    SUPPORT_A="$(remember_health_fixture "G4 structural health fixture: support alpha.")"
    SUPPORT_B="$(remember_health_fixture "G4 structural health fixture: support beta.")"
    SUPPORT_C="$(remember_health_fixture "G4 structural health fixture: support gamma.")"
    CONFLICT_X="$(remember_health_fixture "G4 structural health fixture: claim x.")"
    CONFLICT_Y="$(remember_health_fixture "G4 structural health fixture: claim y.")"
    CONFLICT_Z="$(remember_health_fixture "G4 structural health fixture: claim z.")"

    for memory_id in "$SUPPORT_A" "$SUPPORT_B" "$SUPPORT_C" "$CONFLICT_X" "$CONFLICT_Y" "$CONFLICT_Z"; do
        e2e_log_assert_num "${#memory_id}" -gt 0 "g4_health_structural_seed_memory_id"
    done

    link_health_fixture "$SUPPORT_A" "$SUPPORT_B" supports
    link_health_fixture "$SUPPORT_A" "$SUPPORT_C" supports
    link_health_fixture "$SUPPORT_B" "$SUPPORT_C" supports
    link_health_fixture "$CONFLICT_X" "$CONFLICT_Y" contradicts
    link_health_fixture "$CONFLICT_X" "$CONFLICT_Z" contradicts
    link_health_fixture "$CONFLICT_Y" "$CONFLICT_Z" contradicts
}

seed_structural_health_fixture

e2e_log_note "g4_health_structural_surface=health --robot-insights"
HEALTH_JSON=$(ee_workspace health --robot-insights --json 2>/dev/null || true)
if printf '%s' "$HEALTH_JSON" | jq . >/dev/null 2>&1; then
    assert_jq "$HEALTH_JSON" '.schema' "ee.health.structural.v1" "g4_health_structural_schema"
    assert_jq "$HEALTH_JSON" 'has("schema") and has("snapshotVersion") and has("kTruss") and has("contradictionClusters") and has("summary") and has("degraded")' "true" "g4_health_structural_required_fields"
    assert_jq "$HEALTH_JSON" '(.kTruss | type)' "object" "g4_health_structural_k_truss_object"
    assert_jq "$HEALTH_JSON" '(.contradictionClusters | type)' "array" "g4_health_structural_clusters_array"
    assert_jq "$HEALTH_JSON" '(.summary | type)' "object" "g4_health_structural_summary_object"
    assert_jq "$HEALTH_JSON" '(.degraded | type)' "array" "g4_health_structural_degraded_array"
    assert_jq_nonempty "$HEALTH_JSON" '.kTruss.maxK // empty' "g4_health_structural_k_truss_max_k_present"
    K_TRUSS_MAX="$(printf '%s' "$HEALTH_JSON" | jq -r '.kTruss.maxK // 0' 2>/dev/null || echo 0)"
    SUPPORT_COUNT="$(printf '%s' "$HEALTH_JSON" | jq -r '.kTruss.supportSubgraphMemoryCount // 0' 2>/dev/null || echo 0)"
    CLUSTER_COUNT="$(printf '%s' "$HEALTH_JSON" | jq -r '.summary.contradictionClusterCount // 0' 2>/dev/null || echo 0)"
    DENSE_CLUSTER_COUNT="$(printf '%s' "$HEALTH_JSON" | jq '[.contradictionClusters[]? | select((.memoryCount // 0) >= 3 and (.contradictionDensity // 0) >= 1)] | length' 2>/dev/null || echo 0)"
    e2e_log_assert_num "$K_TRUSS_MAX" -ge 3 "g4_health_structural_k_truss_triangle_detected"
    e2e_log_assert_num "$SUPPORT_COUNT" -ge 3 "g4_health_structural_support_subgraph_seeded"
    e2e_log_assert_num "$CLUSTER_COUNT" -ge 1 "g4_health_structural_cluster_count"
    e2e_log_assert_num "$DENSE_CLUSTER_COUNT" -ge 1 "g4_health_structural_incoherent_cluster_identified"
    todo_assert "g4_health_structural_golden_snapshot_refresh" "bd-zx2v.4" "Golden snapshot refresh is tracked separately from this live e2e harness."
    SNAPSHOT_VERSION=$(printf '%s' "$HEALTH_JSON" | jq -r '.. | objects | .snapshotVersion? // .snapshot_version? // empty' 2>/dev/null | head -n 1)
else
    e2e_log_assert_eq "valid-json" "invalid-json" "g4_health_structural_json_parse"
    SNAPSHOT_VERSION="unavailable"
fi

if [ "${EE_GRAPH_E2E_INJECT_FAILURE:-0}" = "1" ]; then
    e2e_log_assert_eq "actual-health" "expected-health" "g4_health_structural_injected_failure_diff" || true
fi

ELAPSED_MS="$(python3 -c "import time; print(int((time.monotonic() - float('$START_SECONDS')) * 1000))")"
e2e_log_note "g4_health_structural_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} elapsed_ms=${ELAPSED_MS} snapshot_version=${SNAPSHOT_VERSION:-unavailable}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
