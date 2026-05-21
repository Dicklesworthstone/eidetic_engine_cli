#!/usr/bin/env bash
# N14 — mutual-information dedup e2e driver.
#
# Seeds a paraphrase pair that exact docId/content-hash dedup cannot collapse,
# verifies curate proposes the pair, then verifies `ee search --dedupe=mi`
# keeps one representative with metadata.mergedFrom provenance.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
epic_setup "mi_dedup"

MEMORY_A=$(ee_workspace remember \
    "Run cargo fmt before release and run cargo test before release." \
    --level procedural --kind rule --json)
assert_jq "$MEMORY_A" '.success' "true" "mi_dedup_seed_a_success"

MEMORY_B=$(ee_workspace remember \
    "Before release, run cargo test and cargo fmt." \
    --level procedural --kind rule --json)
assert_jq "$MEMORY_B" '.success' "true" "mi_dedup_seed_b_success"

MEMORY_C=$(ee_workspace remember \
    "Graph steward maintenance should inspect pagerank centrality drift." \
    --level semantic --kind fact --json)
assert_jq "$MEMORY_C" '.success' "true" "mi_dedup_seed_c_success"

INDEX_JSON=$(ee_workspace index rebuild --json)
assert_jq "$INDEX_JSON" '.success' "true" "mi_dedup_index_rebuild_success"

CURATE_JSON=$(ee_workspace curate candidates --type dedup --json)
assert_jq "$CURATE_JSON" '.success' "true" "mi_dedup_curate_success"
CURATE_COUNT=$(printf '%s' "$CURATE_JSON" \
    | jq '[.data.candidates[]? | select(.candidateType == "paraphrase_dedup_proposal" or .kind == "paraphrase_dedup_proposal")] | length' 2>/dev/null \
    || echo 0)
if [ "$CURATE_COUNT" -gt 0 ]; then
    e2e_log_assert_eq "true" "true" "mi_dedup_curate_candidate_present"
else
    e2e_log_assert_eq "0" ">=1" "mi_dedup_curate_candidate_present"
fi

SEARCH_JSON=$(ee_workspace search "cargo fmt release" --dedupe=mi --relevance-floor 0.0 --json)
assert_jq "$SEARCH_JSON" '.success' "true" "mi_dedup_search_success"
MI_DEGRADED_COUNT=$(printf '%s' "$SEARCH_JSON" \
    | jq '[.data.degraded[]?.code // empty] | map(select(. == "mi_dedup_candidate_proposed")) | length' 2>/dev/null \
    || echo 0)
if [ "$MI_DEGRADED_COUNT" -gt 0 ]; then
    e2e_log_assert_eq "true" "true" "mi_dedup_search_degraded_code_present"
else
    e2e_log_assert_eq "0" ">=1" "mi_dedup_search_degraded_code_present"
fi

MERGED_FROM_COUNT=$(printf '%s' "$SEARCH_JSON" \
    | jq '[.data.results[]?.metadata.mergedFrom[]?] | length' 2>/dev/null \
    || echo 0)
if [ "$MERGED_FROM_COUNT" -gt 0 ]; then
    e2e_log_assert_eq "true" "true" "mi_dedup_search_merged_from_present"
else
    e2e_log_assert_eq "0" ">=1" "mi_dedup_search_merged_from_present"
fi

if [ "$EE_TEST_LOG_ASSERTS_FAIL" -ne 0 ]; then
    exit 1
fi
