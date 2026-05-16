#!/usr/bin/env bash
# G1.d - PPR graph e2e logging harness.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
START_SECONDS="$(python3 -c 'import time; print(time.monotonic())')"
epic_setup "g1_ppr"
seed_corpus
ee_workspace config set graph.feature.ppr.enabled true --json >/dev/null

e2e_log_note "g1_ppr_surface=context --ppr-weight --explain"
remember_ppr_fixture() {
    local content="${1:?content required}"
    ee_workspace remember --level semantic --kind note --no-auto-link "$content" --json 2>/dev/null \
        | jq -r '.data.memory.id // .data.memory_id // .data.id // empty' 2>/dev/null
}

PPR_SEED_ID="$(remember_ppr_fixture "G1 PPR fixture structural reranking seed release memory.")"
PPR_NEIGHBOR_ID="$(remember_ppr_fixture "G1 PPR fixture structural reranking neighbor release memory.")"
PPR_BASELINE_ID="$(remember_ppr_fixture "G1 PPR fixture structural reranking baseline release memory.")"
for memory_id in "$PPR_SEED_ID" "$PPR_NEIGHBOR_ID" "$PPR_BASELINE_ID"; do
    e2e_log_assert_num "${#memory_id}" -gt 0 "g1_ppr_seed_memory_id"
done

if [ -n "${PPR_SEED_ID:-}" ] && [ -n "${PPR_NEIGHBOR_ID:-}" ]; then
    ee_workspace link "$PPR_SEED_ID" "$PPR_NEIGHBOR_ID" --relation supports --json >/dev/null 2>&1 || true
    ee_workspace graph centrality-refresh --json >/dev/null 2>&1 || true
fi
e2e_log_note "g1_ppr_seed_memory=${PPR_SEED_ID:-missing} neighbor=${PPR_NEIGHBOR_ID:-missing} baseline=${PPR_BASELINE_ID:-missing}"

PPR_JSON=$(ee_workspace context "structural reranking ppr seed" --max-tokens 1000 --ppr-weight 1 --explain --json 2>/dev/null || true)
if printf '%s' "$PPR_JSON" | jq . >/dev/null 2>&1; then
    assert_jq_nonempty "$PPR_JSON" '.schema // empty' "g1_ppr_context_schema_present"
    PPR_BREAKDOWN_COUNT=$(printf '%s' "$PPR_JSON" | jq '[.. | objects | select(has("pprScore") or has("ppr_score"))] | length' 2>/dev/null || echo 0)
    e2e_log_assert_num "$PPR_BREAKDOWN_COUNT" -ge 1 "g1_ppr_score_breakdown_present" || true
    PPR_WHY_COUNT=$(printf '%s' "$PPR_JSON" | jq '[.. | objects | .why? // empty | select(test("Personalized PageRank"))] | length' 2>/dev/null || echo 0)
    e2e_log_assert_num "$PPR_WHY_COUNT" -ge 1 "g1_ppr_why_mentions_rerank" || true
    SNAPSHOT_VERSION=$(printf '%s' "$PPR_JSON" | jq -r '.. | objects | .snapshotVersion? // .snapshot_version? // empty' 2>/dev/null | head -n 1)
else
    todo_assert "g1_ppr_context_surface_available" "bd-ov09.3" "ee context --ppr-weight / --explain is not fully available yet."
    SNAPSHOT_VERSION="unavailable"
fi

if [ "${EE_GRAPH_E2E_INJECT_FAILURE:-0}" = "1" ]; then
    e2e_log_assert_eq "actual-ppr" "expected-ppr" "g1_ppr_injected_failure_diff" || true
fi

ELAPSED_MS="$(python3 -c "import time; print(int((time.monotonic() - float('$START_SECONDS')) * 1000))")"
e2e_log_note "g1_ppr_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} elapsed_ms=${ELAPSED_MS} snapshot_version=${SNAPSHOT_VERSION:-unavailable}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
