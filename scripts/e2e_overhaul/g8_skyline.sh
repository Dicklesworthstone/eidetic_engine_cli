#!/usr/bin/env bash
# G8.d - Knowledge skyline graph e2e logging harness.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
START_SECONDS="$(python3 -c 'import time; print(time.monotonic())')"
# Mirrors SKYLINE_MIN_COMMUNITY_COUNT in src/core/status.rs:97. The product
# reports degenerate communities only BELOW this count, so the suite has to
# know the threshold to assert the contract in either direction.
SKYLINE_MIN_COMMUNITY_COUNT=3
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
    assert_jq "$SKYLINE_JSON" '.schema' "ee.response.v2" "g8_skyline_response_schema"
    assert_jq "$SKYLINE_JSON" '.success // false' "true" "g8_skyline_success"
    assert_jq "$SKYLINE_JSON" '.data.schema // empty' "ee.status.skyline.v1" "g8_skyline_schema_exact"
    assert_jq "$SKYLINE_JSON" '.data.command // empty' "status --skyline" "g8_skyline_command"
    assert_jq "$SKYLINE_JSON" '(.data | has("schema") and has("snapshotVersion") and has("skyline") and has("summary") and has("degraded"))' "true" "g8_skyline_required_fields"
    assert_jq "$SKYLINE_JSON" '(.data.skyline | type)' "array" "g8_skyline_array"
    assert_jq "$SKYLINE_JSON" '(.data.summary | type)' "object" "g8_skyline_summary_object"
    assert_jq "$SKYLINE_JSON" '(.data.degraded | type)' "array" "g8_skyline_degraded_array"
    assert_jq "$SKYLINE_JSON" '(.data.summary | has("communityCount") and has("loadBearingMemoryCount") and has("staleCommunityCount"))' "true" "g8_skyline_summary_counters"
    SKYLINE_ITEM_COUNT=$(printf '%s' "$SKYLINE_JSON" | jq '.data.skyline | length' 2>/dev/null || echo 0)
    e2e_log_assert_num "$SKYLINE_ITEM_COUNT" -ge 1 "g8_skyline_items_present"
    PERIPHERY_HEAVY_COUNT=$(printf '%s' "$SKYLINE_JSON" | jq '[.data.skyline[]? | select(.structuralHealth == "periphery_heavy")] | length' 2>/dev/null || echo 0)
    e2e_log_assert_num "$PERIPHERY_HEAVY_COUNT" -ge 1 "g8_skyline_periphery_heavy_label"
    assert_jq "$SKYLINE_JSON" '(.data.summary.communityCount | type)' "number" "g8_skyline_community_count_is_a_number"
    # The degenerate diagnostic is a BICONDITIONAL on the measured community
    # count, not a fixture assumption. `seed_corpus` above contributes its own
    # linked communities on top of the two pairs seed_status_skyline_fixture
    # creates, so this workspace sits ABOVE the threshold (communityCount=5,
    # measured 2026-10-20) and silence is the correct product behaviour. The
    # previous unconditional `-ge 1` encoded the pre-corpus fixture state and
    # reported a correct product as a defect for as long as it existed.
    # The emitter's positive arm is exercised in the isolated workspace below.
    # bd-cxnf8.
    SKYLINE_COMMUNITY_COUNT=$(printf '%s' "$SKYLINE_JSON" | jq -r '.data.summary.communityCount // 0' 2>/dev/null || echo 0)
    DEGENERATE_DIAGNOSTIC_COUNT=$(printf '%s' "$SKYLINE_JSON" | jq '[.data.degraded[]? | select(.code == "graph_skyline_degenerate_communities" and .severity == "info" and ((.message // "") | contains("degenerate")))] | length' 2>/dev/null || echo 0)
    e2e_log_note "g8_skyline_community_count=${SKYLINE_COMMUNITY_COUNT} threshold=${SKYLINE_MIN_COMMUNITY_COUNT} degenerate_diagnostics=${DEGENERATE_DIAGNOSTIC_COUNT}"
    if [ "$SKYLINE_COMMUNITY_COUNT" -lt "$SKYLINE_MIN_COMMUNITY_COUNT" ]; then
        e2e_log_assert_num "$DEGENERATE_DIAGNOSTIC_COUNT" -ge 1 "g8_skyline_degenerate_diagnostic_fires_below_threshold"
    else
        e2e_log_assert_num "$DEGENERATE_DIAGNOSTIC_COUNT" -eq 0 "g8_skyline_degenerate_diagnostic_silent_above_threshold"
    fi
    SNAPSHOT_VERSION=$(printf '%s' "$SKYLINE_JSON" | jq -r '.data.snapshotVersion // empty' 2>/dev/null | head -n 1)
else
    e2e_log_assert_eq "parseable-json" "invalid-json" "g8_skyline_parseable"
    SNAPSHOT_VERSION="unavailable"
fi

# ---------------------------------------------------------------------------
# Positive control for the degenerate-communities emitter.
#
# The seeded epic workspace above sits above the threshold, so nothing there
# executes the emitter's reporting arm. This isolated workspace holds exactly
# the two linked pairs and no corpus, which puts it below the threshold
# (communityCount=2, measured 2026-10-20) and makes the diagnostic mandatory.
# The community count is asserted BESIDE the diagnostic so this control can
# never pass vacuously by drifting above the threshold itself.
#
# The workspace is retained, not removed, and its path is logged.
# ---------------------------------------------------------------------------
WS_G8_ISOLATED=$(mktemp -d "${TMPDIR:-/tmp}/ee-e2e-g8-skyline-isolated.XXXXXX")
e2e_log_note "g8_skyline_isolated_workspace=$WS_G8_ISOLATED"

ee_isolated() {
    e2e_log_command "$EE_BINARY" "$@" --workspace "$WS_G8_ISOLATED"
}

ee_isolated init --json >/dev/null
ee_isolated config set graph.feature.skyline.enabled true --json >/dev/null

ISO_CORE_A=$(ee_isolated remember "G8 isolated skyline core alpha." --level procedural --kind rule --confidence 0.91 --no-auto-link --json | jq -r '.data.memory_id // .data.memoryId // .data.memory.id // .data.id // empty')
ISO_CORE_B=$(ee_isolated remember "G8 isolated skyline core beta." --level semantic --kind note --confidence 0.87 --no-auto-link --json | jq -r '.data.memory_id // .data.memoryId // .data.memory.id // .data.id // empty')
ISO_LEAF_A=$(ee_isolated remember "G8 isolated skyline periphery alpha." --level episodic --kind observation --confidence 0.82 --no-auto-link --json | jq -r '.data.memory_id // .data.memoryId // .data.memory.id // .data.id // empty')
ISO_LEAF_B=$(ee_isolated remember "G8 isolated skyline periphery beta." --level episodic --kind observation --confidence 0.81 --no-auto-link --json | jq -r '.data.memory_id // .data.memoryId // .data.memory.id // .data.id // empty')

if [ -z "$ISO_CORE_A" ] || [ -z "$ISO_CORE_B" ] || [ -z "$ISO_LEAF_A" ] || [ -z "$ISO_LEAF_B" ]; then
    e2e_log_assert_eq "memory-ids" "missing" "g8_skyline_isolated_fixture_memory_ids"
else
    ee_isolated memory link "$ISO_CORE_A" "$ISO_CORE_B" --relation related --undirected --source agent --json >/dev/null
    ee_isolated memory link "$ISO_LEAF_A" "$ISO_LEAF_B" --relation related --undirected --source agent --json >/dev/null

    ISO_JSON=$(ee_isolated status --skyline --json 2>/dev/null || true)
    ISO_COMMUNITY_COUNT=$(printf '%s' "$ISO_JSON" | jq -r '.data.summary.communityCount // -1' 2>/dev/null || echo -1)
    ISO_DEGENERATE_COUNT=$(printf '%s' "$ISO_JSON" | jq '[.data.degraded[]? | select(.code == "graph_skyline_degenerate_communities" and .severity == "info" and ((.message // "") | contains("degenerate")))] | length' 2>/dev/null || echo 0)
    e2e_log_note "g8_skyline_isolated community_count=${ISO_COMMUNITY_COUNT} degenerate_diagnostics=${ISO_DEGENERATE_COUNT}"

    # Precondition: this control is only meaningful below the threshold.
    e2e_log_assert_num "$ISO_COMMUNITY_COUNT" -ge 1 "g8_skyline_isolated_graph_is_non_empty"
    e2e_log_assert_num "$ISO_COMMUNITY_COUNT" -lt "$SKYLINE_MIN_COMMUNITY_COUNT" "g8_skyline_isolated_below_threshold"
    # The assertion the emitter owes: below the threshold it must say so.
    e2e_log_assert_num "$ISO_DEGENERATE_COUNT" -ge 1 "g8_skyline_isolated_degenerate_diagnostic"
fi

if [ "${EE_GRAPH_E2E_INJECT_FAILURE:-0}" = "1" ]; then
    e2e_log_assert_eq "actual-skyline" "expected-skyline" "g8_skyline_injected_failure_diff" || true
fi

ELAPSED_MS="$(python3 -c "import time; print(int((time.monotonic() - float('$START_SECONDS')) * 1000))")"
e2e_log_note "g8_skyline_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} elapsed_ms=${ELAPSED_MS} snapshot_version=${SNAPSHOT_VERSION:-unavailable}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
