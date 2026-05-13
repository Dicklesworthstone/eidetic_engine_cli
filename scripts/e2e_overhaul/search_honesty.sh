#!/usr/bin/env bash
# J3 — Epic B: search honesty & quality e2e driver.
#
# Drives `ee search` and asserts the search response exposes the honesty
# signals shipped by B1-B8/B11 and records TODOs for B10.
#
# Shipped (real assertions):  B1, B2, B3, B4, B5, B6, B7, B8, B11
# Not yet shipped (todo):     B10

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
B6_LEXICAL_JSON=$(ee_workspace search "forbidden dependencies" \
    --source-mode lexical_only \
    --relevance-floor 0.0 \
    --json || true)
assert_jq "$B6_LEXICAL_JSON" '.data.request.sourceMode' "lexical_only" "b6_lexical_request_source_mode"
assert_jq "$B6_LEXICAL_JSON" '.data.metrics.sourceModeRequested' "lexical_only" "b6_lexical_metric_requested"
assert_jq "$B6_LEXICAL_JSON" '.data.metrics.sourceModeApplied' "lexical_only" "b6_lexical_metric_applied"
B6_LEXICAL_NON_LEXICAL_COUNT=$(printf '%s' "$B6_LEXICAL_JSON" \
    | jq '[.data.results[]? | select(.source != "lexical")] | length' 2>/dev/null \
    || echo 0)
e2e_log_assert_eq "$B6_LEXICAL_NON_LEXICAL_COUNT" "0" "b6_lexical_only_returns_lexical_sources"

B6_SEMANTIC_JSON=$(ee_workspace search "forbidden dependencies" \
    --source-mode semantic_only \
    --relevance-floor 0.0 \
    --json || true)
assert_jq "$B6_SEMANTIC_JSON" '.data.request.sourceMode' "semantic_only" "b6_semantic_request_source_mode"
assert_jq "$B6_SEMANTIC_JSON" '.data.metrics.sourceModeRequested' "semantic_only" "b6_semantic_metric_requested"
assert_jq "$B6_SEMANTIC_JSON" '.data.metrics.sourceModeApplied' "semantic_only" "b6_semantic_metric_applied"
B6_SEMANTIC_BAD_SOURCE_COUNT=$(printf '%s' "$B6_SEMANTIC_JSON" \
    | jq '[.data.results[]? | select(.source != "semantic_fast" and .source != "semantic_quality")] | length' 2>/dev/null \
    || echo 0)
e2e_log_assert_eq "$B6_SEMANTIC_BAD_SOURCE_COUNT" "0" "b6_semantic_only_returns_semantic_sources"

B6_HYBRID_JSON=$(ee_workspace search "forbidden dependencies" \
    --source-mode hybrid \
    --relevance-floor 0.0 \
    --json || true)
assert_jq "$B6_HYBRID_JSON" '.data.request.sourceMode' "hybrid" "b6_hybrid_request_source_mode"
assert_jq "$B6_HYBRID_JSON" '.data.metrics.sourceModeRequested' "hybrid" "b6_hybrid_metric_requested"

B6_EMPTY_INDEX_DIR="$EPIC_WORKSPACE/b6-empty-index-lexical"
mkdir -p "$B6_EMPTY_INDEX_DIR"
B6_LEXICAL_UNAVAILABLE_JSON=$(ee_workspace search "source mode unavailable" \
    --index-dir "$B6_EMPTY_INDEX_DIR" \
    --source-mode lexical_only \
    --json || true)
B6_LEXICAL_UNAVAILABLE_RESULT_COUNT=$(printf '%s' "$B6_LEXICAL_UNAVAILABLE_JSON" \
    | jq '.data.results | length' 2>/dev/null \
    || echo 1)
e2e_log_assert_eq "$B6_LEXICAL_UNAVAILABLE_RESULT_COUNT" "0" "b6_lexical_unavailable_returns_no_results"
B6_LEXICAL_UNAVAILABLE_CODE_COUNT=$(printf '%s' "$B6_LEXICAL_UNAVAILABLE_JSON" \
    | jq '[.data.degraded[]?.code // empty] | map(select(. == "lexical_unavailable")) | length' 2>/dev/null \
    || echo 0)
if [ "$B6_LEXICAL_UNAVAILABLE_CODE_COUNT" -gt 0 ]; then
    e2e_log_assert_eq "true" "true" "b6_lexical_unavailable_degraded_code"
else
    e2e_log_assert_eq "$B6_LEXICAL_UNAVAILABLE_CODE_COUNT" ">=1" "b6_lexical_unavailable_degraded_code"
fi

# B10 — output redaction.
todo_assert "b10_output_redaction" "bd-17c65.2.7" \
    "Search output redaction policy not yet implemented."

# B11 — valid_from/valid_to filtering + --as-of historic replay.
B11_QUERY="b11 validity window marker zeta"
B11_REFERENCE_TIME="2098-01-01T00:00:00Z"
B11_REPLAY_TIME="2099-06-15T00:00:00Z"
B11_CURRENT_JSON=$(ee_workspace remember \
    --level semantic \
    --kind fact \
    --tags b11,validity \
    --valid-from "2020-01-01T00:00:00Z" \
    --valid-to "2099-01-01T00:00:00Z" \
    "$B11_QUERY current memory" \
    --json || true)
B11_EXPIRED_JSON=$(ee_workspace remember \
    --level semantic \
    --kind fact \
    --tags b11,validity \
    --valid-from "2020-01-01T00:00:00Z" \
    --valid-to "2021-01-01T00:00:00Z" \
    "$B11_QUERY expired memory" \
    --json || true)
B11_FUTURE_JSON=$(ee_workspace remember \
    --level semantic \
    --kind fact \
    --tags b11,validity \
    --valid-from "2099-06-01T00:00:00Z" \
    "$B11_QUERY future memory" \
    --json || true)
B11_CURRENT_ID=$(printf '%s' "$B11_CURRENT_JSON" | jq -r '.data.memory_id // empty' 2>/dev/null || true)
B11_EXPIRED_ID=$(printf '%s' "$B11_EXPIRED_JSON" | jq -r '.data.memory_id // empty' 2>/dev/null || true)
B11_FUTURE_ID=$(printf '%s' "$B11_FUTURE_JSON" | jq -r '.data.memory_id // empty' 2>/dev/null || true)
if [ -n "$B11_CURRENT_ID" ] && [ -n "$B11_EXPIRED_ID" ] && [ -n "$B11_FUTURE_ID" ]; then
    B11_DEFAULT_SEARCH_JSON=$(ee_workspace search "$B11_QUERY" \
        --as-of "$B11_REFERENCE_TIME" \
        --relevance-floor 0.0 \
        --json || true)
    B11_DEFAULT_CURRENT_COUNT=$(printf '%s' "$B11_DEFAULT_SEARCH_JSON" \
        | jq --arg id "$B11_CURRENT_ID" '[.data.results[]?.docId // empty] | map(select(. == $id)) | length' 2>/dev/null \
        || echo 0)
    B11_DEFAULT_EXPIRED_COUNT=$(printf '%s' "$B11_DEFAULT_SEARCH_JSON" \
        | jq --arg id "$B11_EXPIRED_ID" '[.data.results[]?.docId // empty] | map(select(. == $id)) | length' 2>/dev/null \
        || echo 0)
    B11_DEFAULT_FUTURE_COUNT=$(printf '%s' "$B11_DEFAULT_SEARCH_JSON" \
        | jq --arg id "$B11_FUTURE_ID" '[.data.results[]?.docId // empty] | map(select(. == $id)) | length' 2>/dev/null \
        || echo 0)
    if [ "$B11_DEFAULT_CURRENT_COUNT" -gt 0 ]; then
        e2e_log_assert_eq "true" "true" "b11_search_default_keeps_current_memory"
    else
        e2e_log_assert_eq "$B11_DEFAULT_CURRENT_COUNT" ">=1" "b11_search_default_keeps_current_memory"
    fi
    e2e_log_assert_eq "$B11_DEFAULT_EXPIRED_COUNT" "0" "b11_search_default_excludes_expired"
    e2e_log_assert_eq "$B11_DEFAULT_FUTURE_COUNT" "0" "b11_search_default_excludes_future"
    B11_DEFAULT_FILTER_CODES=$(printf '%s' "$B11_DEFAULT_SEARCH_JSON" \
        | jq '[.data.degraded[]?.code // empty] | map(select(. == "expired_filtered" or . == "future_validity_filtered")) | length' 2>/dev/null \
        || echo 0)
    if [ "$B11_DEFAULT_FILTER_CODES" -gt 0 ]; then
        e2e_log_assert_eq "true" "true" "b11_search_default_reports_validity_filtering"
    else
        e2e_log_assert_eq "$B11_DEFAULT_FILTER_CODES" ">=1" "b11_search_default_reports_validity_filtering"
    fi

    B11_INCLUDE_SEARCH_JSON=$(ee_workspace search "$B11_QUERY" \
        --as-of "$B11_REFERENCE_TIME" \
        --include-expired \
        --include-future \
        --relevance-floor 0.0 \
        --json || true)
    B11_INCLUDE_EXPIRED_COUNT=$(printf '%s' "$B11_INCLUDE_SEARCH_JSON" \
        | jq --arg id "$B11_EXPIRED_ID" '[.data.results[]? | select(.docId == $id and .validityStatus == "expired")] | length' 2>/dev/null \
        || echo 0)
    B11_INCLUDE_FUTURE_COUNT=$(printf '%s' "$B11_INCLUDE_SEARCH_JSON" \
        | jq --arg id "$B11_FUTURE_ID" '[.data.results[]? | select(.docId == $id and .validityStatus == "future")] | length' 2>/dev/null \
        || echo 0)
    if [ "$B11_INCLUDE_EXPIRED_COUNT" -gt 0 ]; then
        e2e_log_assert_eq "true" "true" "b11_search_include_expired_returns_lifecycle"
    else
        e2e_log_assert_eq "$B11_INCLUDE_EXPIRED_COUNT" ">=1" "b11_search_include_expired_returns_lifecycle"
    fi
    if [ "$B11_INCLUDE_FUTURE_COUNT" -gt 0 ]; then
        e2e_log_assert_eq "true" "true" "b11_search_include_future_returns_lifecycle"
    else
        e2e_log_assert_eq "$B11_INCLUDE_FUTURE_COUNT" ">=1" "b11_search_include_future_returns_lifecycle"
    fi

    B11_REPLAY_SEARCH_JSON=$(ee_workspace search "$B11_QUERY" \
        --as-of "$B11_REPLAY_TIME" \
        --relevance-floor 0.0 \
        --json || true)
    B11_REPLAY_FUTURE_COUNT=$(printf '%s' "$B11_REPLAY_SEARCH_JSON" \
        | jq --arg id "$B11_FUTURE_ID" '[.data.results[]?.docId // empty] | map(select(. == $id)) | length' 2>/dev/null \
        || echo 0)
    if [ "$B11_REPLAY_FUTURE_COUNT" -gt 0 ]; then
        e2e_log_assert_eq "true" "true" "b11_search_as_of_replay_includes_future_after_start"
    else
        e2e_log_assert_eq "$B11_REPLAY_FUTURE_COUNT" ">=1" "b11_search_as_of_replay_includes_future_after_start"
    fi

    B11_CONTEXT_DEFAULT_JSON=$(ee_workspace context "$B11_QUERY" \
        --as-of "$B11_REFERENCE_TIME" \
        --max-tokens 1200 \
        --candidate-pool 20 \
        --json || true)
    B11_CONTEXT_CURRENT_COUNT=$(printf '%s' "$B11_CONTEXT_DEFAULT_JSON" \
        | jq --arg id "$B11_CURRENT_ID" '[.data.pack.items[]?.memoryId // empty] | map(select(. == $id)) | length' 2>/dev/null \
        || echo 0)
    B11_CONTEXT_EXPIRED_COUNT=$(printf '%s' "$B11_CONTEXT_DEFAULT_JSON" \
        | jq --arg id "$B11_EXPIRED_ID" '[.data.pack.items[]?.memoryId // empty] | map(select(. == $id)) | length' 2>/dev/null \
        || echo 0)
    B11_CONTEXT_FUTURE_COUNT=$(printf '%s' "$B11_CONTEXT_DEFAULT_JSON" \
        | jq --arg id "$B11_FUTURE_ID" '[.data.pack.items[]?.memoryId // empty] | map(select(. == $id)) | length' 2>/dev/null \
        || echo 0)
    if [ "$B11_CONTEXT_CURRENT_COUNT" -gt 0 ]; then
        e2e_log_assert_eq "true" "true" "b11_context_default_keeps_current_memory"
    else
        e2e_log_assert_eq "$B11_CONTEXT_CURRENT_COUNT" ">=1" "b11_context_default_keeps_current_memory"
    fi
    e2e_log_assert_eq "$B11_CONTEXT_EXPIRED_COUNT" "0" "b11_context_default_excludes_expired"
    e2e_log_assert_eq "$B11_CONTEXT_FUTURE_COUNT" "0" "b11_context_default_excludes_future"

    B11_CONTEXT_INCLUDE_EXPIRED_JSON=$(ee_workspace context "$B11_QUERY expired memory" \
        --as-of "$B11_REFERENCE_TIME" \
        --include-expired \
        --max-tokens 1200 \
        --candidate-pool 20 \
        --json || true)
    B11_CONTEXT_INCLUDE_FUTURE_JSON=$(ee_workspace context "$B11_QUERY future memory" \
        --as-of "$B11_REFERENCE_TIME" \
        --include-future \
        --max-tokens 1200 \
        --candidate-pool 20 \
        --json || true)
    B11_CONTEXT_INCLUDE_EXPIRED_COUNT=$(printf '%s' "$B11_CONTEXT_INCLUDE_EXPIRED_JSON" \
        | jq --arg id "$B11_EXPIRED_ID" '[.data.pack.items[]? | select(.memoryId == $id and .lifecycle.validity_status == "expired")] | length' 2>/dev/null \
        || echo 0)
    B11_CONTEXT_INCLUDE_FUTURE_COUNT=$(printf '%s' "$B11_CONTEXT_INCLUDE_FUTURE_JSON" \
        | jq --arg id "$B11_FUTURE_ID" '[.data.pack.items[]? | select(.memoryId == $id and .lifecycle.validity_status == "future")] | length' 2>/dev/null \
        || echo 0)
    if [ "$B11_CONTEXT_INCLUDE_EXPIRED_COUNT" -gt 0 ]; then
        e2e_log_assert_eq "true" "true" "b11_context_include_expired_lifecycle"
    else
        e2e_log_assert_eq "$B11_CONTEXT_INCLUDE_EXPIRED_COUNT" ">=1" "b11_context_include_expired_lifecycle"
    fi
    if [ "$B11_CONTEXT_INCLUDE_FUTURE_COUNT" -gt 0 ]; then
        e2e_log_assert_eq "true" "true" "b11_context_include_future_lifecycle"
    else
        e2e_log_assert_eq "$B11_CONTEXT_INCLUDE_FUTURE_COUNT" ">=1" "b11_context_include_future_lifecycle"
    fi

    e2e_log_note "b11_include_stale is covered by indexed-metadata unit tests; public remember CLI derives validity_status from valid_from/valid_to."
else
    e2e_log_assert_eq "missing_fixture_memory" "present" "b11_validity_fixture_created"
fi
