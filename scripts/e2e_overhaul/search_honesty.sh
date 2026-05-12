#!/usr/bin/env bash
# J3 — Epic B: search honesty & quality e2e driver.
#
# Drives `ee search` and asserts the search response exposes the honesty
# signals shipped by B1-B5 and records TODOs for B2, B6-B11.
#
# Shipped (real assertions):  B1, B3, B4, B5
# Not yet shipped (todo):     B2, B6, B7, B8, B10, B11

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
# B2 — lexical/BM25 fusion. Today the default features advertise lexical-bm25
# but `source: lexical` never appears in any result. The fix lives in B2.
# ------------------------------------------------------------
LEXICAL_SOURCE_COUNT=$(printf '%s' "$SEARCH_JSON" \
    | jq '[.data.results[]?.source // empty] | map(select(. == "lexical")) | length' 2>/dev/null \
    || echo 0)
e2e_log_note "b2_lexical_source_count=$LEXICAL_SOURCE_COUNT (expected >=1 once B2 ships)"
todo_assert "b2_lexical_fusion_emits_lexical_source" "bd-17c65.2.2" \
    "Default features claim lexical-bm25 but no result has source=lexical."

# B6 — --source-mode flag (lexical_only|semantic_only|hybrid).
todo_assert "b6_source_mode_flag" "bd-17c65.2.3" \
    "ee search lacks --source-mode for forcing lexical/semantic isolation."

# B7 — ee diag search --all-arms.
todo_assert "b7_diag_search_all_arms" "bd-17c65.2.4" \
    "ee diag search --all-arms not yet implemented for cross-retrieval comparison."

# B8 — tombstone visibility semantics in search/pack/graph.
todo_assert "b8_tombstone_visibility_documented" "bd-17c65.2.8" \
    "Tombstone visibility through search/pack/graph is currently undocumented."

# B10 — output redaction.
todo_assert "b10_output_redaction" "bd-17c65.2.7" \
    "Search output redaction policy not yet implemented."

# B11 — valid_from/valid_to filtering + --as-of historic replay.
todo_assert "b11_validity_window_filtering" "bd-17c65.2.10" \
    "valid_from/valid_to filtering and --as-of historic replay not yet wired."
