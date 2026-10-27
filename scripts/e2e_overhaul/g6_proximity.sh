#!/usr/bin/env bash
# G6.d - Proximity graph e2e logging harness.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
START_SECONDS="$(python3 -c 'import time; print(time.monotonic())')"
epic_setup "g6_proximity"
seed_corpus
# graph.feature.proximity.enabled DEFAULTS TO FALSE (src/config/merge.rs:1304,
# built_in_config). THIS SUITE IS THE ONE THAT SAYS SO OUT LOUD: before this
# line, g6_proximity_interpretation expected "weak" and got the literal string
# "graph_feature_disabled". Fourteen assertions failed here on 2026-10-19,
# mostly nulls downstream of that same dormant feature.
ee_workspace config set graph.feature.proximity.enabled true --json >/dev/null

e2e_log_note "g6_proximity_surface=proximity <id1> <id2>"
MEM_A_JSON=$(ee_workspace remember "G6 proximity source." --level semantic --kind note --no-auto-link --json 2>/dev/null || true)
MEM_B_JSON=$(ee_workspace remember "G6 proximity target." --level semantic --kind note --no-auto-link --json 2>/dev/null || true)
MEM_A=$(printf '%s' "$MEM_A_JSON" | jq -r '.data.memory_id // .data.memory.id // empty' 2>/dev/null || true)
MEM_B=$(printf '%s' "$MEM_B_JSON" | jq -r '.data.memory_id // .data.memory.id // empty' 2>/dev/null || true)
e2e_log_note "g6_proximity_seed_pair=${MEM_A:-missing},${MEM_B:-missing}"

if [ -n "${MEM_A:-}" ] && [ -n "${MEM_B:-}" ]; then
    LINK_JSON=$(ee_workspace memory link "$MEM_A" "$MEM_B" \
        --relation supports \
        --weight 0.91 \
        --confidence 0.91 \
        --json 2>/dev/null || true)
    if printf '%s' "$LINK_JSON" | jq . >/dev/null 2>&1; then
        assert_jq "$LINK_JSON" '.schema' "ee.response.v2" "g6_proximity_link_schema"
        assert_jq "$LINK_JSON" '.data.status' "created" "g6_proximity_link_created"
        assert_jq "$LINK_JSON" '.data.link.source_memory_id' "$MEM_A" "g6_proximity_link_source"
        assert_jq "$LINK_JSON" '.data.link.target_memory_id' "$MEM_B" "g6_proximity_link_target"
    else
        e2e_log_assert_eq "valid-link-json" "invalid-link-json" "g6_proximity_link_json_parse"
    fi
    PROXIMITY_JSON=$(ee_workspace proximity "$MEM_A" "$MEM_B" --json 2>/dev/null || true)
else
    PROXIMITY_JSON=""
fi
if printf '%s' "$PROXIMITY_JSON" | jq . >/dev/null 2>&1; then
    assert_jq "$PROXIMITY_JSON" '.schema' "ee.proximity.v1" "g6_proximity_schema"
    assert_jq "$PROXIMITY_JSON" 'has("schema") and has("memoryA") and has("memoryB") and has("snapshotVersion") and has("minCut") and has("interpretation") and has("treePath") and has("degraded")' "true" "g6_proximity_required_fields"
    assert_jq "$PROXIMITY_JSON" '.memoryA' "$MEM_A" "g6_proximity_memory_a_echo"
    assert_jq "$PROXIMITY_JSON" '.memoryB' "$MEM_B" "g6_proximity_memory_b_echo"
    assert_jq "$PROXIMITY_JSON" '(.treePath | type)' "array" "g6_proximity_tree_path_array"
    assert_jq "$PROXIMITY_JSON" '(.degraded | type)' "array" "g6_proximity_degraded_array"
    assert_jq "$PROXIMITY_JSON" '.interpretation' "weak" "g6_proximity_interpretation"
    assert_jq "$PROXIMITY_JSON" '.treePath[0]' "$MEM_A" "g6_proximity_tree_path_start"
    assert_jq "$PROXIMITY_JSON" '.treePath[1]' "$MEM_B" "g6_proximity_tree_path_end"
    assert_jq "$PROXIMITY_JSON" '.degraded | length' "0" "g6_proximity_no_degraded"
    assert_jq "$PROXIMITY_JSON" '(.minCut >= 0.90 and .minCut <= 0.92)' "true" "g6_proximity_min_cut_seeded_weight"
    HOTSPOTS_JSON=$(ee_workspace insights --section proximityHotspots --json 2>/dev/null || true)
    if printf '%s' "$HOTSPOTS_JSON" | jq . >/dev/null 2>&1; then
        assert_jq "$HOTSPOTS_JSON" '.schema' "ee.response.v2" "g6_proximity_hotspots_response_schema"
        assert_jq "$HOTSPOTS_JSON" '.data.selectedSection' "proximityHotspots" "g6_proximity_hotspots_selected_section"
        assert_jq "$HOTSPOTS_JSON" '(.data.degradedSignals | type)' "array" "g6_proximity_hotspots_degraded_array"
        assert_jq "$HOTSPOTS_JSON" '(.data.degradedSignals | length)' "0" "g6_proximity_hotspots_no_degraded"
        assert_jq "$HOTSPOTS_JSON" '(.data.sections | length)' "1" "g6_proximity_hotspots_single_section"
        assert_jq "$HOTSPOTS_JSON" '.data.sections[0].name' "proximityHotspots" "g6_proximity_hotspots_section_name"
        # proximityHotspots is a GLOBAL top-N section -- it takes no pair
        # argument -- and docs/schemas/ee.insights.v1.json documents its page
        # size as limit 10. `seed_corpus` above contributes its own linked
        # pairs, so the seeded pair is one hotspot among many (measured
        # 2026-10-20: 18 hotspots total, the seeded pair at rank 3 of a
        # 10-item page). Selecting it BY IDENTITY rather than by position is
        # what makes these assertions about the product instead of about how
        # much unrelated structure the corpus happens to contain. bd-k3m0t.
        HOTSPOT_ITEM_COUNT=$(printf '%s' "$HOTSPOTS_JSON" | jq '.data.sections[0].items | length' 2>/dev/null || echo 0)
        e2e_log_assert_num "$HOTSPOT_ITEM_COUNT" -ge 1 "g6_proximity_hotspots_items_present"
        e2e_log_assert_num "$HOTSPOT_ITEM_COUNT" -le 10 "g6_proximity_hotspots_within_documented_limit"
        SEEDED_HOTSPOT=$(printf '%s' "$HOTSPOTS_JSON" | jq -c --arg a "$MEM_A" --arg b "$MEM_B" '[.data.sections[0].items[]? | select(.memoryA == $a and .memoryB == $b)][0] // {}' 2>/dev/null || echo '{}')
        e2e_log_note "g6_proximity_hotspots total_items=${HOTSPOT_ITEM_COUNT} seeded_rank=$(printf '%s' "$SEEDED_HOTSPOT" | jq -r '.rank // "absent"')"
        assert_jq "$SEEDED_HOTSPOT" 'has("memoryA")' "true" "g6_proximity_hotspots_seeded_pair_reported"
        assert_jq "$SEEDED_HOTSPOT" '(.rank >= 1)' "true" "g6_proximity_hotspots_rank_valid"
        assert_jq "$SEEDED_HOTSPOT" '.memoryA' "$MEM_A" "g6_proximity_hotspots_memory_a"
        assert_jq "$SEEDED_HOTSPOT" '.memoryB' "$MEM_B" "g6_proximity_hotspots_memory_b"
        assert_jq "$SEEDED_HOTSPOT" '(.minCut >= 0.90 and .minCut <= 0.92)' "true" "g6_proximity_hotspots_min_cut_seeded_weight"
        assert_jq "$SEEDED_HOTSPOT" '.interpretation' "weak" "g6_proximity_hotspots_interpretation"
        assert_jq "$SEEDED_HOTSPOT" '(.treePath | type)' "array" "g6_proximity_hotspots_tree_path_array"
        assert_jq "$SEEDED_HOTSPOT" '.treePath[0]' "$MEM_A" "g6_proximity_hotspots_tree_path_start"
        assert_jq "$SEEDED_HOTSPOT" '.treePath[1]' "$MEM_B" "g6_proximity_hotspots_tree_path_end"
        assert_jq "$SEEDED_HOTSPOT" '.evidence.schema' "ee.proximity.v1" "g6_proximity_hotspots_evidence_schema"
        assert_jq "$SEEDED_HOTSPOT" '.evidence.algorithm' "gomory_hu_tree" "g6_proximity_hotspots_evidence_algorithm"
        # The two assertions that used to stand here read evidence.memoryA and
        # evidence.memoryB. Those fields cannot exist: proximityHotspotItem.evidence
        # in docs/schemas/ee.insights.v1.json is `additionalProperties: false`
        # over exactly {schema, algorithm, snapshotVersion}, so emitting them
        # would make the surface violate its own published schema. Identity
        # lives on the item (asserted above), and evidence carries provenance.
        # Asserting the exact key set is strictly stronger than asserting two
        # keys that the schema forbids.
        assert_jq "$SEEDED_HOTSPOT" '((.evidence | keys) == ["algorithm","schema","snapshotVersion"])' "true" "g6_proximity_hotspots_evidence_fields_exact"
        assert_jq "$SEEDED_HOTSPOT" '(.evidence.snapshotVersion | type)' "number" "g6_proximity_hotspots_evidence_snapshot_version_number"
    else
        e2e_log_assert_eq "valid-proximity-hotspots-json" "invalid-proximity-hotspots-json" "g6_proximity_hotspots_json_parse"
    fi
    SNAPSHOT_VERSION=$(printf '%s' "$PROXIMITY_JSON" | jq -r '.. | objects | .snapshotVersion? // .snapshot_version? // empty' 2>/dev/null | head -n 1)
else
    e2e_log_assert_eq "valid-proximity-json" "invalid-proximity-json" "g6_proximity_json_parse"
    SNAPSHOT_VERSION="unavailable"
fi

if [ "${EE_GRAPH_E2E_INJECT_FAILURE:-0}" = "1" ]; then
    e2e_log_assert_eq "actual-proximity" "expected-proximity" "g6_proximity_injected_failure_diff" || true
fi

ELAPSED_MS="$(python3 -c "import time; print(int((time.monotonic() - float('$START_SECONDS')) * 1000))")"
e2e_log_note "g6_proximity_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} elapsed_ms=${ELAPSED_MS} snapshot_version=${SNAPSHOT_VERSION:-unavailable}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
