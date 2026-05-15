#!/usr/bin/env bash
# G7.d - Revision impact graph e2e logging harness.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
START_SECONDS="$(python3 -c 'import time; print(time.monotonic())')"
epic_setup "g7_revision_impact"
seed_corpus

e2e_log_note "g7_revision_impact_surface=memory revise --dry-run impactAnalysis"
ROOT_JSON=$(ee_workspace remember "G7 revision impact root." --level semantic --kind note --json 2>/dev/null || true)
ROOT_ID=$(printf '%s' "$ROOT_JSON" | jq -r '.data.public_id // .data.memory_id // .data.memory.id // .data.id // empty' 2>/dev/null || true)
e2e_log_note "g7_revision_impact_root_memory=${ROOT_ID:-missing}"

if [ -n "${ROOT_ID:-}" ]; then
    CHILD_JSON=$(ee_workspace memory revise "$ROOT_ID" --content "G7 persisted child revision." --json 2>/dev/null || true)
    CHILD_ID=$(printf '%s' "$CHILD_JSON" | jq -r '.data.new_id // .data.memory_id // .data.memory.id // empty' 2>/dev/null || true)
else
    CHILD_JSON=""
    CHILD_ID=""
fi
e2e_log_note "g7_revision_impact_child_memory=${CHILD_ID:-missing}"

if [ -n "${CHILD_ID:-}" ]; then
    IMPACT_JSON=$(ee_workspace memory revise "$CHILD_ID" --content "G7 dry-run impact preview." --dry-run --json 2>/dev/null || true)
else
    IMPACT_JSON=""
fi
if printf '%s' "$IMPACT_JSON" | jq . >/dev/null 2>&1; then
    assert_jq "$IMPACT_JSON" '.success // false' "true" "g7_revision_impact_dry_run_success"
    assert_jq "$IMPACT_JSON" '.data.impactAnalysis.schema // empty' "ee.memory.impact_analysis.v1" "g7_revision_impact_schema_present"
    assert_jq "$IMPACT_JSON" '.data.impactAnalysis | has("schema") and has("memoryId") and has("snapshotVersion") and has("revisionLineage") and has("impactAnalysis") and has("frontiers") and has("degraded")' "true" "g7_revision_impact_required_fields"
    assert_jq "$IMPACT_JSON" '.data.impactAnalysis.memoryId // empty' "$CHILD_ID" "g7_revision_impact_memory_id_matches_child"
    assert_jq "$IMPACT_JSON" '(.data.impactAnalysis.revisionLineage | type)' "array" "g7_revision_impact_lineage_array"
    assert_jq "$IMPACT_JSON" '(.data.impactAnalysis.impactAnalysis | type)' "object" "g7_revision_impact_analysis_object"
    assert_jq "$IMPACT_JSON" '(.data.impactAnalysis.degraded | type)' "array" "g7_revision_impact_degraded_array"
    assert_jq "$IMPACT_JSON" '.data.impactAnalysis.impactAnalysis.validationStatus // empty' "valid" "g7_revision_impact_validation_status_valid"
    assert_jq "$IMPACT_JSON" '.data.impactAnalysis.impactAnalysis.immediateDominator // empty' "$ROOT_ID" "g7_revision_impact_immediate_dominator_root"
    LINEAGE_COUNT=$(printf '%s' "$IMPACT_JSON" | jq '.data.impactAnalysis.revisionLineage | length' 2>/dev/null || echo 0)
    e2e_log_assert_num "$LINEAGE_COUNT" -ge 2 "g7_revision_impact_revision_lineage_depth"
    FRONTIER_TYPE=$(printf '%s' "$IMPACT_JSON" | jq -r '.data.impactAnalysis.frontiers | type' 2>/dev/null || true)
    e2e_log_assert_eq "$FRONTIER_TYPE" "array" "g7_revision_impact_frontiers_array"
    SNAPSHOT_VERSION=$(printf '%s' "$IMPACT_JSON" | jq -r '.data.impactAnalysis.snapshotVersion // empty' 2>/dev/null | head -n 1)
else
    todo_assert "g7_revision_impact_surface_available" "bd-a7mm.2" "ee memory revise --dry-run impactAnalysis did not return parseable JSON."
    SNAPSHOT_VERSION="unavailable"
fi

if [ "${EE_GRAPH_E2E_INJECT_FAILURE:-0}" = "1" ]; then
    e2e_log_assert_eq "actual-revision-impact" "expected-revision-impact" "g7_revision_impact_injected_failure_diff" || true
fi

ELAPSED_MS="$(python3 -c "import time; print(int((time.monotonic() - float('$START_SECONDS')) * 1000))")"
e2e_log_note "g7_revision_impact_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} elapsed_ms=${ELAPSED_MS} snapshot_version=${SNAPSHOT_VERSION:-unavailable}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
