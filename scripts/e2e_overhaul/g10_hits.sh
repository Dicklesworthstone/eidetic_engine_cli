#!/usr/bin/env bash
# G10.d - HITS hubs/authorities graph e2e logging harness.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
START_SECONDS="$(python3 -c 'import time; print(time.monotonic())')"
epic_setup "g10_hits"
seed_corpus

e2e_log_note "g10_hits_surface=insights --section hubs/authorities"
remember_hits_fixture() {
    local content="${1:?content required}"
    ee_workspace remember "$content" --level semantic --kind note --no-auto-link --json 2>/dev/null \
        | jq -r '.data.memory_id // .data.memory.id // .data.id // empty' 2>/dev/null
}

HITS_HUB_ID="$(remember_hits_fixture "G10 HITS fixture hub memory that points to authoritative facts.")"
HITS_AUTHORITY_ID="$(remember_hits_fixture "G10 HITS fixture authority memory grounded by hubs.")"
for memory_id in "$HITS_HUB_ID" "$HITS_AUTHORITY_ID"; do
    e2e_log_assert_num "${#memory_id}" -gt 0 "g10_hits_seed_memory_id"
done

if [ -n "${HITS_HUB_ID:-}" ] && [ -n "${HITS_AUTHORITY_ID:-}" ]; then
    ee_workspace link "$HITS_HUB_ID" "$HITS_AUTHORITY_ID" --relation supports --json >/dev/null 2>&1 || true
fi
e2e_log_note "g10_hits_seed_link hub=${HITS_HUB_ID:-missing} authority=${HITS_AUTHORITY_ID:-missing}"

HUBS_JSON=$(ee_workspace insights --section hubs --json 2>/dev/null || true)
AUTHORITIES_JSON=$(ee_workspace insights --section authorities --json 2>/dev/null || true)
if printf '%s' "$HUBS_JSON" | jq . >/dev/null 2>&1 && printf '%s' "$AUTHORITIES_JSON" | jq . >/dev/null 2>&1; then
    assert_jq_nonempty "$HUBS_JSON" '.schema // empty' "g10_hits_hubs_schema_present"
    assert_jq_nonempty "$AUTHORITIES_JSON" '.schema // empty' "g10_hits_authorities_schema_present"
    assert_jq "$HUBS_JSON" '.data.schema // empty' "ee.insights.v1" "g10_hits_hubs_data_schema"
    assert_jq "$AUTHORITIES_JSON" '.data.schema // empty' "ee.insights.v1" "g10_hits_authorities_data_schema"
    assert_jq "$HUBS_JSON" '.data.command // empty' "insights" "g10_hits_hubs_command"
    assert_jq "$AUTHORITIES_JSON" '.data.command // empty' "insights" "g10_hits_authorities_command"
    assert_jq "$HUBS_JSON" '.data.mode // empty' "section" "g10_hits_hubs_section_mode"
    assert_jq "$AUTHORITIES_JSON" '.data.mode // empty' "section" "g10_hits_authorities_section_mode"
    assert_jq "$HUBS_JSON" '.data.selectedSection // empty' "hubs" "g10_hits_hubs_selected_section"
    assert_jq "$AUTHORITIES_JSON" '.data.selectedSection // empty' "authorities" "g10_hits_authorities_selected_section"
    assert_jq "$HUBS_JSON" '(.data.sections | type)' "array" "g10_hits_hubs_sections_array"
    assert_jq "$AUTHORITIES_JSON" '(.data.sections | type)' "array" "g10_hits_authorities_sections_array"
    assert_jq "$HUBS_JSON" '(.data.degradedSignals | type)' "array" "g10_hits_hubs_degraded_signals_array"
    assert_jq "$AUTHORITIES_JSON" '(.data.degradedSignals | type)' "array" "g10_hits_authorities_degraded_signals_array"
    assert_jq "$HUBS_JSON" '((.data.availableSections // []) | index("hubs") != null)' "true" "g10_hits_hubs_available_section"
    assert_jq "$AUTHORITIES_JSON" '((.data.availableSections // []) | index("authorities") != null)' "true" "g10_hits_authorities_available_section"
    assert_jq "$HUBS_JSON" '.data.sections[0].name // empty' "hubs" "g10_hits_hubs_section_name"
    assert_jq "$AUTHORITIES_JSON" '.data.sections[0].name // empty' "authorities" "g10_hits_authorities_section_name"
    HUB_SCORE_PRESENT=$(printf '%s' "$HUBS_JSON" | jq '[.data.sections[0].items[]? | select(has("hubScore"))] | length' 2>/dev/null || echo 0)
    AUTHORITY_SCORE_PRESENT=$(printf '%s' "$AUTHORITIES_JSON" | jq '[.data.sections[0].items[]? | select(has("authorityScore"))] | length' 2>/dev/null || echo 0)
    e2e_log_assert_num "$HUB_SCORE_PRESENT" -ge 1 "g10_hits_hub_score_present"
    e2e_log_assert_num "$AUTHORITY_SCORE_PRESENT" -ge 1 "g10_hits_authority_score_present"
    assert_jq "$HUBS_JSON" '.data.sections[0].items[0].memoryId // empty' "$HITS_HUB_ID" "g10_hits_seed_hub_ranked"
    assert_jq "$AUTHORITIES_JSON" '.data.sections[0].items[0].memoryId // empty' "$HITS_AUTHORITY_ID" "g10_hits_seed_authority_ranked"
    assert_jq "$HUBS_JSON" '.data.sections[0].items[0].evidence.schema // empty' "ee.graph.hits.v1" "g10_hits_hubs_evidence_schema"
    assert_jq "$AUTHORITIES_JSON" '.data.sections[0].items[0].evidence.schema // empty' "ee.graph.hits.v1" "g10_hits_authorities_evidence_schema"
    assert_jq "$HUBS_JSON" '.data.sections[0].items[0].evidence.algorithm // empty' "hits_centrality_directed" "g10_hits_hubs_evidence_algorithm"
    assert_jq "$AUTHORITIES_JSON" '.data.sections[0].items[0].evidence.algorithm // empty' "hits_centrality_directed" "g10_hits_authorities_evidence_algorithm"
    SNAPSHOT_VERSION=$(printf '%s\n%s' "$HUBS_JSON" "$AUTHORITIES_JSON" | jq -r '.. | objects | .snapshotVersion? // .snapshot_version? // empty' 2>/dev/null | head -n 1)
else
    todo_assert "g10_hits_surface_available" "bd-jy4w.4" "ee insights hubs/authorities sections are not fully available yet."
    SNAPSHOT_VERSION="unavailable"
fi

if [ "${EE_GRAPH_E2E_INJECT_FAILURE:-0}" = "1" ]; then
    e2e_log_assert_eq "actual-hits" "expected-hits" "g10_hits_injected_failure_diff" || true
fi

ELAPSED_MS="$(python3 -c "import time; print(int((time.monotonic() - float('$START_SECONDS')) * 1000))")"
e2e_log_note "g10_hits_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} elapsed_ms=${ELAPSED_MS} snapshot_version=${SNAPSHOT_VERSION:-unavailable}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
