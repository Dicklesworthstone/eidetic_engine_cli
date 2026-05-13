#!/usr/bin/env bash
# S8 - Consensus and contradiction surfacing e2e driver.
#
# Seeds query-relevant memories that trigger consensus plus all three
# conflict kinds, then asserts search/context JSON exposes the compact
# analysis arrays without mutating memories.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
epic_setup "epic_S_consensus_conflict"

remember_s8() {
    local content="${1:?content required}"
    local tag="${2:?tag required}"
    local valid_from="${3:-}"
    if [ -n "$valid_from" ]; then
        ee_workspace remember "$content" \
            --level procedural \
            --kind rule \
            --tags "$tag" \
            --valid-from "$valid_from" \
            --json >/dev/null
    else
        ee_workspace remember "$content" \
            --level procedural \
            --kind rule \
            --tags "$tag" \
            --json >/dev/null
    fi
}

remember_s8 "Run cargo fmt before release prep." "s8-consensus" "2026-05-01T00:00:00Z"
remember_s8 "Run cargo fmt before release prep." "s8-consensus" "2026-05-02T00:00:00Z"
remember_s8 "Run cargo fmt before release prep." "s8-consensus" "2026-05-03T00:00:00Z"

remember_s8 "Always use HTTPS for callback release prep." "s8-direct" "2026-05-04T00:00:00Z"
remember_s8 "Never use HTTPS for callback release prep." "s8-direct" "2026-05-05T00:00:00Z"

remember_s8 "Use API v1 for release prep service." "s8-stale" "2025-01-01T00:00:00Z"
remember_s8 "Use API v2 for release prep service." "s8-stale" "2026-05-01T00:00:00Z"

remember_s8 "Run cargo fmt before release prep." "s8-partial" "2026-05-06T00:00:00Z"
remember_s8 "Run cargo check before release prep." "s8-partial" "2026-05-07T00:00:00Z"

ee_workspace index rebuild --json >/dev/null

SEARCH_JSON=$(ee_workspace search "release prep" --limit 20 --json || true)
if ! printf '%s' "$SEARCH_JSON" | jq . >/dev/null 2>&1; then
    e2e_log_assert_eq "false" "true" "s8_search_json_parses"
    exit 0
fi

assert_jq "$SEARCH_JSON" '.data.consensus | length >= 1' "true" "s8_search_consensus_present"
assert_jq "$SEARCH_JSON" '.data.conflicts | length >= 3' "true" "s8_search_conflicts_present"
assert_jq "$SEARCH_JSON" \
    '[.data.conflicts[]?.kind] | index("direct") != null' \
    "true" \
    "s8_search_direct_conflict"
assert_jq "$SEARCH_JSON" \
    '[.data.conflicts[]?.kind] | index("stale_replacement") != null' \
    "true" \
    "s8_search_stale_conflict"
assert_jq "$SEARCH_JSON" \
    '[.data.conflicts[]?.kind] | index("partial_overlap") != null' \
    "true" \
    "s8_search_partial_conflict"
assert_jq "$SEARCH_JSON" \
    '[.data.results[]?.metadata | has("_ee_analysis_content")] | any' \
    "false" \
    "s8_search_internal_metadata_hidden"

consensus_conflict_signature() {
    jq -c '{consensus: .data.consensus, conflicts: .data.conflicts}' 2>/dev/null
}

SIG_1=$(printf '%s' "$SEARCH_JSON" | consensus_conflict_signature)
SIG_2=$(ee_workspace search "release prep" --limit 20 --json | consensus_conflict_signature)
SIG_3=$(ee_workspace search "release prep" --limit 20 --json | consensus_conflict_signature)
e2e_log_assert_eq "$SIG_1" "$SIG_2" "s8_search_determinism_run_1_2"
e2e_log_assert_eq "$SIG_2" "$SIG_3" "s8_search_determinism_run_2_3"

CONTEXT_JSON=$(ee_workspace context "release prep" --candidate-pool 20 --json || true)
if printf '%s' "$CONTEXT_JSON" | jq . >/dev/null 2>&1; then
    assert_jq "$CONTEXT_JSON" '.data | has("consensus")' "true" "s8_context_consensus_field"
    assert_jq "$CONTEXT_JSON" '.data | has("conflicts")' "true" "s8_context_conflicts_field"
else
    e2e_log_assert_eq "false" "true" "s8_context_json_parses"
fi
