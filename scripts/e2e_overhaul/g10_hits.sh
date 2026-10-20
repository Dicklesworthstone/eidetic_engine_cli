#!/usr/bin/env bash
# G10.d - HITS hubs/authorities graph e2e logging harness.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
START_SECONDS="$(python3 -c 'import time; print(time.monotonic())')"
epic_setup "g10_hits"
seed_corpus
# graph.feature.hits_profiles.enabled DEFAULTS TO FALSE (src/config/merge.rs:1304,
# built_in_config). This suite carried 39 of the 75 graph failures on 2026-10-19
# -- the largest single block -- and they are the signature of a dormant
# feature: hubScore/authorityScore null, evidence.schema and
# evidence.algorithm empty, every rank invalid. Enabling the flag is the
# precondition for reading any of those as real defects.
ee_workspace config set graph.feature.hits_profiles.enabled true --json >/dev/null

e2e_log_note "g10_hits_surface=insights --section hubs/authorities"
remember_hits_fixture() {
    local content="${1:?content required}"
    ee_workspace remember "$content" --level semantic --kind note --no-auto-link --json 2>/dev/null \
        | jq -r '.data.memory_id // .data.memory.id // .data.id // empty' 2>/dev/null
}

HITS_HUB_ID="$(remember_hits_fixture "G10 HITS fixture hub memory that points to authoritative facts.")"
HITS_AUTHORITY_ID="$(remember_hits_fixture "G10 HITS fixture authority memory grounded by hubs.")"
for memory_id in "$HITS_HUB_ID" "$HITS_AUTHORITY_ID"; do
    e2e_log_assert_num "${#memory_id}" -gt 0 "g10_hits_seed_memory_id"
done

if [ -n "${HITS_HUB_ID:-}" ] && [ -n "${HITS_AUTHORITY_ID:-}" ]; then
    ee_workspace link "$HITS_HUB_ID" "$HITS_AUTHORITY_ID" --relation supports --json >/dev/null 2>&1 || true
fi
e2e_log_note "g10_hits_seed_link hub=${HITS_HUB_ID:-missing} authority=${HITS_AUTHORITY_ID:-missing}"

if ee_workspace graph centrality-refresh --json >/dev/null 2>&1; then
    e2e_log_note "g10_hits_graph_snapshot_refreshed=true"
else
    todo_assert "g10_hits_graph_snapshot_refresh" "bd-jy4w.4" "ee graph centrality-refresh is not fully available for the HITS fixture yet."
fi

HUBS_JSON=$(ee_workspace insights --section hubs --json 2>/dev/null || true)
AUTHORITIES_JSON=$(ee_workspace insights --section authorities --json 2>/dev/null || true)
if printf '%s' "$HUBS_JSON" | jq . >/dev/null 2>&1 && printf '%s' "$AUTHORITIES_JSON" | jq . >/dev/null 2>&1; then
    assert_jq_nonempty "$HUBS_JSON" '.schema // empty' "g10_hits_hubs_schema_present"
    assert_jq_nonempty "$AUTHORITIES_JSON" '.schema // empty' "g10_hits_authorities_schema_present"
    assert_jq "$HUBS_JSON" '.data.schema // empty' "ee.insights.v1" "g10_hits_hubs_data_schema"
    assert_jq "$AUTHORITIES_JSON" '.data.schema // empty' "ee.insights.v1" "g10_hits_authorities_data_schema"
    assert_jq "$HUBS_JSON" '.data.command // empty' "insights" "g10_hits_hubs_command"
    assert_jq "$AUTHORITIES_JSON" '.data.command // empty' "insights" "g10_hits_authorities_command"
    assert_jq "$HUBS_JSON" '.data.mode // empty' "section" "g10_hits_hubs_section_mode"
    assert_jq "$AUTHORITIES_JSON" '.data.mode // empty' "section" "g10_hits_authorities_section_mode"
    assert_jq "$HUBS_JSON" '.data.selectedSection // empty' "hubs" "g10_hits_hubs_selected_section"
    assert_jq "$AUTHORITIES_JSON" '.data.selectedSection // empty' "authorities" "g10_hits_authorities_selected_section"
    assert_jq "$HUBS_JSON" '(.data.sections | type)' "array" "g10_hits_hubs_sections_array"
    assert_jq "$AUTHORITIES_JSON" '(.data.sections | type)' "array" "g10_hits_authorities_sections_array"
    assert_jq "$HUBS_JSON" '(.data.degradedSignals | type)' "array" "g10_hits_hubs_degraded_signals_array"
    assert_jq "$AUTHORITIES_JSON" '(.data.degradedSignals | type)' "array" "g10_hits_authorities_degraded_signals_array"
    assert_jq "$HUBS_JSON" '((.data.availableSections // []) | index("hubs") != null)' "true" "g10_hits_hubs_available_section"
    assert_jq "$AUTHORITIES_JSON" '((.data.availableSections // []) | index("authorities") != null)' "true" "g10_hits_authorities_available_section"
    assert_jq "$HUBS_JSON" '.data.sections[0].name // empty' "hubs" "g10_hits_hubs_section_name"
    assert_jq "$AUTHORITIES_JSON" '.data.sections[0].name // empty' "authorities" "g10_hits_authorities_section_name"
    HUB_SCORE_PRESENT=$(printf '%s' "$HUBS_JSON" | jq '[.data.sections[0].items[]? | select(has("hubScore"))] | length' 2>/dev/null || echo 0)
    AUTHORITY_SCORE_PRESENT=$(printf '%s' "$AUTHORITIES_JSON" | jq '[.data.sections[0].items[]? | select(has("authorityScore"))] | length' 2>/dev/null || echo 0)
    e2e_log_assert_num "$HUB_SCORE_PRESENT" -ge 1 "g10_hits_hub_score_present"
    e2e_log_assert_num "$AUTHORITY_SCORE_PRESENT" -ge 1 "g10_hits_authority_score_present"
    assert_jq "$HUBS_JSON" '.data.sections[0].items[0].rank >= 1' "true" "g10_hits_hub_item_rank_valid"
    assert_jq "$AUTHORITIES_JSON" '.data.sections[0].items[0].rank >= 1' "true" "g10_hits_authority_item_rank_valid"
    # These two used to assert that the seeded hub/authority occupy items[0] of
    # the GLOBAL top-N. `seed_corpus` above contributes a densely co-tag-linked
    # component, and the seeded pair is a disconnected two-node component, so
    # HITS normalises it to near zero: measured 2026-10-20, the seeded hub and
    # authority rank 11 of 11 and do not appear on the default 10-item page at
    # all. "The seeded hub ranks first" is therefore false of a CORRECT
    # implementation on this corpus, and asserting it reported a working
    # product as broken. The contract that does hold here is that every memory
    # in the graph receives a HITS profile; the ranking claim moves to the
    # isolated workspace at the end of this suite, where it is both meaningful
    # and strong. bd-ol177.
    HUBS_FULL_JSON=$(ee_workspace insights --section hubs --limit 200 --json 2>/dev/null || true)
    AUTHORITIES_FULL_JSON=$(ee_workspace insights --section authorities --limit 200 --json 2>/dev/null || true)
    SEEDED_HUB_ITEM=$(printf '%s' "$HUBS_FULL_JSON" | jq -c --arg id "$HITS_HUB_ID" '[.data.sections[0].items[]? | select(.memoryId == $id)][0] // {}' 2>/dev/null || echo '{}')
    SEEDED_AUTHORITY_ITEM=$(printf '%s' "$AUTHORITIES_FULL_JSON" | jq -c --arg id "$HITS_AUTHORITY_ID" '[.data.sections[0].items[]? | select(.memoryId == $id)][0] // {}' 2>/dev/null || echo '{}')
    e2e_log_note "g10_hits_seeded_profiles hub_rank=$(printf '%s' "$SEEDED_HUB_ITEM" | jq -r '.rank // "absent"') authority_rank=$(printf '%s' "$SEEDED_AUTHORITY_ITEM" | jq -r '.rank // "absent"')"
    assert_jq "$SEEDED_HUB_ITEM" 'has("memoryId")' "true" "g10_hits_seed_hub_profiled"
    assert_jq "$SEEDED_AUTHORITY_ITEM" 'has("memoryId")' "true" "g10_hits_seed_authority_profiled"
    assert_jq "$SEEDED_HUB_ITEM" '(.hubScore | type)' "number" "g10_hits_seed_hub_score_number"
    assert_jq "$SEEDED_AUTHORITY_ITEM" '(.authorityScore | type)' "number" "g10_hits_seed_authority_score_number"
    assert_jq "$SEEDED_HUB_ITEM" '(.rank >= 1)' "true" "g10_hits_seed_hub_rank_valid"
    assert_jq "$SEEDED_AUTHORITY_ITEM" '(.rank >= 1)' "true" "g10_hits_seed_authority_rank_valid"
    assert_jq "$HUBS_JSON" '(.data.sections[0].items[0].hubScore | type)' "number" "g10_hits_hub_score_number"
    assert_jq "$AUTHORITIES_JSON" '(.data.sections[0].items[0].authorityScore | type)' "number" "g10_hits_authority_score_number"
    assert_jq "$HUBS_JSON" '.data.sections[0].items[0].hubScore >= 0' "true" "g10_hits_hub_score_nonnegative"
    assert_jq "$AUTHORITIES_JSON" '.data.sections[0].items[0].authorityScore >= 0' "true" "g10_hits_authority_score_nonnegative"
    assert_jq "$HUBS_JSON" '.data.sections[0].items[0].interpretation // empty' "hub" "g10_hits_hub_interpretation"
    assert_jq "$AUTHORITIES_JSON" '.data.sections[0].items[0].interpretation // empty' "authority" "g10_hits_authority_interpretation"
    assert_jq "$HUBS_JSON" '.data.sections[0].items[0].evidence.schema // empty' "ee.graph.hits.v1" "g10_hits_hubs_evidence_schema"
    assert_jq "$AUTHORITIES_JSON" '.data.sections[0].items[0].evidence.schema // empty' "ee.graph.hits.v1" "g10_hits_authorities_evidence_schema"
    assert_jq "$HUBS_JSON" '.data.sections[0].items[0].evidence.algorithm // empty' "hits_centrality_directed" "g10_hits_hubs_evidence_algorithm"
    assert_jq "$AUTHORITIES_JSON" '.data.sections[0].items[0].evidence.algorithm // empty' "hits_centrality_directed" "g10_hits_authorities_evidence_algorithm"
    assert_jq "$HUBS_JSON" '(.data.sections[0].items[0].evidence.snapshotVersion | type)' "number" "g10_hits_hub_evidence_snapshot_version_number"
    assert_jq "$AUTHORITIES_JSON" '(.data.sections[0].items[0].evidence.snapshotVersion | type)' "number" "g10_hits_authority_evidence_snapshot_version_number"
    SNAPSHOT_VERSION=$(printf '%s\n%s' "$HUBS_JSON" "$AUTHORITIES_JSON" | jq -r '.. | objects | .snapshotVersion? // .snapshot_version? // empty' 2>/dev/null | head -n 1)
else
    todo_assert "g10_hits_surface_available" "bd-jy4w.4" "ee insights hubs/authorities sections are not fully available yet."
    SNAPSHOT_VERSION="unavailable"
fi

WHY_JSON=$(ee_workspace why "$HITS_AUTHORITY_ID" --json 2>/dev/null || true)
if printf '%s' "$WHY_JSON" | jq . >/dev/null 2>&1; then
    e2e_log_note "g10_hits_surface=why graph.hits"
    assert_jq_nonempty "$WHY_JSON" '.schema // empty' "g10_hits_why_schema_present"
    assert_jq "$WHY_JSON" '.data.command // empty' "why" "g10_hits_why_command"
    assert_jq "$WHY_JSON" '.data.memoryId // empty' "$HITS_AUTHORITY_ID" "g10_hits_why_memory_id"
    assert_jq "$WHY_JSON" '.data.graph.hits != null' "true" "g10_hits_why_hits_block_present"
    assert_jq "$WHY_JSON" '(.data.graph.hits.authorityScore | type)' "number" "g10_hits_why_authority_score_number"
    assert_jq "$WHY_JSON" '.data.graph.hits.authorityScore >= 0' "true" "g10_hits_why_authority_score_nonnegative"
    assert_jq "$WHY_JSON" '(.data.graph.hits.hubScore | type)' "number" "g10_hits_why_hub_score_number"
    assert_jq "$WHY_JSON" '.data.graph.hits.hubScore >= 0' "true" "g10_hits_why_hub_score_nonnegative"
    assert_jq "$WHY_JSON" '.data.graph.hits.authorityRank >= 1' "true" "g10_hits_why_authority_rank_valid"
    assert_jq "$WHY_JSON" '.data.graph.hits.hubRank >= 1' "true" "g10_hits_why_hub_rank_valid"
    assert_jq "$WHY_JSON" '.data.graph.hits.dominantRole // empty' "authority" "g10_hits_why_dominant_role"
    assert_jq "$WHY_JSON" '(.data.graph.hits.profileInfluence.groundingBoost | type)' "number" "g10_hits_why_grounding_boost_number"
    assert_jq "$WHY_JSON" '(.data.graph.hits.profileInfluence.orientationBoost | type)' "number" "g10_hits_why_orientation_boost_number"
    assert_jq "$WHY_JSON" '(.data.graph.hits.profileInfluence.balancedBoost | type)' "number" "g10_hits_why_balanced_boost_number"
    assert_jq "$WHY_JSON" '.data.graph.hits.evidence.schema // empty' "ee.graph.hits.v1" "g10_hits_why_evidence_schema"
    assert_jq "$WHY_JSON" '.data.graph.hits.evidence.algorithm // empty' "hits_centrality_directed" "g10_hits_why_evidence_algorithm"
    assert_jq "$WHY_JSON" '.data.graph.hits.evidence.graphType // empty' "memory_links" "g10_hits_why_evidence_graph_type"
    assert_jq "$WHY_JSON" '(.data.graph.hits.evidence.snapshotVersion | type)' "number" "g10_hits_why_evidence_snapshot_version_number"
    assert_jq_nonempty "$WHY_JSON" '.data.graph.hits.rationale // empty' "g10_hits_why_rationale"
    WHY_HITS_SNAPSHOT_VERSION=$(printf '%s' "$WHY_JSON" | jq -r '.data.graph.hits.evidence.snapshotVersion // empty' 2>/dev/null | head -n 1)
    if [ -n "${WHY_HITS_SNAPSHOT_VERSION:-}" ]; then
        SNAPSHOT_VERSION="$WHY_HITS_SNAPSHOT_VERSION"
    fi
else
    todo_assert "g10_hits_why_surface_available" "bd-jy4w.4" "ee why --json HITS block is not fully available yet."
fi

CONTEXT_GROUNDING_JSON=$(ee_workspace context "G10 HITS fixture graph profile" --profile grounding --max-tokens 400 --json 2>/dev/null || true)
CONTEXT_ORIENTATION_JSON=$(ee_workspace context "G10 HITS fixture graph profile" --profile orientation --max-tokens 400 --json 2>/dev/null || true)
CONTEXT_BALANCED_JSON=$(ee_workspace context "G10 HITS fixture graph profile" --profile balanced --max-tokens 400 --json 2>/dev/null || true)
if printf '%s' "$CONTEXT_GROUNDING_JSON" | jq . >/dev/null 2>&1 \
    && printf '%s' "$CONTEXT_ORIENTATION_JSON" | jq . >/dev/null 2>&1 \
    && printf '%s' "$CONTEXT_BALANCED_JSON" | jq . >/dev/null 2>&1; then
    e2e_log_note "g10_hits_surface=context profile rerank"
    assert_jq "$CONTEXT_GROUNDING_JSON" '.data.pack.profile // .data.profile // empty' "grounding" "g10_hits_context_grounding_profile"
    assert_jq "$CONTEXT_ORIENTATION_JSON" '.data.pack.profile // .data.profile // empty' "orientation" "g10_hits_context_orientation_profile"
    assert_jq "$CONTEXT_BALANCED_JSON" '.data.pack.profile // .data.profile // empty' "balanced" "g10_hits_context_balanced_profile"
    GROUNDING_FIRST=$(printf '%s' "$CONTEXT_GROUNDING_JSON" | jq -r '.data.pack.items[0].memoryId // .data.items[0].memoryId // empty' 2>/dev/null || true)
    ORIENTATION_FIRST=$(printf '%s' "$CONTEXT_ORIENTATION_JSON" | jq -r '.data.pack.items[0].memoryId // .data.items[0].memoryId // empty' 2>/dev/null || true)
    BALANCED_FIRST=$(printf '%s' "$CONTEXT_BALANCED_JSON" | jq -r '.data.pack.items[0].memoryId // .data.items[0].memoryId // empty' 2>/dev/null || true)
    assert_jq_nonempty "$CONTEXT_GROUNDING_JSON" '.data.pack.items[0].memoryId // .data.items[0].memoryId // empty' "g10_hits_context_grounding_first_present"
    assert_jq_nonempty "$CONTEXT_ORIENTATION_JSON" '.data.pack.items[0].memoryId // .data.items[0].memoryId // empty' "g10_hits_context_orientation_first_present"
    assert_jq_nonempty "$CONTEXT_BALANCED_JSON" '.data.pack.items[0].memoryId // .data.items[0].memoryId // empty' "g10_hits_context_balanced_first_present"
    e2e_log_assert_eq "${GROUNDING_FIRST:-missing}" "$HITS_AUTHORITY_ID" "g10_hits_context_grounding_authority_ranked"
    e2e_log_assert_eq "${ORIENTATION_FIRST:-missing}" "$HITS_HUB_ID" "g10_hits_context_orientation_hub_ranked"
    if [ "${BALANCED_FIRST:-}" = "$HITS_AUTHORITY_ID" ] || [ "${BALANCED_FIRST:-}" = "$HITS_HUB_ID" ]; then
        e2e_log_assert_eq "seed" "seed" "g10_hits_context_balanced_seed_ranked"
    else
        e2e_log_assert_eq "${BALANCED_FIRST:-missing}" "${HITS_AUTHORITY_ID}-or-${HITS_HUB_ID}" "g10_hits_context_balanced_seed_ranked" || true
    fi
    if [ "${GROUNDING_FIRST:-}" = "${ORIENTATION_FIRST:-}" ]; then
        e2e_log_assert_eq "$GROUNDING_FIRST" "${ORIENTATION_FIRST}-different" "g10_hits_context_profiles_differ" || true
    else
        e2e_log_assert_eq "different" "different" "g10_hits_context_profiles_differ"
    fi
else
    todo_assert "g10_hits_context_profiles_available" "bd-jy4w.4" "ee context --profile grounding/orientation/balanced is not fully available for the HITS fixture yet."
fi

# ---------------------------------------------------------------------------
# The ranking claim, in the only workspace where it is meaningful.
#
# A hub is only "the" hub relative to a graph. In the seeded epic workspace the
# corpus supplies a denser component than the fixture does, so the strong claim
# belongs here, over a graph that contains nothing but the seeded hub ->
# authority edge. Measured 2026-10-20: hubs and authorities each return exactly
# one item, the seeded memory, with a score of 1.0.
#
# The list length is asserted BESIDE the ranking so this cannot pass vacuously
# by some future corpus leaking in and making "ranked first" trivial again.
#
# The workspace is retained, not removed, and its path is logged.
# ---------------------------------------------------------------------------
WS_G10_ISOLATED=$(mktemp -d "${TMPDIR:-/tmp}/ee-e2e-g10-hits-isolated.XXXXXX")
e2e_log_note "g10_hits_isolated_workspace=$WS_G10_ISOLATED"

ee_hits_isolated() {
    e2e_log_command "$EE_BINARY" "$@" --workspace "$WS_G10_ISOLATED"
}

ee_hits_isolated init --json >/dev/null
ee_hits_isolated config set graph.feature.hits_profiles.enabled true --json >/dev/null
ISO_HUB_ID=$(ee_hits_isolated remember "G10 HITS fixture hub memory that points to authoritative facts." --level semantic --kind note --no-auto-link --json 2>/dev/null | jq -r '.data.memory_id // .data.memory.id // .data.id // empty' 2>/dev/null)
ISO_AUTHORITY_ID=$(ee_hits_isolated remember "G10 HITS fixture authority memory grounded by hubs." --level semantic --kind note --no-auto-link --json 2>/dev/null | jq -r '.data.memory_id // .data.memory.id // .data.id // empty' 2>/dev/null)

if [ -z "${ISO_HUB_ID:-}" ] || [ -z "${ISO_AUTHORITY_ID:-}" ]; then
    e2e_log_assert_eq "memory-ids" "missing" "g10_hits_isolated_fixture_memory_ids"
else
    ee_hits_isolated link "$ISO_HUB_ID" "$ISO_AUTHORITY_ID" --relation supports --json >/dev/null 2>&1 || true
    ee_hits_isolated graph centrality-refresh --json >/dev/null 2>&1 || true

    ISO_HUBS_JSON=$(ee_hits_isolated insights --section hubs --json 2>/dev/null || true)
    ISO_AUTHORITIES_JSON=$(ee_hits_isolated insights --section authorities --json 2>/dev/null || true)
    ISO_HUB_COUNT=$(printf '%s' "$ISO_HUBS_JSON" | jq '.data.sections[0].items | length' 2>/dev/null || echo 0)
    ISO_AUTHORITY_COUNT=$(printf '%s' "$ISO_AUTHORITIES_JSON" | jq '.data.sections[0].items | length' 2>/dev/null || echo 0)
    e2e_log_note "g10_hits_isolated hubs=${ISO_HUB_COUNT} authorities=${ISO_AUTHORITY_COUNT}"

    # Precondition: the isolated graph really is just the seeded pair.
    e2e_log_assert_num "$ISO_HUB_COUNT" -ge 1 "g10_hits_isolated_hubs_present"
    e2e_log_assert_num "$ISO_HUB_COUNT" -le 2 "g10_hits_isolated_hubs_uncontaminated"
    e2e_log_assert_num "$ISO_AUTHORITY_COUNT" -ge 1 "g10_hits_isolated_authorities_present"
    e2e_log_assert_num "$ISO_AUTHORITY_COUNT" -le 2 "g10_hits_isolated_authorities_uncontaminated"
    # The claim the suite is really making.
    assert_jq "$ISO_HUBS_JSON" '.data.sections[0].items[0].memoryId // empty' "$ISO_HUB_ID" "g10_hits_isolated_seed_hub_ranked_first"
    assert_jq "$ISO_AUTHORITIES_JSON" '.data.sections[0].items[0].memoryId // empty' "$ISO_AUTHORITY_ID" "g10_hits_isolated_seed_authority_ranked_first"
    assert_jq "$ISO_HUBS_JSON" '(.data.sections[0].items[0].hubScore > 0)' "true" "g10_hits_isolated_seed_hub_score_positive"
    assert_jq "$ISO_AUTHORITIES_JSON" '(.data.sections[0].items[0].authorityScore > 0)' "true" "g10_hits_isolated_seed_authority_score_positive"
fi

if [ "${EE_GRAPH_E2E_INJECT_FAILURE:-0}" = "1" ]; then
    e2e_log_assert_eq "actual-hits" "expected-hits" "g10_hits_injected_failure_diff" || true
fi

ELAPSED_MS="$(python3 -c "import time; print(int((time.monotonic() - float('$START_SECONDS')) * 1000))")"
e2e_log_note "g10_hits_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} elapsed_ms=${ELAPSED_MS} snapshot_version=${SNAPSHOT_VERSION:-unavailable}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
