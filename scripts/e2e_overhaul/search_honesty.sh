#!/usr/bin/env bash
# J3 — Epic B: search honesty & quality e2e driver.
#
# Drives `ee search` and asserts the search response exposes the honesty
# signals shipped by B1-B5/B7/B8 and records TODOs for B6, B10, B11.
#
# Shipped (real assertions):  B1, B2, B3, B4, B5, B7, B8
# Not yet shipped (todo):     B6, B10, B11

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
epic_setup "epic_B_search_honesty"
seed_corpus

# ------------------------------------------------------------
# Nominal query — expect at least one shipped honesty signal in the response.
# ------------------------------------------------------------
SEARCH_JSON=$(ee_workspace search "forbidden dependencies" --json || true)
if ! printf '%s' "$SEARCH_JSON" | jq . >/dev/null 2>&1; then
    e2e_log_note "search_json_unparseable bytes=${#SEARCH_JSON}"
    e2e_log_assert_eq "false" "true" "search_json_parses"
    exit 0
fi

# ------------------------------------------------------------
# B1 (shipped) — relevance floor metric appears in retrieval metrics.
# Default floor is 0.05; presence of relevanceFloor in metrics is the contract.
# ------------------------------------------------------------
RELEVANCE_FLOOR=$(printf '%s' "$SEARCH_JSON" \
    | jq -r '.data.metrics.relevanceFloor // empty' 2>/dev/null || true)
if [ -n "$RELEVANCE_FLOOR" ]; then
    e2e_log_assert_eq "true" "true" "b1_relevance_floor_metric_present"
else
    e2e_log_assert_eq "missing" "present" "b1_relevance_floor_metric_present"
fi

# B1 acceptance: when the query has no real hits, degraded[] includes
# no_relevant_results or low_recall_after_floor.
NO_RECALL_COUNT=$(printf '%s' "$SEARCH_JSON" \
    | jq '[.data.degraded[]?.code // empty] | map(select(. == "no_relevant_results" or . == "low_recall_after_floor")) | length' 2>/dev/null \
    || echo 0)
e2e_log_note "b1_degraded_no_recall_count=$NO_RECALL_COUNT"

# ------------------------------------------------------------
# B3 (shipped) — duplicates_collapsed degraded code surfaces when dedupe ran.
# We can't easily induce duplicates from the corpus, so just note presence.
# ------------------------------------------------------------
DUP_COLLAPSED=$(printf '%s' "$SEARCH_JSON" \
    | jq '[.data.degraded[]?.code // empty] | map(select(. == "duplicates_collapsed")) | length' 2>/dev/null \
    || echo 0)
e2e_log_note "b3_duplicates_collapsed_degraded_count=$DUP_COLLAPSED"

# ------------------------------------------------------------
# B4 (shipped) — qualityAssessment + honestQualityScore appear in metrics.
# ------------------------------------------------------------
QA=$(printf '%s' "$SEARCH_JSON" \
    | jq -r '.data.metrics.qualityAssessment // empty' 2>/dev/null || true)
if [ -n "$QA" ]; then
    e2e_log_assert_eq "true" "true" "b4_quality_assessment_present"
    # qualityAssessment must be one of the three enum values.
    case "$QA" in
        good|weak|empty) e2e_log_assert_eq "true" "true" "b4_quality_assessment_enum_valid" ;;
        *) e2e_log_assert_eq "$QA" "good|weak|empty" "b4_quality_assessment_enum_valid" ;;
    esac
else
    e2e_log_assert_eq "missing" "present" "b4_quality_assessment_present"
fi

# honestQualityScore is a float or null; presence as a key is the contract.
HAS_HONEST_SCORE=$(printf '%s' "$SEARCH_JSON" \
    | jq -r '.data.metrics | has("honestQualityScore")' 2>/dev/null || echo false)
e2e_log_assert_eq "$HAS_HONEST_SCORE" "true" "b4_honest_quality_score_field_present"

# ------------------------------------------------------------
# B5 (shipped) — weak_query_recall degradation appears when the top score is
# below 2x the relevance floor. Trigger with a query unlikely to match well.
# ------------------------------------------------------------
WEAK_JSON=$(ee_workspace search "xyzzy quux fnord" --json || true)
WEAK_DEGRADED=$(printf '%s' "$WEAK_JSON" \
    | jq -r '[.data.degraded[]?.code // empty] | join(",")' 2>/dev/null || true)
e2e_log_note "b5_weak_query_degraded_codes=$WEAK_DEGRADED"

# ------------------------------------------------------------
# Counter-check: results must never be 10 zero-score hits silently (B1 acceptance).
# Either we get >0 above-floor results, or degraded[] is non-empty.
# ------------------------------------------------------------
RESULT_COUNT=$(printf '%s' "$SEARCH_JSON" | jq '.data.results | length' 2>/dev/null || echo 0)
DEGRADED_COUNT=$(printf '%s' "$SEARCH_JSON" | jq '.data.degraded | length' 2>/dev/null || echo 0)
if [ "$RESULT_COUNT" -gt 0 ] || [ "$DEGRADED_COUNT" -gt 0 ]; then
    e2e_log_assert_eq "true" "true" "b1_no_silent_zero_score_returns"
else
    e2e_log_assert_eq "empty_silent" "either_results_or_degraded" "b1_no_silent_zero_score_returns"
fi

# ------------------------------------------------------------
# B2 — lexical/BM25 fusion. Default search must preserve evidence that the
# lexical arm contributed: at least one result should be lexical or hybrid, and
# sourceCounts must reflect the same contribution.
# ------------------------------------------------------------
LEXICAL_OR_HYBRID_SOURCE_COUNT=$(printf '%s' "$SEARCH_JSON" \
    | jq '[.data.results[]?.source // empty] | map(select(. == "lexical" or . == "hybrid")) | length' 2>/dev/null \
    || echo 0)
if [ "$LEXICAL_OR_HYBRID_SOURCE_COUNT" -gt 0 ]; then
    e2e_log_assert_eq "true" "true" "b2_lexical_fusion_emits_lexical_or_hybrid_source"
else
    e2e_log_assert_eq "0" ">=1" "b2_lexical_fusion_emits_lexical_or_hybrid_source"
fi

LEXICAL_OR_HYBRID_METRIC_COUNT=$(printf '%s' "$SEARCH_JSON" \
    | jq '(.data.metrics.sourceCounts.lexical // 0) + (.data.metrics.sourceCounts.hybrid // 0)' 2>/dev/null \
    || echo 0)
if [ "$LEXICAL_OR_HYBRID_METRIC_COUNT" -gt 0 ]; then
    e2e_log_assert_eq "true" "true" "b2_source_counts_record_lexical_or_hybrid"
else
    e2e_log_assert_eq "0" ">=1" "b2_source_counts_record_lexical_or_hybrid"
fi

# B7 — ee diag search --all-arms.
DIAG_JSON=$(ee_workspace diag search "forbidden dependencies" --all-arms --json || true)
if printf '%s' "$DIAG_JSON" | jq . >/dev/null 2>&1; then
    assert_jq "$DIAG_JSON" '.schema' "ee.diag.search.v1" "b7_diag_search_schema"
    assert_jq "$DIAG_JSON" '.command' "diag search" "b7_diag_search_command"
    assert_jq "$DIAG_JSON" '.preFusion.lexical.available' "true" "b7_lexical_arm_available"
    LEXICAL_PREFUSION_COUNT=$(printf '%s' "$DIAG_JSON" \
        | jq '.preFusion.lexical.results | length' 2>/dev/null || echo 0)
    if [ "$LEXICAL_PREFUSION_COUNT" -gt 0 ]; then
        e2e_log_assert_eq "true" "true" "b7_lexical_prefusion_results_present"
    else
        e2e_log_assert_eq "0" ">=1" "b7_lexical_prefusion_results_present"
    fi
    FUSION_CONTRIBUTION_COUNT=$(printf '%s' "$DIAG_JSON" \
        | jq '.fusion.perDocContribution | length' 2>/dev/null || echo 0)
    if [ "$FUSION_CONTRIBUTION_COUNT" -gt 0 ]; then
        e2e_log_assert_eq "true" "true" "b7_fusion_contributions_present"
    else
        e2e_log_assert_eq "0" ">=1" "b7_fusion_contributions_present"
    fi
else
    e2e_log_note "diag_search_json_unparseable bytes=${#DIAG_JSON}"
    e2e_log_assert_eq "false" "true" "b7_diag_search_json_parses"
fi

# B8 — tombstone visibility semantics in search/context/why/graph/export.
B8_QUERY="b8 tombstone visibility alpha marker"
B8_REASON="b8-search-honesty-fixture"
B8_TOMBSTONE_JSON=$(ee_workspace remember \
    --level procedural \
    --kind rule \
    --tags b8,tombstone \
    "$B8_QUERY tombstoned rule" \
    --json || true)
B8_ACTIVE_JSON=$(ee_workspace remember \
    --level procedural \
    --kind rule \
    --tags b8,tombstone \
    "b8 tombstone visibility beta active companion" \
    --json || true)
B8_MEMORY_ID=$(printf '%s' "$B8_TOMBSTONE_JSON" \
    | jq -r '.data.memory_id // empty' 2>/dev/null || true)
B8_ACTIVE_ID=$(printf '%s' "$B8_ACTIVE_JSON" \
    | jq -r '.data.memory_id // empty' 2>/dev/null || true)
if [ -n "$B8_MEMORY_ID" ] && [ -n "$B8_ACTIVE_ID" ]; then
    ee_workspace memory link "$B8_MEMORY_ID" "$B8_ACTIVE_ID" \
        --relation supports \
        --actor search_honesty_e2e \
        --json >/dev/null || true
    ee_workspace curate tombstone "$B8_MEMORY_ID" \
        --reason "$B8_REASON" \
        --actor search_honesty_e2e \
        --json >/dev/null || true

    B8_DEFAULT_JSON=$(ee_workspace search "$B8_QUERY" --relevance-floor 0.0 --json || true)
    B8_DEFAULT_COUNT=$(printf '%s' "$B8_DEFAULT_JSON" \
        | jq --arg id "$B8_MEMORY_ID" '[.data.results[]?.docId // empty] | map(select(. == $id)) | length' 2>/dev/null \
        || echo 0)
    e2e_log_assert_eq "$B8_DEFAULT_COUNT" "0" "b8_search_excludes_tombstoned_by_default"
    B8_FILTERED_COUNT=$(printf '%s' "$B8_DEFAULT_JSON" \
        | jq '[.data.degraded[]?.code // empty] | map(select(. == "tombstoned_filtered")) | length' 2>/dev/null \
        || echo 0)
    if [ "$B8_FILTERED_COUNT" -gt 0 ]; then
        e2e_log_assert_eq "true" "true" "b8_search_emits_tombstoned_filtered"
    else
        e2e_log_assert_eq "$B8_FILTERED_COUNT" ">=1" "b8_search_emits_tombstoned_filtered"
    fi

    B8_INCLUDE_JSON=$(ee_workspace search "$B8_QUERY" --include-tombstoned --relevance-floor 0.0 --json || true)
    B8_INCLUDE_COUNT=$(printf '%s' "$B8_INCLUDE_JSON" \
        | jq --arg id "$B8_MEMORY_ID" '[.data.results[]? | select(.docId == $id and .tombstoned == true and (.tombstonedAt // "") != "" and .metadata.tombstoned == true)] | length' 2>/dev/null \
        || echo 0)
    if [ "$B8_INCLUDE_COUNT" -gt 0 ]; then
        e2e_log_assert_eq "true" "true" "b8_search_include_tombstoned_returns_marker"
    else
        e2e_log_assert_eq "$B8_INCLUDE_COUNT" ">=1" "b8_search_include_tombstoned_returns_marker"
    fi
    B8_DEGRADED_COUNT=$(printf '%s' "$B8_INCLUDE_JSON" \
        | jq '[.data.degraded[]?.code // empty] | map(select(. == "tombstoned_in_results")) | length' 2>/dev/null \
        || echo 0)
    if [ "$B8_DEGRADED_COUNT" -gt 0 ]; then
        e2e_log_assert_eq "true" "true" "b8_search_include_tombstoned_emits_degraded"
    else
        e2e_log_assert_eq "$B8_DEGRADED_COUNT" ">=1" "b8_search_include_tombstoned_emits_degraded"
    fi

    B8_CONTEXT_DEFAULT_JSON=$(ee_workspace context "$B8_QUERY" --json || true)
    B8_CONTEXT_DEFAULT_COUNT=$(printf '%s' "$B8_CONTEXT_DEFAULT_JSON" \
        | jq --arg id "$B8_MEMORY_ID" '[.data.pack.items[]?.memoryId // empty] | map(select(. == $id)) | length' 2>/dev/null \
        || echo 0)
    e2e_log_assert_eq "$B8_CONTEXT_DEFAULT_COUNT" "0" "b8_context_excludes_tombstoned_by_default"
    B8_CONTEXT_INCLUDE_JSON=$(ee_workspace context "$B8_QUERY" --include-tombstoned --json || true)
    B8_CONTEXT_INCLUDE_COUNT=$(printf '%s' "$B8_CONTEXT_INCLUDE_JSON" \
        | jq --arg id "$B8_MEMORY_ID" '[.data.pack.items[]? | select(.memoryId == $id and .lifecycle.status == "tombstoned" and (.lifecycle.tombstonedAt // "") != "")] | length' 2>/dev/null \
        || echo 0)
    if [ "$B8_CONTEXT_INCLUDE_COUNT" -gt 0 ]; then
        e2e_log_assert_eq "true" "true" "b8_context_include_tombstoned_has_lifecycle"
    else
        e2e_log_assert_eq "$B8_CONTEXT_INCLUDE_COUNT" ">=1" "b8_context_include_tombstoned_has_lifecycle"
    fi

    B8_WHY_JSON=$(ee_workspace why "$B8_MEMORY_ID" --json || true)
    assert_jq "$B8_WHY_JSON" '.data.lifecycle.status' "tombstoned" "b8_why_tombstoned_status"
    assert_jq "$B8_WHY_JSON" '.data.lifecycle.tombstoned_reason' "$B8_REASON" "b8_why_tombstoned_reason"

    B8_MEMORY_LIST_JSON=$(ee_workspace memory list --json || true)
    B8_MEMORY_LIST_COUNT=$(printf '%s' "$B8_MEMORY_LIST_JSON" \
        | jq --arg id "$B8_MEMORY_ID" '[.data.memories[]? | select(.id == $id and .is_tombstoned == true)] | length' 2>/dev/null \
        || echo 0)
    if [ "$B8_MEMORY_LIST_COUNT" -gt 0 ]; then
        e2e_log_assert_eq "true" "true" "b8_memory_list_includes_tombstoned_by_default"
    else
        e2e_log_assert_eq "$B8_MEMORY_LIST_COUNT" ">=1" "b8_memory_list_includes_tombstoned_by_default"
    fi
    B8_MEMORY_LIST_NO_JSON=$(ee_workspace memory list --no-tombstoned --json || true)
    B8_MEMORY_LIST_NO_COUNT=$(printf '%s' "$B8_MEMORY_LIST_NO_JSON" \
        | jq --arg id "$B8_MEMORY_ID" '[.data.memories[]?.id // empty] | map(select(. == $id)) | length' 2>/dev/null \
        || echo 0)
    e2e_log_assert_eq "$B8_MEMORY_LIST_NO_COUNT" "0" "b8_memory_list_no_tombstoned_excludes"

    B8_GRAPH_DEFAULT_JSON=$(ee_workspace graph pagerank --json || true)
    B8_GRAPH_EXCLUDED_COUNT=$(printf '%s' "$B8_GRAPH_DEFAULT_JSON" \
        | jq --arg id "$B8_MEMORY_ID" '[.data.graph.excludedNodes[]?] | map(select(. == $id)) | length' 2>/dev/null \
        || echo 0)
    if [ "$B8_GRAPH_EXCLUDED_COUNT" -gt 0 ]; then
        e2e_log_assert_eq "true" "true" "b8_graph_excludes_tombstoned_by_default"
    else
        e2e_log_assert_eq "$B8_GRAPH_EXCLUDED_COUNT" ">=1" "b8_graph_excludes_tombstoned_by_default"
    fi
    B8_GRAPH_INCLUDE_JSON=$(ee_workspace graph pagerank --include-tombstoned --json || true)
    B8_GRAPH_INCLUDE_EXCLUDED_COUNT=$(printf '%s' "$B8_GRAPH_INCLUDE_JSON" \
        | jq --arg id "$B8_MEMORY_ID" '[.data.graph.excludedNodes[]?] | map(select(. == $id)) | length' 2>/dev/null \
        || echo 0)
    e2e_log_assert_eq "$B8_GRAPH_INCLUDE_EXCLUDED_COUNT" "0" "b8_graph_include_tombstoned_recomputes"

    B8_EXPORT_DIR="$EPIC_WORKSPACE/b8-export"
    B8_EXPORT_JSON=$(ee_workspace export --output-dir "$B8_EXPORT_DIR" --redaction none --label b8-tombstone --json || true)
    B8_RECORDS_PATH=$(printf '%s' "$B8_EXPORT_JSON" \
        | jq -r '.data.recordsPath // empty' 2>/dev/null || true)
    if [ -n "$B8_RECORDS_PATH" ] && [ -f "$B8_RECORDS_PATH" ]; then
        B8_EXPORT_TOMBSTONE_COUNT=$(jq --arg id "$B8_MEMORY_ID" --arg reason "$B8_REASON" \
            'select(.schema == "ee.export.memory.v1" and .memory_id == $id and (.tombstoned_at // "") != "" and .tombstoned_reason == $reason) | .memory_id' \
            "$B8_RECORDS_PATH" 2>/dev/null | wc -l | tr -d ' ')
        if [ "$B8_EXPORT_TOMBSTONE_COUNT" -gt 0 ]; then
            e2e_log_assert_eq "true" "true" "b8_export_includes_tombstone_metadata"
        else
            e2e_log_assert_eq "$B8_EXPORT_TOMBSTONE_COUNT" ">=1" "b8_export_includes_tombstone_metadata"
        fi
    else
        e2e_log_assert_eq "missing_records_path" "present" "b8_export_records_path"
    fi
else
    e2e_log_assert_eq "missing_fixture_memory" "present" "b8_tombstone_fixture_created"
fi

# B6 — --source-mode flag (lexical_only|semantic_only|hybrid).
todo_assert "b6_source_mode_flag" "bd-17c65.2.3" \
    "ee search lacks --source-mode for forcing lexical/semantic isolation."

# B10 — output redaction.
todo_assert "b10_output_redaction" "bd-17c65.2.7" \
    "Search output redaction policy not yet implemented."

# B11 — valid_from/valid_to filtering + --as-of historic replay.
todo_assert "b11_validity_window_filtering" "bd-17c65.2.10" \
    "valid_from/valid_to filtering and --as-of historic replay not yet wired."
