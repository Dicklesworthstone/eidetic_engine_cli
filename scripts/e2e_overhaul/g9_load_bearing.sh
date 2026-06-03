#!/usr/bin/env bash
# G9.d - Load-bearing memories graph e2e logging harness.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
START_SECONDS="$(python3 -c 'import time; print(time.monotonic())')"
epic_setup "g9_load_bearing"
seed_corpus
ee_workspace config set graph.feature.load_bearing.enabled true --json >/dev/null

LOAD_BEARING_MEMORY_JSON=$(ee_workspace remember \
    "G9 load-bearing source memory shared by cornerstone rules." \
    --level semantic \
    --kind fact \
    --confidence 0.91 \
    --json)
LOAD_BEARING_MEMORY_ID=$(printf '%s' "$LOAD_BEARING_MEMORY_JSON" | jq -r '.data.memory_id // empty')
SOLO_MEMORY_JSON=$(ee_workspace remember \
    "G9 solo source memory cited by one rule." \
    --level semantic \
    --kind fact \
    --confidence 0.81 \
    --json)
SOLO_MEMORY_ID=$(printf '%s' "$SOLO_MEMORY_JSON" | jq -r '.data.memory_id // empty')

if [ -z "$LOAD_BEARING_MEMORY_ID" ] || [ -z "$SOLO_MEMORY_ID" ]; then
    e2e_log_assert_eq "${LOAD_BEARING_MEMORY_ID:-missing}/${SOLO_MEMORY_ID:-missing}" "memory-ids" "g9_load_bearing_fixture_memory_ids"
else
    ee_workspace rule add \
        "G9 cornerstone rule alpha cites the load-bearing memory." \
        --maturity validated \
        --source-memory "$LOAD_BEARING_MEMORY_ID" \
        --json >/dev/null
    ee_workspace rule add \
        "G9 cornerstone rule beta cites both load-bearing and solo memories." \
        --maturity validated \
        --source-memory "$LOAD_BEARING_MEMORY_ID" \
        --source-memory "$SOLO_MEMORY_ID" \
        --json >/dev/null
    e2e_log_note "g9_load_bearing_fixture load_bearing_memory=${LOAD_BEARING_MEMORY_ID} solo_memory=${SOLO_MEMORY_ID}"
fi

e2e_log_note "g9_load_bearing_surface=insights --section loadBearingMemories"
INSIGHTS_JSON=$(ee_workspace insights --section loadBearingMemories --json 2>/dev/null || true)
if printf '%s' "$INSIGHTS_JSON" | jq . >/dev/null 2>&1; then
    assert_jq_nonempty "$INSIGHTS_JSON" '.schema // empty' "g9_load_bearing_schema_present"
    assert_jq "$INSIGHTS_JSON" '.data.schema // empty' "ee.insights.v1" "g9_load_bearing_data_schema"
    assert_jq "$INSIGHTS_JSON" '.data.command // empty' "insights" "g9_load_bearing_command"
    assert_jq "$INSIGHTS_JSON" '.data.mode // empty' "section" "g9_load_bearing_section_mode"
    assert_jq "$INSIGHTS_JSON" '.data.selectedSection // empty' "loadBearingMemories" "g9_load_bearing_selected_section"
    assert_jq "$INSIGHTS_JSON" '(.data.availableSections // [] | index("loadBearingMemories") != null)' "true" "g9_load_bearing_available_section"
    assert_jq "$INSIGHTS_JSON" '(.data.sections | type)' "array" "g9_load_bearing_sections_array"
    assert_jq "$INSIGHTS_JSON" '(.data.degradedSignals | type)' "array" "g9_load_bearing_degraded_signals_array"
    assert_jq "$INSIGHTS_JSON" '.data.sections[0].name // empty' "loadBearingMemories" "g9_load_bearing_section_name"
    assert_jq_nonempty "$INSIGHTS_JSON" '.data.sections[0].title // empty' "g9_load_bearing_section_title"
    assert_jq_nonempty "$INSIGHTS_JSON" '.data.sections[0].summary // empty' "g9_load_bearing_section_summary"
    assert_jq "$INSIGHTS_JSON" '.data.sections[0].items | type' "array" "g9_load_bearing_items_array"
    LOAD_BEARING_ITEMS=$(printf '%s' "$INSIGHTS_JSON" | jq '(.data.sections[0].items // []) | length' 2>/dev/null || echo 0)
    e2e_log_assert_num "$LOAD_BEARING_ITEMS" -ge 1 "g9_load_bearing_item_present"
    assert_jq "$INSIGHTS_JSON" '.data.sections[0].items[0].rank >= 1' "true" "g9_load_bearing_item_rank_valid"
    assert_jq_nonempty "$INSIGHTS_JSON" '.data.sections[0].items[0].memoryId // empty' "g9_load_bearing_item_memory_id"
    assert_jq "$INSIGHTS_JSON" '(.data.sections[0].items[0].loadBearingScore | type)' "number" "g9_load_bearing_score_number"
    assert_jq "$INSIGHTS_JSON" '.data.sections[0].items[0].loadBearingScore >= 0' "true" "g9_load_bearing_score_nonnegative"
    assert_jq "$INSIGHTS_JSON" '(.data.sections[0].items[0].citingRuleCount | type)' "number" "g9_load_bearing_citing_rule_count_number"
    assert_jq "$INSIGHTS_JSON" '.data.sections[0].items[0].citingRuleCount >= 0' "true" "g9_load_bearing_citing_rule_count_nonnegative"
    assert_jq "$INSIGHTS_JSON" '.data.sections[0].items[0].interpretation // empty' "load_bearing" "g9_load_bearing_interpretation"
    assert_jq "$INSIGHTS_JSON" '.data.sections[0].items[0].evidence.schema // empty' "ee.graph.hits.v1" "g9_load_bearing_evidence_schema"
    assert_jq "$INSIGHTS_JSON" '.data.sections[0].items[0].evidence.algorithm // empty' "bipartite_hits" "g9_load_bearing_evidence_algorithm"
    assert_jq "$INSIGHTS_JSON" '(.data.sections[0].items[0].evidence.snapshotVersion | type)' "number" "g9_load_bearing_evidence_snapshot_version_number"
    SNAPSHOT_VERSION=$(printf '%s' "$INSIGHTS_JSON" | jq -r '.. | objects | .snapshotVersion? // .snapshot_version? // empty' 2>/dev/null | head -n 1)
else
    todo_assert "g9_load_bearing_surface_available" "bd-2jl2.4" "ee insights --section loadBearingMemories is not fully available yet."
    SNAPSHOT_VERSION="unavailable"
fi

e2e_log_note "g9_load_bearing_surface=why graph.loadBearing"
if [ -n "${LOAD_BEARING_MEMORY_ID:-}" ]; then
    WHY_JSON=$(ee_workspace why "$LOAD_BEARING_MEMORY_ID" --json 2>/dev/null || true)
    if printf '%s' "$WHY_JSON" | jq . >/dev/null 2>&1; then
        assert_jq "$WHY_JSON" '.schema // empty' "ee.response.v2" "g9_load_bearing_why_envelope_schema"
        assert_jq "$WHY_JSON" '.success // false' "true" "g9_load_bearing_why_success"
        assert_jq "$WHY_JSON" '.data.graph.loadBearing.isLoadBearing // false' "true" "g9_load_bearing_why_flag"
        assert_jq "$WHY_JSON" '.data.graph.loadBearing.interpretation // empty' "load_bearing" "g9_load_bearing_why_interpretation"
        assert_jq "$WHY_JSON" '(.data.graph.loadBearing.citingRuleCount // 0) >= 2' "true" "g9_load_bearing_why_citing_rule_count"
        assert_jq "$WHY_JSON" '.data.graph.loadBearing.evidence.projection // empty' "rule_provenance_bipartite" "g9_load_bearing_why_projection"
        assert_jq "$WHY_JSON" '.data.graph.loadBearing.evidence.algorithm // empty' "bipartite_hits" "g9_load_bearing_why_algorithm"
    else
        e2e_log_assert_eq "parseable-json" "invalid-json" "g9_load_bearing_why_parseable_json" || true
    fi
fi

if [ "${EE_GRAPH_E2E_INJECT_FAILURE:-0}" = "1" ]; then
    e2e_log_assert_eq "actual-load-bearing" "expected-load-bearing" "g9_load_bearing_injected_failure_diff" || true
fi

ELAPSED_MS="$(python3 -c "import time; print(int((time.monotonic() - float('$START_SECONDS')) * 1000))")"
e2e_log_note "g9_load_bearing_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} elapsed_ms=${ELAPSED_MS} snapshot_version=${SNAPSHOT_VERSION:-unavailable}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
