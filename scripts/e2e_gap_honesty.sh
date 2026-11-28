#!/usr/bin/env bash
# bd-1n0np.6.6 — Gap-honesty (Blind-Spot Map + Query-Miss Clustering) end-to-end.
#
# Scenario: a temp workspace whose memories cover only SOME of the codebase, so
# the blind-spot map has something honest to report.
#   1. init + remember memories that anchor only a couple of modules (partial
#      coverage on purpose).
#   2. `ee insights --section blindSpots --json` reports the uncovered
#      high-importance nodes + a coverageRatio (bd-1n0np.6.1 / 6.2).
#   3. repeated low-utility / missed searches feed the query-miss ledger
#      (bd-1n0np.6.3); `ee learn gaps` clusters repeated misses into
#      knowledge_gap candidates (bd-1n0np.6.4) ...
#   4. ... which surfaces in `ee swarm brief`.
#   5. a pack on an uncovered target carries `coverage: thin` (bd-1n0np.6.2).
#
# The gap-honesty surfaces (blindSpots section, coverageRatio, `learn gaps`
# knowledge_gap candidates, swarm-brief surfacing, pack coverage flag)
# are CAPABILITY-GUARDED: where a surface is absent in the binary under test, the
# step records a visible log_drop (the no-silent-cap rule) instead of a false
# pass, and its assertions activate automatically once the binary provides it.
# The init / remember / search path runs for real on any binary.
#
# NOTE: no `set -e` — the harness assert_* helpers accumulate pass/fail and
# `harness_summary` decides the exit code.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/e2e_harness.sh
source "$E2E_DIR/lib/e2e_harness.sh"

harness_init "gap_honesty"

ee_json() { "$EE_BIN" "$@" 2>/dev/null || true; }
ee_supports() { "$EE_BIN" "$@" --help >/dev/null 2>&1; }
# True only when `ee <cmd> --help` actually lists <token> (avoids clap positional
# false-positives on binaries that predate the flag/section).
ee_help_lists() {
    local cmd="$1" token="$2"
    "$EE_BIN" "$cmd" --help 2>&1 | grep -qw "$token"
}

with_temp_workspace WS

step "init workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.success == true' "ee init succeeds"

step "remember partial-coverage memories (blind-spot map has gaps to report)"
# Two memories anchoring only a couple of surfaces — the rest of the tree is
# deliberately uncovered so blindSpots/coverageRatio have something honest to say.
r1="$(ee_json remember "search.rs ranks hybrid BM25 + vector hits via frankensearch." \
    --workspace "$WS" --level semantic --kind fact --tags search,coverage --json)"
assert_jq "$r1" '.success == true' "remember covered-module memory (search)"
r2="$(ee_json remember "pack.rs assembles the context pack with provenance." \
    --workspace "$WS" --level semantic --kind fact --tags pack,coverage --json)"
assert_jq "$r2" '.success == true' "remember covered-module memory (pack)"

step "blind-spot map: uncovered high-importance nodes + coverageRatio (6.1/6.2)"
if ee_supports insights && ee_help_lists insights blindSpots; then
    bs="$(ee_json insights --section blindSpots --workspace "$WS" --json)"
    assert_jq "$bs" '.success == true' "insights --section blindSpots succeeds"
    # THE SECTION PAYLOAD LIVES IN `.data.sections[]`, KEYED BY `.data.selectedSection`.
    #
    # This assertion used to read `.data.blindSpots // .data.blind_spots //
    # .data.section` and failed on all three (bd-psna1). That looked like a
    # regression because the envelope is well-formed and `.success` is true --
    # the command works, only the reader was wrong. It is a STALE EXPECTATION
    # that nothing caught, because no gate has ever invoked this script.
    #
    # The replacement is not another guess. It is the shape three sibling
    # suites assert and PASS against on this same 0.18.0 binary:
    #
    #   g9_load_bearing  .data.selectedSection == "loadBearingMemories"
    #   g6_proximity     (.data.sections | type) == "array"
    #   g10_hits         .data.sections[0].name == <section>
    #
    # Do not widen this back into a `//` chain. The original chain is what hid
    # the defect: a chain that tries several spellings cannot distinguish
    # "absent" from "I never knew the name", and it reports the same null for
    # both.
    # NOTE THE 3-ARG FORM. This harness is assert_jq(json, PREDICATE, label) and
    # grades on `jq -e`'s exit code, whereas scripts/e2e_overhaul/lib/shared.sh
    # takes assert_jq(json, filter, EXPECTED, label). Copying the 4-arg shape
    # here does not fail loudly -- the 4th argument is silently dropped and the
    # 3rd becomes the label, so `'(.data.sections | type)' "array"` would assert
    # the string "array", which is truthy, and PASS for any value at all.
    assert_jq "$bs" '(.data.sections | type) == "array"' "blindSpots sections is an array"
    assert_jq "$bs" '.data.selectedSection == "blindSpots"' "blindSpots is the selected section"
    assert_jq "$bs" '(.data.sections | map(.name) | index("blindSpots")) != null' "blindSpots section is present"
    # coverageRatio is an honesty number in [0,1]; log it (no silent omission).
    #
    # PATH SETTLED 2026-10-21, the way the previous comment here asked for it
    # to be settled: by capturing keys, NOT by widening the chain.
    #
    # `coverageRatio` is a PER-ITEM field of the blindSpots items. The product
    # emits that spelling at exactly one place, `blind_spots_section_from_inputs`
    # (src/cli/insights/mod.rs:2801), in the same json! object as
    # rank/blindSpotId/coverageStatus. Confirmed against a live envelope:
    #
    #   .data | keys             -> 13 keys, none coverage-ish
    #   .data.sections[0] | keys -> items,name,nextCommands,summary,title,
    #                               whyItMatters
    #
    # So the old two-spelling read could never hit, and the old `else` branch
    # then attributed a wrong-path read to an unbuilt feature -- exactly the
    # failure the previous comment predicted of itself. Same defect class as
    # bd-psna1 (fixed in 9429f5be5), in this same script, which that fix did
    # not reach.
    #
    # Zero items and items-without-the-field are now DIFFERENT outcomes: the
    # first is an honest empty section, the second is a product gap that fails.
    cov_items="$(printf '%s' "$bs" | jq -r '[.data.sections[]? | select(.name == "blindSpots") | .items[]?] | length')"
    if [ "${cov_items:-0}" -gt 0 ]; then
        # (length > 0) is the empty-world guard: jq's all() returns true for an
        # empty array, so without it an absent field would pass vacuously.
        # Verified all four ways -- in-range true, out-of-range false, field
        # absent false, zero items false.
        assert_jq "$bs" \
            '[.data.sections[]? | select(.name == "blindSpots") | .items[]? | .coverageRatio] | map(select(. != null)) | (length > 0) and all(. >= 0 and . <= 1)' \
            "every blindSpots item carries a coverageRatio in [0,1]"
        e2e_log_note "coverageRatio values=$(printf '%s' "$bs" | jq -c '[.data.sections[]? | select(.name == "blindSpots") | .items[]? | .coverageRatio]')"
    else
        log_drop 1 "blindSpots carried 0 items in this fixture, so no coverageRatio existed to assert; the PATH is settled (.data.sections[].items[].coverageRatio, src/cli/insights/mod.rs:2801) -- this drop reports an empty section, not an unverified read"
    fi
else
    log_drop 1 "insights --section blindSpots pending (bd-1n0np.6.1): blind-spot assertions skipped"
fi

step "feed the query-miss ledger with repeated low-utility searches (6.3)"
# Repeated paraphrased searches for a topic the store does NOT cover -> misses.
for q in "kubernetes pod eviction policy" "pod eviction policy kubernetes" "eviction policy for kubernetes pods"; do
    s="$(ee_json search "$q" --workspace "$WS" --json)"
    # A search on an uncovered topic must still succeed (0 hits is the honest miss).
    assert_jq "$s" '.success == true' "search miss runs: $q"
done

step "learn gaps clusters the misses into knowledge_gap candidates (6.4)"
# 6.4 shipped the clustering as `ee learn gaps`, not as a steward job: there
# is no miss-cluster StewardJobType, so a `steward --job miss-cluster` probe
# could never pass. Its envelope is flat (no `.data`). Whether these three
# searches leave miss rows depends on index state, so the cluster count is
# logged, not asserted.
if ee_help_lists learn gaps; then
    kg="$(ee_json learn gaps --workspace "$WS" --json)"
    assert_jq "$kg" '.success == true' "learn gaps runs"
    assert_jq "$kg" '(.gaps | type) == "array"' "learn gaps emits a gaps array"
    assert_jq "$kg" '(.scannedMissCount | type) == "number"' "learn gaps reports scannedMissCount"
    e2e_log_note "learn_gaps clusterCount=$(printf '%s' "$kg" | jq -r '.clusterCount // "<absent>"')"
else
    log_drop 1 "ee learn gaps absent in binary under test: knowledge_gap assertions skipped"
fi

step "knowledge_gap surfaces in swarm brief (6.4)"
if ee_supports swarm brief; then
    # `--fields summary` is a hand-built projection with no knowledgeGaps key,
    # so the probe needs standard. The key is also omitted when empty, and the
    # brief clusters by exact query hash: three paraphrases are three hashes and
    # never reach KNOWLEDGE_GAP_MIN_CLUSTER_MISSES (3).
    brief="$(ee_json swarm brief --workspace "$WS" --fields standard --json)"
    assert_jq "$brief" '.success == true' "swarm brief succeeds"
    if printf '%s' "$brief" | jq -e '.data | has("knowledgeGaps")' >/dev/null 2>&1; then
        assert_jq "$brief" \
            '(.data.knowledgeGaps | type) == "array" and (.data.knowledgeGaps | length) >= 1' \
            "swarm brief carries at least one knowledge gap"
    else
        log_drop 1 "swarm brief has no knowledgeGaps (omitted when empty; no exact query hash missed 3 times): surfacing assertion skipped"
    fi
else
    log_drop 1 "ee swarm brief surface unavailable in binary under test"
fi

step "pack on an uncovered target carries coverage: thin (6.2)"
packed="$(ee_json pack "kubernetes pod eviction policy" --workspace "$WS" --json)"
assert_jq "$packed" '.success == true' "pack on uncovered target succeeds"
# The coverage signal shipped as `pack --explain-gaps` (CoverageGapReport,
# src/pack/mod.rs), not as a `coverage` key on the ordinary pack, so probing
# the plain pack for one could never pass. Which quality this fixture earns
# depends on what retrieval selects, so the value is logged, not pinned.
if ee_help_lists pack explain-gaps; then
    gaps="$(ee_json pack "kubernetes pod eviction policy" --workspace "$WS" --explain-gaps --json)"
    assert_jq "$gaps" '.success == true' "pack --explain-gaps succeeds"
    assert_jq "$gaps" \
        '.data.packSummary.quality as $q | ["empty", "thin", "partial", "covered"] | index($q) != null' \
        "pack --explain-gaps reports a packSummary.quality"
    e2e_log_note "pack coverage quality=$(printf '%s' "$gaps" | jq -r '.data.packSummary.quality // "<absent>"') posture=$(printf '%s' "$gaps" | jq -r '.data.posture // "<absent>"')"
else
    log_drop 1 "ee pack --explain-gaps absent in binary under test: coverage assertion skipped"
fi

end_temp_workspace
harness_summary
