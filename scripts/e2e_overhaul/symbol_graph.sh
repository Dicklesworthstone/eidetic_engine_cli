#!/usr/bin/env bash
# G-symbol - symbol graph contract e2e logging harness.
# Emits ee.test_event.v1 rows through scripts/e2e_overhaul/lib/shared.sh.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
epic_setup "symbol_graph"

symbol_phase_log() {
    local phase="${1:?phase required}"
    local surface="${2:?surface required}"
    local message="${3:?message required}"
    _e2e_emit_event "note" \
        "phase" "$phase" \
        "symbolScenario" "symbol_graph_contract" \
        "surface" "$surface" \
        "message" "$message"
}

mkdir -p "$EPIC_WORKSPACE/src"
SYMBOL_FIXTURE="$EPIC_WORKSPACE/src/symbol_graph_fixture.rs"
cat >"$SYMBOL_FIXTURE" <<'RS'
pub struct SymbolGraphFixture {
    pub value: u64,
}

impl SymbolGraphFixture {
    pub fn render_context_boost(&self) -> u64 {
        self.value + 1
    }
}
RS

symbol_phase_log "extraction" "symbol snapshot" "fixture=src/symbol_graph_fixture.rs expectedSchema=ee.symbol_snapshot.v1"
todo_assert "symbol_graph_extraction_logged" "bd-2xuu7.1" "ee symbol snapshot should emit ee.symbol_snapshot.v1 with no raw source body."

MEMORY_JSON="$(ee_workspace remember \
    "Symbol graph fixture: render_context_boost is the changed symbol for context boost." \
    --level semantic \
    --kind fact \
    --json 2>/dev/null || true)"
MEMORY_ID="$(printf '%s' "$MEMORY_JSON" | jq -r '.data.memory_id // empty' 2>/dev/null || true)"

symbol_phase_log "linking" "symbol evidence links" "memoryId=${MEMORY_ID:-missing} expectedSchema=ee.symbol_evidence_links.v1"
todo_assert "symbol_graph_linking_logged" "bd-2xuu7.2" "ee symbol link should map memory and CASS evidence to redaction-safe symbol ids."

CONTEXT_JSON="$(ee_workspace pack \
    "render_context_boost changed symbol" \
    --max-tokens 700 \
    --json 2>/dev/null || true)"
symbol_phase_log "context_boost" "context" "queryHash=symbol_graph_context_boost"
if printf '%s' "$CONTEXT_JSON" | jq . >/dev/null 2>&1; then
    assert_jq "$CONTEXT_JSON" '.success // false' "true" "symbol_graph_context_json_success"
else
    e2e_log_note "symbol_graph_context_json_unparseable bytes=${#CONTEXT_JSON}"
    todo_assert "symbol_graph_context_boost_logged" "bd-2xuu7.5" "ee context should expose symbol-boosted evidence once changed-symbol boosting lands."
fi

symbol_phase_log "why_explanation" "why" "memoryId=${MEMORY_ID:-missing}"
if [ -n "${MEMORY_ID:-}" ]; then
    WHY_JSON="$(ee_workspace why "$MEMORY_ID" --json 2>/dev/null || true)"
    if printf '%s' "$WHY_JSON" | jq . >/dev/null 2>&1; then
        assert_jq "$WHY_JSON" '.success // false' "true" "symbol_graph_why_json_success"
    else
        e2e_log_note "symbol_graph_why_json_unparseable bytes=${#WHY_JSON}"
        todo_assert "symbol_graph_why_explanation_logged" "bd-2xuu7.4" "ee why should explain symbol evidence links without raw source bodies."
    fi
else
    todo_assert "symbol_graph_why_memory_id_present" "bd-2xuu7.6" "remember should return a memory id for why explanation logging."
fi

e2e_log_note "symbol_graph_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} fixture=src/symbol_graph_fixture.rs"
if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
