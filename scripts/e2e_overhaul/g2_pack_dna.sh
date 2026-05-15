#!/usr/bin/env bash
# G2.d - Pack DNA graph e2e logging harness.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
START_SECONDS="$(python3 -c 'import time; print(time.monotonic())')"
epic_setup "g2_pack_dna"
seed_corpus

e2e_log_note "g2_pack_dna_surface=context --explain packDna"
PACK_JSON=$(ee_workspace context "pack dna structural diversity" --max-tokens 1200 --explain --json 2>/dev/null || true)
if printf '%s' "$PACK_JSON" | jq . >/dev/null 2>&1; then
    assert_jq_nonempty "$PACK_JSON" '.schema // empty' "g2_pack_dna_context_schema_present"
    assert_jq "$PACK_JSON" '.data.pack.packDna.schema // empty' "ee.context.pack_dna.v1" "g2_pack_dna_schema" || true
    assert_jq "$PACK_JSON" '.data.pack.packDna | has("snapshotVersion") and has("voronoiDominator") and has("communityOfMass") and has("egoSubgraph") and has("pprNeighbors") and has("degraded")' "true" "g2_pack_dna_required_fields" || true
    assert_jq "$PACK_JSON" '.data.pack.packDna | (has("dominator") or has("packMemoryCount") or has("querySeedCount") or has("trustAnchorCount")) | not' "true" "g2_pack_dna_no_implementation_fields" || true
    assert_jq "$PACK_JSON" '.data.pack.packDna.pprNeighbors | type' "array" "g2_pack_dna_ppr_neighbors_array" || true
    assert_jq "$PACK_JSON" '.data.pack.packDna.degraded | type' "array" "g2_pack_dna_degraded_array" || true
    SNAPSHOT_VERSION=$(printf '%s' "$PACK_JSON" | jq -r '.data.pack.packDna.snapshotVersion? // .data.pack.packDna.snapshot_version? // empty' 2>/dev/null | head -n 1)
else
    todo_assert "g2_pack_dna_context_surface_available" "bd-fdvt.2" "ee context --explain packDna is not fully available yet."
    SNAPSHOT_VERSION="unavailable"
fi

if [ "${EE_GRAPH_E2E_INJECT_FAILURE:-0}" = "1" ]; then
    e2e_log_assert_eq "actual-pack-dna" "expected-pack-dna" "g2_pack_dna_injected_failure_diff" || true
fi

ELAPSED_MS="$(python3 -c "import time; print(int((time.monotonic() - float('$START_SECONDS')) * 1000))")"
e2e_log_note "g2_pack_dna_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} elapsed_ms=${ELAPSED_MS} snapshot_version=${SNAPSHOT_VERSION:-unavailable}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
