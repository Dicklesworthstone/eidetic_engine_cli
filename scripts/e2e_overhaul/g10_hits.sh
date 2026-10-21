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
# The guard below cannot distinguish a payload from a refusal: `ee` writes its
# error envelope to STDOUT, so an error parses cleanly AND carries a top-level
# `schema`, and `jq .` additionally exits 0 on EMPTY input. Measured 2026-10-21
# on ee 0.18.0: a single error envelope here turned into 22 assertions logging
# `act=` with the cause recorded nowhere. Log the code beside them.
HUBS_ERR=$(printf '%s' "$HUBS_JSON" | jq -r '.error.code // empty' 2>/dev/null)
AUTHORITIES_ERR=$(printf '%s' "$AUTHORITIES_JSON" | jq -r '.error.code // empty' 2>/dev/null)
e2e_log_note "g10_hits_insights_error_codes=hubs:${HUBS_ERR:-none} authorities:${AUTHORITIES_ERR:-none}"
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
    # authorityRank is ["integer","null"] in docs/schemas/ee.why.v1.json for the
    # same reason hubRank is: a memory with no authority mass in THIS snapshot
    # has no authority rank. In the seeded workspace that is exactly what the
    # fixture authority becomes once seed_corpus supplies a denser component.
    # The strong form -- authorityRank == 1 -- is asserted in the isolated
    # workspace below, where the seeded pair really is the whole graph, so the
    # claim is moved rather than dropped.
    assert_jq "$WHY_JSON" '(.data.graph.hits.authorityRank == null) or (.data.graph.hits.authorityRank >= 1)' "true" "g10_hits_why_authority_rank_valid"
    # hubRank is declared ["integer","null"] by docs/schemas/ee.why.v1.json, and
    # a PURE AUTHORITY legitimately has no hub rank -- the seeded authority in
    # this fixture scores hub 0.0 with rank null. Asserting `>= 1` demanded a
    # value the schema explicitly permits to be absent, so it would stay red
    # against a correct implementation. Accept null OR a valid rank. bd-maisx.
    assert_jq "$WHY_JSON" '(.data.graph.hits.hubRank == null) or (.data.graph.hits.hubRank >= 1)' "true" "g10_hits_why_hub_rank_valid_or_absent"
    # Corpus-independent here: assert dominantRole is one of the values the
    # schema's enum permits. Measured act="weak" in the seeded workspace, which
    # is CORRECT output -- graph_hits_role_label returns "weak" when both
    # normalized scores fall below 0.5, and normalization is snapshot-wide.
    # Demanding "authority" here asserted a property of an isolated fixture
    # against a contaminated graph. The exact-role claim moves to the isolated
    # arm; this one still fails on an unknown or empty role.
    assert_jq "$WHY_JSON" '(.data.graph.hits.dominantRole) as $role | (["authority","hub","balanced","weak","unavailable"] | index($role)) != null' "true" "g10_hits_why_dominant_role"
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
    # `ee context` DOES echo its profile, in three places -- but not at either
    # of the two paths these assertions used to read. `.data.pack.profile` and
    # `.data.profile` do not exist, so all three failed against a surface that
    # reports the profile correctly. Measured 2026-10-20; the real paths are
    # .data.request.profile, .data.pack.selectionAudit.profile and
    # .data.pack.meta.algorithm.profile. Asserting all three together also
    # catches the two agreeing while the third drifts. bd-ol177.
    # Labels stay as literals rather than being built in a loop:
    # tests/e2e_graph_logging_scripts.rs pins these exact strings by substring
    # match against the script TEXT, so a constructed label silently breaks
    # that guard even though the suite still emits the right thing.
    assert_jq "$CONTEXT_GROUNDING_JSON" '.data.request.profile // empty' "grounding" "g10_hits_context_grounding_profile"
    assert_jq "$CONTEXT_ORIENTATION_JSON" '.data.request.profile // empty' "orientation" "g10_hits_context_orientation_profile"
    assert_jq "$CONTEXT_BALANCED_JSON" '.data.request.profile // empty' "balanced" "g10_hits_context_balanced_profile"
    # The same value is echoed in three independent places; asserting all three
    # catches two agreeing while the third drifts.
    assert_jq "$CONTEXT_GROUNDING_JSON" '.data.pack.selectionAudit.profile // empty' "grounding" "g10_hits_context_grounding_audit_profile"
    assert_jq "$CONTEXT_ORIENTATION_JSON" '.data.pack.selectionAudit.profile // empty' "orientation" "g10_hits_context_orientation_audit_profile"
    assert_jq "$CONTEXT_BALANCED_JSON" '.data.pack.selectionAudit.profile // empty' "balanced" "g10_hits_context_balanced_audit_profile"
    assert_jq "$CONTEXT_GROUNDING_JSON" '.data.pack.meta.algorithm.profile // empty' "grounding" "g10_hits_context_grounding_algorithm_profile"
    assert_jq "$CONTEXT_ORIENTATION_JSON" '.data.pack.meta.algorithm.profile // empty' "orientation" "g10_hits_context_orientation_algorithm_profile"
    assert_jq "$CONTEXT_BALANCED_JSON" '.data.pack.meta.algorithm.profile // empty' "balanced" "g10_hits_context_balanced_algorithm_profile"
    GROUNDING_FIRST=$(printf '%s' "$CONTEXT_GROUNDING_JSON" | jq -r '.data.pack.items[0].memoryId // .data.items[0].memoryId // empty' 2>/dev/null || true)
    ORIENTATION_FIRST=$(printf '%s' "$CONTEXT_ORIENTATION_JSON" | jq -r '.data.pack.items[0].memoryId // .data.items[0].memoryId // empty' 2>/dev/null || true)
    BALANCED_FIRST=$(printf '%s' "$CONTEXT_BALANCED_JSON" | jq -r '.data.pack.items[0].memoryId // .data.items[0].memoryId // empty' 2>/dev/null || true)
    assert_jq_nonempty "$CONTEXT_GROUNDING_JSON" '.data.pack.items[0].memoryId // .data.items[0].memoryId // empty' "g10_hits_context_grounding_first_present"
    assert_jq_nonempty "$CONTEXT_ORIENTATION_JSON" '.data.pack.items[0].memoryId // .data.items[0].memoryId // empty' "g10_hits_context_orientation_first_present"
    assert_jq_nonempty "$CONTEXT_BALANCED_JSON" '.data.pack.items[0].memoryId // .data.items[0].memoryId // empty' "g10_hits_context_balanced_first_present"
    # Kept, but it is NOT evidence that the HITS boost works. Measured
    # 2026-10-21 with graph.feature.hits_profiles.enabled=FALSE, grounding
    # STILL ranks the authority first, because base relevance already orders
    # the pair that way. An assertion that cannot fail when the feature is off
    # is a sanity check, not a proof.
    e2e_log_assert_eq "${GROUNDING_FIRST:-missing}" "$HITS_AUTHORITY_ID" "g10_hits_context_grounding_authority_ranked"
    if [ "${BALANCED_FIRST:-}" = "$HITS_AUTHORITY_ID" ] || [ "${BALANCED_FIRST:-}" = "$HITS_HUB_ID" ]; then
        e2e_log_assert_eq "seed" "seed" "g10_hits_context_balanced_seed_ranked"
    else
        e2e_log_assert_eq "${BALANCED_FIRST:-missing}" "${HITS_AUTHORITY_ID}-or-${HITS_HUB_ID}" "g10_hits_context_balanced_seed_ranked" || true
    fi
    # g10_hits_context_orientation_hub_ranked and g10_hits_context_profiles_differ
    # were asserted HERE and failed for something the product is not
    # responsible for: HITS normalization is snapshot-wide, so in the SEEDED
    # workspace this pair's normalized scores collapse toward 0 against
    # seed_corpus's denser component and the blend degenerates into an
    # order-preserving rescale of relevance. Both claims moved to the isolated
    # arm below, where the pair IS the whole graph and where a flag-off control
    # shows they are the only two that discriminate. bd-ol177.
    e2e_log_note "g10_hits_context_profile_order_claims=isolated_arm"
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
    ISO_HUBS_ERR=$(printf '%s' "$ISO_HUBS_JSON" | jq -r '.error.code // empty' 2>/dev/null)
    ISO_AUTHORITIES_ERR=$(printf '%s' "$ISO_AUTHORITIES_JSON" | jq -r '.error.code // empty' 2>/dev/null)
    e2e_log_note "g10_hits_isolated hubs=${ISO_HUB_COUNT} authorities=${ISO_AUTHORITY_COUNT} hubs_error=${ISO_HUBS_ERR:-none} authorities_error=${ISO_AUTHORITIES_ERR:-none}"

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

    # ---------------------------------------------------------------------
    # The `ee why` role claim, in the only workspace where it is meaningful.
    #
    # Measured 2026-10-21 on ee 0.18.0: in the SEEDED workspace this fixture's
    # authority reports dominantRole="weak" and a null authorityRank, and the
    # product is RIGHT. graph_hits_role_label (src/core/why.rs:3025) returns
    # "weak" when BOTH normalized scores are below 0.5, and normalization is
    # against the max over the whole snapshot -- so once seed_corpus supplies a
    # denser component, the fixture pair stops being the strong authority it is
    # in isolation. The seeded assertions below were encoding "the fixture is
    # the entire graph", which is only true here.
    #
    # The role and the exact rank are asserted BESIDE the uncontamination
    # preconditions above, so this cannot pass vacuously on an empty graph.
    # ---------------------------------------------------------------------
    ISO_WHY_JSON=$(ee_hits_isolated why "$ISO_AUTHORITY_ID" --json 2>/dev/null || true)
    if printf '%s' "$ISO_WHY_JSON" | jq . >/dev/null 2>&1; then
        assert_jq "$ISO_WHY_JSON" '.data.graph.hits != null' "true" "g10_hits_isolated_why_hits_block_present"
        assert_jq "$ISO_WHY_JSON" '.data.graph.hits.dominantRole // empty' "authority" "g10_hits_isolated_why_dominant_role_authority"
        assert_jq "$ISO_WHY_JSON" '(.data.graph.hits.authorityRank == 1)' "true" "g10_hits_isolated_why_authority_rank_first"
        assert_jq "$ISO_WHY_JSON" '(.data.graph.hits.authorityScore > 0)' "true" "g10_hits_isolated_why_authority_score_positive"
    else
        e2e_log_assert_eq "parseable" "unparseable" "g10_hits_isolated_why_available"
    fi

    # ---------------------------------------------------------------------
    # The profile-ordering claim, in the only workspace where it is
    # meaningful. Measured 2026-10-21 on ee 0.18.0 -- same fixture, same
    # binary, one variable:
    #
    #   flag=false   grounding [AUTH, HUB]   orientation [AUTH, HUB]   SAME
    #   flag=true    grounding [AUTH, HUB]   orientation [HUB, AUTH]   DIFFER
    #
    # So the orientation flip and the profile disagreement are produced BY the
    # HITS boost, with polarity -- not by the two profiles differing in their
    # other parameters. bd-ol177.
    # ---------------------------------------------------------------------
    ISO_GROUNDING_JSON=$(ee_hits_isolated context "G10 HITS fixture graph profile" --profile grounding --max-tokens 400 --json 2>/dev/null || true)
    ISO_ORIENTATION_JSON=$(ee_hits_isolated context "G10 HITS fixture graph profile" --profile orientation --max-tokens 400 --json 2>/dev/null || true)
    ISO_GROUNDING_FIRST=$(printf '%s' "$ISO_GROUNDING_JSON" | jq -r '.data.pack.items[0].memoryId // empty' 2>/dev/null || true)
    ISO_ORIENTATION_FIRST=$(printf '%s' "$ISO_ORIENTATION_JSON" | jq -r '.data.pack.items[0].memoryId // empty' 2>/dev/null || true)
    e2e_log_note "g10_hits_isolated_order grounding=${ISO_GROUNDING_FIRST:-missing} orientation=${ISO_ORIENTATION_FIRST:-missing}"
    # Empty-world guard: both packs must name a real memory before either
    # ordering claim below can mean anything.
    assert_jq_nonempty "$ISO_GROUNDING_JSON" '.data.pack.items[0].memoryId // empty' "g10_hits_isolated_context_grounding_first_present"
    assert_jq_nonempty "$ISO_ORIENTATION_JSON" '.data.pack.items[0].memoryId // empty' "g10_hits_isolated_context_orientation_first_present"
    e2e_log_assert_eq "${ISO_ORIENTATION_FIRST:-missing}" "$ISO_HUB_ID" "g10_hits_context_orientation_hub_ranked"
    if [ "${ISO_GROUNDING_FIRST:-}" = "${ISO_ORIENTATION_FIRST:-}" ]; then
        e2e_log_assert_eq "$ISO_GROUNDING_FIRST" "${ISO_ORIENTATION_FIRST}-different" "g10_hits_context_profiles_differ" || true
    else
        e2e_log_assert_eq "different" "different" "g10_hits_context_profiles_differ"
    fi
fi

if [ "${EE_GRAPH_E2E_INJECT_FAILURE:-0}" = "1" ]; then
    e2e_log_assert_eq "actual-hits" "expected-hits" "g10_hits_injected_failure_diff" || true
fi

ELAPSED_MS="$(python3 -c "import time; print(int((time.monotonic() - float('$START_SECONDS')) * 1000))")"
e2e_log_note "g10_hits_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} elapsed_ms=${ELAPSED_MS} snapshot_version=${SNAPSHOT_VERSION:-unavailable}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
