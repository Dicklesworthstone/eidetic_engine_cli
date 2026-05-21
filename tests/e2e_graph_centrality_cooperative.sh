#!/usr/bin/env bash
# bd-dre9v: no-mock cooperative centrality-refresh e2e.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$REPO_ROOT/scripts/e2e_overhaul/lib/shared.sh"

require_jq
START_SECONDS="$(python3 -c 'import time; print(time.monotonic())')"
epic_setup "graph_centrality_cooperative"

LINK_COUNT="${EE_COOPERATIVE_E2E_LINKS:-5000}"
BUDGET_MS="${EE_COOPERATIVE_E2E_BUDGET_MS:-5000}"
if [ "${EE_COOPERATIVE_E2E_SMOKE:-0}" = "1" ]; then
    LINK_COUNT="${EE_COOPERATIVE_E2E_SMOKE_LINKS:-50}"
fi

e2e_log_note "graph_centrality_cooperative_seed_links=$LINK_COUNT budget_ms=$BUDGET_MS"

MEMORY_IDS=()
index=0
while [ "$index" -le "$LINK_COUNT" ]; do
    payload="$(ee_workspace remember \
        "bd-dre9v cooperative graph fixture memory $index" \
        --level semantic \
        --kind note \
        --no-auto-link \
        --json 2>/dev/null)"
    memory_id="$(printf '%s' "$payload" | jq -r '.data.memory_id // .data.memory.id // .data.id // empty')"
    if [ -z "$memory_id" ]; then
        e2e_log_assert_num 0 -gt 0 "cooperative_seed_memory_id_present"
        exit 1
    fi
    MEMORY_IDS+=("$memory_id")
    index=$((index + 1))
done
e2e_log_assert_num "${#MEMORY_IDS[@]}" -eq "$((LINK_COUNT + 1))" "cooperative_seed_memory_count"

index=0
while [ "$index" -lt "$LINK_COUNT" ]; do
    ee_workspace link "${MEMORY_IDS[$index]}" "${MEMORY_IDS[$((index + 1))]}" \
        --relation supports \
        --json >/dev/null
    index=$((index + 1))
done

SEQUENTIAL_JSON="$(ee_workspace graph centrality-refresh --json)"
COOPERATIVE_JSON="$(ee_workspace graph centrality-refresh --cooperative --json)"

assert_jq "$SEQUENTIAL_JSON" '.schema // empty' "ee.graph.centrality_refresh.v1" "sequential_refresh_schema"
assert_jq "$COOPERATIVE_JSON" '.schema // empty' "ee.graph.centrality_refresh.v1" "cooperative_refresh_schema"
assert_jq "$SEQUENTIAL_JSON" '.data.status // empty' "refreshed" "sequential_refresh_status"
assert_jq "$COOPERATIVE_JSON" '.data.status // empty' "refreshed" "cooperative_refresh_status"
assert_jq "$COOPERATIVE_JSON" '.data.graph.edgeCount' "$LINK_COUNT" "cooperative_refresh_edge_count"
assert_jq "$COOPERATIVE_JSON" '(.data.topPagerank | length) > 0' "true" "cooperative_refresh_pagerank_converged"
assert_jq "$COOPERATIVE_JSON" '(.data.topBetweenness | length) > 0' "true" "cooperative_refresh_betweenness_converged"
assert_jq "$COOPERATIVE_JSON" '(.data.topHubs | length) > 0' "true" "cooperative_refresh_hubs_converged"
assert_jq "$COOPERATIVE_JSON" '(.data.topAuthorities | length) > 0' "true" "cooperative_refresh_authorities_converged"

COOPERATIVE_TOTAL_MS="$(printf '%s' "$COOPERATIVE_JSON" | jq -r '(.data.timing.totalMs // 0) | floor')"
e2e_log_assert_num "$COOPERATIVE_TOTAL_MS" -le "$BUDGET_MS" "cooperative_refresh_within_budget"

normalize_refresh_json() {
    jq -S -c '
      .data
      | del(.timing)
      | {
          status,
          dryRun,
          graph,
          scores,
          topPagerank,
          topBetweenness,
          topHubs,
          topAuthorities
        }
    '
}

sha256_stdin() {
    python3 -c 'import hashlib, sys; print(hashlib.sha256(sys.stdin.buffer.read()).hexdigest())'
}

SEQUENTIAL_HASH="$(printf '%s' "$SEQUENTIAL_JSON" | normalize_refresh_json | sha256_stdin)"
COOPERATIVE_HASH="$(printf '%s' "$COOPERATIVE_JSON" | normalize_refresh_json | sha256_stdin)"
e2e_log_assert_eq "$COOPERATIVE_HASH" "$SEQUENTIAL_HASH" "cooperative_refresh_matches_sequential_json_mod_timing"

ELAPSED_MS="$(python3 -c "import time; print(int((time.monotonic() - float('$START_SECONDS')) * 1000))")"
e2e_log_note "graph_centrality_cooperative_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} elapsed_ms=${ELAPSED_MS} total_ms=${COOPERATIVE_TOTAL_MS}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
