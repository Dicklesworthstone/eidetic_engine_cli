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
# EVERY graph.feature.*.enabled DEFAULTS TO FALSE -- all ten of them, in
# built_in_config at src/config/merge.rs:1304. Without this line the suite
# asserts enabled-feature behaviour against a workspace where the feature is
# off, and the nulls that come back read as a product regression. Measured
# 2026-10-19: 4 failed assertions here, all of that shape. g1/g8/g9 already
# carry the equivalent line; g9 is the control -- same default-false flag,
# enabled explicitly, 30 of 30 assertions green.
#
# THE FLAG IS ONE OF TWO INDEPENDENT WAYS THIS GRAPH GOES INERT, and they are
# easy to confuse. The other is an EMPTY graph: src/core/memory.rs:5633 records
# that plain `ee remember` used to produce zero links, "leaving the entire
# graph layer (PageRank/HITS/bridges/proximity/Pack DNA/skyline) dormant by
# default", which bd-pp1fk fixed with co-tag auto-linking. That comment is
# about link scarcity, NOT about these flags -- do not cite it for the
# defaults. A suite can therefore still read empty with the flag ON if its
# fixtures carry no links, which is the first thing to check before calling a
# residual failure a product defect.
ee_workspace config set graph.feature.pack_dna.enabled true --json >/dev/null

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
