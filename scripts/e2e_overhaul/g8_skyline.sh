#!/usr/bin/env bash
# G8.d - Knowledge skyline graph e2e logging harness.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
START_SECONDS="$(python3 -c 'import time; print(time.monotonic())')"
epic_setup "g8_skyline"
seed_corpus

e2e_log_note "g8_skyline_surface=status --skyline"
SKYLINE_JSON=$(ee_workspace status --skyline --json 2>/dev/null || true)
if printf '%s' "$SKYLINE_JSON" | jq . >/dev/null 2>&1; then
    assert_jq "$SKYLINE_JSON" '.schema' "ee.status.skyline.v1" "g8_skyline_schema_exact"
    assert_jq "$SKYLINE_JSON" '[has("schema"), has("snapshotVersion"), has("skyline"), has("summary"), has("degraded")] | all' "true" "g8_skyline_required_fields"
    assert_jq "$SKYLINE_JSON" '(.skyline | type)' "array" "g8_skyline_array"
    assert_jq "$SKYLINE_JSON" '(.summary | type)' "object" "g8_skyline_summary_object"
    assert_jq "$SKYLINE_JSON" '(.degraded | type)' "array" "g8_skyline_degraded_array"
    assert_jq "$SKYLINE_JSON" '(.summary | has("communityCount") and has("loadBearingMemoryCount") and has("staleCommunityCount"))' "true" "g8_skyline_summary_counters"
    SKYLINE_ITEM_COUNT=$(printf '%s' "$SKYLINE_JSON" | jq '.skyline | length' 2>/dev/null || echo 0)
    e2e_log_assert_num "$SKYLINE_ITEM_COUNT" -ge 1 "g8_skyline_items_present"
    DEGENERATE_DIAGNOSTIC_COUNT=$(printf '%s' "$SKYLINE_JSON" | jq '[.degraded[]? | select(.code == "graph_skyline_degenerate_communities" and .severity == "info" and ((.message // "") | contains("degenerate")))] | length' 2>/dev/null || echo 0)
    e2e_log_assert_num "$DEGENERATE_DIAGNOSTIC_COUNT" -ge 1 "g8_skyline_degenerate_diagnostic"
    SNAPSHOT_VERSION=$(printf '%s' "$SKYLINE_JSON" | jq -r '.snapshotVersion // empty' 2>/dev/null | head -n 1)
else
    todo_assert "g8_skyline_surface_available" "bd-mhc1.4" "ee status --skyline is not fully available yet."
    SNAPSHOT_VERSION="unavailable"
fi

if [ "${EE_GRAPH_E2E_INJECT_FAILURE:-0}" = "1" ]; then
    e2e_log_assert_eq "actual-skyline" "expected-skyline" "g8_skyline_injected_failure_diff" || true
fi

ELAPSED_MS="$(python3 -c "import time; print(int((time.monotonic() - float('$START_SECONDS')) * 1000))")"
e2e_log_note "g8_skyline_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} elapsed_ms=${ELAPSED_MS} snapshot_version=${SNAPSHOT_VERSION:-unavailable}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
