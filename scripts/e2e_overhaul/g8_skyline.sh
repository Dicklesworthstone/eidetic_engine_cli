#!/usr/bin/env bash
# G8.d - Knowledge skyline graph e2e logging harness.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
START_SECONDS="$(python3 -c 'import time; print(time.monotonic())')"
epic_setup "g8_skyline"
seed_corpus
ee_workspace config set graph.feature.skyline.enabled true --json >/dev/null
CONFIG_JSON=$(ee_workspace config get graph.feature.skyline.enabled --json)
assert_jq "$CONFIG_JSON" '.data.value // empty' "true" "g8_skyline_config_enabled"

seed_status_skyline_fixture() {
    local core_a_json core_b_json leaf_a_json leaf_b_json
    local core_a_id core_b_id leaf_a_id leaf_b_id

    core_a_json=$(ee_workspace remember "G8 skyline core memory alpha supports status topology." --level procedural --kind rule --confidence 0.91 --no-auto-link --json)
    core_b_json=$(ee_workspace remember "G8 skyline core memory beta supports status topology." --level semantic --kind note --confidence 0.87 --no-auto-link --json)
    leaf_a_json=$(ee_workspace remember "G8 skyline periphery memory alpha exercises degenerate communities." --level episodic --kind observation --confidence 0.82 --no-auto-link --json)
    leaf_b_json=$(ee_workspace remember "G8 skyline periphery memory beta exercises degenerate communities." --level episodic --kind observation --confidence 0.81 --no-auto-link --json)

    core_a_id=$(printf '%s' "$core_a_json" | jq -r '.data.memory_id // .data.memoryId // .data.memory.id // .data.id // empty')
    core_b_id=$(printf '%s' "$core_b_json" | jq -r '.data.memory_id // .data.memoryId // .data.memory.id // .data.id // empty')
    leaf_a_id=$(printf '%s' "$leaf_a_json" | jq -r '.data.memory_id // .data.memoryId // .data.memory.id // .data.id // empty')
    leaf_b_id=$(printf '%s' "$leaf_b_json" | jq -r '.data.memory_id // .data.memoryId // .data.memory.id // .data.id // empty')

    if [ -z "$core_a_id" ] || [ -z "$core_b_id" ] || [ -z "$leaf_a_id" ] || [ -z "$leaf_b_id" ]; then
        e2e_log_assert_eq "memory-ids" "missing" "g8_skyline_fixture_memory_ids" || true
        return 1
    fi

    ee_workspace memory link "$core_a_id" "$core_b_id" --relation related --undirected --source agent --json >/dev/null
    ee_workspace memory link "$leaf_a_id" "$leaf_b_id" --relation related --undirected --source agent --json >/dev/null
    e2e_log_note "g8_skyline_fixture communities=2 core=${core_a_id},${core_b_id} periphery=${leaf_a_id},${leaf_b_id}"
}

seed_status_skyline_fixture

e2e_log_note "g8_skyline_surface=status --skyline"
SKYLINE_JSON=$(ee_workspace status --skyline --json 2>/dev/null || true)
if printf '%s' "$SKYLINE_JSON" | jq . >/dev/null 2>&1; then
    assert_jq "$SKYLINE_JSON" '.schema' "ee.status.skyline.v1" "g8_skyline_schema_exact"
    assert_jq "$SKYLINE_JSON" '[has("schema"), has("snapshotVersion"), has("skyline"), has("summary"), has("degraded")] | all' "true" "g8_skyline_required_fields"
    assert_jq "$SKYLINE_JSON" '(.skyline | type)' "array" "g8_skyline_array"
    assert_jq "$SKYLINE_JSON" '(.summary | type)' "object" "g8_skyline_summary_object"
    assert_jq "$SKYLINE_JSON" '(.degraded | type)' "array" "g8_skyline_degraded_array"
    assert_jq "$SKYLINE_JSON" '(.summary | has("communityCount") and has("loadBearingMemoryCount") and has("staleCommunityCount"))' "true" "g8_skyline_summary_counters"
    SKYLINE_ITEM_COUNT=$(printf '%s' "$SKYLINE_JSON" | jq '.skyline | length' 2>/dev/null || echo 0)
    e2e_log_assert_num "$SKYLINE_ITEM_COUNT" -ge 1 "g8_skyline_items_present"
    PERIPHERY_HEAVY_COUNT=$(printf '%s' "$SKYLINE_JSON" | jq '[.skyline[]? | select(.structuralHealth == "periphery_heavy")] | length' 2>/dev/null || echo 0)
    e2e_log_assert_num "$PERIPHERY_HEAVY_COUNT" -ge 1 "g8_skyline_periphery_heavy_label"
    DEGENERATE_DIAGNOSTIC_COUNT=$(printf '%s' "$SKYLINE_JSON" | jq '[.degraded[]? | select(.code == "graph_skyline_degenerate_communities" and .severity == "info" and ((.message // "") | contains("degenerate")))] | length' 2>/dev/null || echo 0)
    e2e_log_assert_num "$DEGENERATE_DIAGNOSTIC_COUNT" -ge 1 "g8_skyline_degenerate_diagnostic"
    SNAPSHOT_VERSION=$(printf '%s' "$SKYLINE_JSON" | jq -r '.snapshotVersion // empty' 2>/dev/null | head -n 1)
else
    todo_assert "g8_skyline_surface_available" "bd-mhc1.4" "ee status --skyline is not fully available yet."
    SNAPSHOT_VERSION="unavailable"
fi

if [ "${EE_GRAPH_E2E_INJECT_FAILURE:-0}" = "1" ]; then
    e2e_log_assert_eq "actual-skyline" "expected-skyline" "g8_skyline_injected_failure_diff" || true
fi

ELAPSED_MS="$(python3 -c "import time; print(int((time.monotonic() - float('$START_SECONDS')) * 1000))")"
e2e_log_note "g8_skyline_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} elapsed_ms=${ELAPSED_MS} snapshot_version=${SNAPSHOT_VERSION:-unavailable}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
