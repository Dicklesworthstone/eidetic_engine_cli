#!/usr/bin/env bash
# G9.d - Load-bearing memories graph e2e logging harness.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
START_SECONDS="$(python3 -c 'import time; print(time.monotonic())')"
epic_setup "g9_load_bearing"
seed_corpus

e2e_log_note "g9_load_bearing_surface=insights --section loadBearingMemories"
INSIGHTS_JSON=$(ee_workspace insights --section loadBearingMemories --json 2>/dev/null || true)
if printf '%s' "$INSIGHTS_JSON" | jq . >/dev/null 2>&1; then
    assert_jq_nonempty "$INSIGHTS_JSON" '.schema // empty' "g9_load_bearing_schema_present"
    assert_jq "$INSIGHTS_JSON" '.data.schema // empty' "ee.insights.v1" "g9_load_bearing_data_schema"
    assert_jq "$INSIGHTS_JSON" '.data.command // empty' "insights" "g9_load_bearing_command"
    assert_jq "$INSIGHTS_JSON" '.data.mode // empty' "section" "g9_load_bearing_section_mode"
    assert_jq "$INSIGHTS_JSON" '.data.selectedSection // empty' "loadBearingMemories" "g9_load_bearing_selected_section"
    assert_jq "$INSIGHTS_JSON" '(.data.availableSections // [] | index("loadBearingMemories") != null)' "true" "g9_load_bearing_available_section"
    assert_jq "$INSIGHTS_JSON" '(.data.sections | type)' "array" "g9_load_bearing_sections_array"
    assert_jq "$INSIGHTS_JSON" '(.data.degradedSignals | type)' "array" "g9_load_bearing_degraded_signals_array"
    assert_jq "$INSIGHTS_JSON" '.data.sections[0].name // empty' "loadBearingMemories" "g9_load_bearing_section_name"
    assert_jq_nonempty "$INSIGHTS_JSON" '.data.sections[0].title // empty' "g9_load_bearing_section_title"
    assert_jq_nonempty "$INSIGHTS_JSON" '.data.sections[0].summary // empty' "g9_load_bearing_section_summary"
    assert_jq "$INSIGHTS_JSON" '.data.sections[0].items | type' "array" "g9_load_bearing_items_array"
    LOAD_BEARING_ITEMS=$(printf '%s' "$INSIGHTS_JSON" | jq '(.data.sections[0].items // []) | length' 2>/dev/null || echo 0)
    e2e_log_assert_num "$LOAD_BEARING_ITEMS" -ge 1 "g9_load_bearing_item_present"
    SNAPSHOT_VERSION=$(printf '%s' "$INSIGHTS_JSON" | jq -r '.. | objects | .snapshotVersion? // .snapshot_version? // empty' 2>/dev/null | head -n 1)
else
    todo_assert "g9_load_bearing_surface_available" "bd-2jl2.4" "ee insights --section loadBearingMemories is not fully available yet."
    SNAPSHOT_VERSION="unavailable"
fi

if [ "${EE_GRAPH_E2E_INJECT_FAILURE:-0}" = "1" ]; then
    e2e_log_assert_eq "actual-load-bearing" "expected-load-bearing" "g9_load_bearing_injected_failure_diff" || true
fi

ELAPSED_MS="$(python3 -c "import time; print(int((time.monotonic() - float('$START_SECONDS')) * 1000))")"
e2e_log_note "g9_load_bearing_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} elapsed_ms=${ELAPSED_MS} snapshot_version=${SNAPSHOT_VERSION:-unavailable}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
