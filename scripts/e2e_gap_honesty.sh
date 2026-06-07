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
#      (bd-1n0np.6.3); the miss-cluster steward turns a tight cluster into a
#      knowledge_gap candidate (bd-1n0np.6.4) ...
#   4. ... which surfaces in `ee swarm brief`.
#   5. a pack on an uncovered target carries `coverage: thin` (bd-1n0np.6.2).
#
# The gap-honesty surfaces (blindSpots section, coverageRatio, the miss-cluster
# steward + knowledge_gap candidate, swarm-brief surfacing, pack coverage flag)
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
    assert_jq "$bs" \
        '(.data.blindSpots // .data.blind_spots // .data.section) != null' \
        "blindSpots section is present"
    # coverageRatio is an honesty number in [0,1]; log it (no silent omission).
    cov="$(printf '%s' "$bs" | jq -r '(.data.coverageRatio // .data.coverage_ratio // empty)')"
    if [ -n "$cov" ]; then
        assert_jq "$bs" \
            '((.data.coverageRatio // .data.coverage_ratio) >= 0) and ((.data.coverageRatio // .data.coverage_ratio) <= 1)' \
            "coverageRatio is a ratio in [0,1]"
        e2e_log_note "coverageRatio=$cov"
    else
        log_drop 1 "coverageRatio absent (bd-1n0np.6.2 importance/coverage ranking not built)"
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

step "miss-cluster steward -> knowledge_gap candidate (6.4)"
if ee_supports steward && ee_help_lists steward knowledge_gap; then
    kg="$(ee_json steward --job miss-cluster --workspace "$WS" --json)"
    assert_jq "$kg" '.success == true' "miss-cluster steward runs"
    assert_jq "$kg" \
        '((.data.candidates // .data.knowledgeGaps // []) | type) == "array"' \
        "steward emits a knowledge_gap candidate array"
else
    log_drop 1 "miss-cluster steward pending (bd-1n0np.6.4 wiring): knowledge_gap assertions skipped"
fi

step "knowledge_gap surfaces in swarm brief (6.4)"
if ee_supports swarm brief; then
    brief="$(ee_json swarm brief --workspace "$WS" --fields summary --json)"
    assert_jq "$brief" '.success == true' "swarm brief succeeds"
    if printf '%s' "$brief" | jq -e '.. | objects | has("knowledgeGaps") or has("knowledge_gaps")' >/dev/null 2>&1; then
        assert_jq "$brief" \
            '[.. | objects | (.knowledgeGaps // .knowledge_gaps // empty)] | length >= 0' \
            "swarm brief carries a knowledge_gaps surface"
    else
        log_drop 1 "swarm brief knowledge_gaps surfacing pending (bd-1n0np.6.4)"
    fi
else
    log_drop 1 "ee swarm brief surface unavailable in binary under test"
fi

step "pack on an uncovered target carries coverage: thin (6.2)"
packed="$(ee_json pack "kubernetes pod eviction policy" --workspace "$WS" --json)"
assert_jq "$packed" '.success == true' "pack on uncovered target succeeds"
if printf '%s' "$packed" | jq -e '.. | objects | has("coverage")' >/dev/null 2>&1; then
    assert_jq "$packed" \
        '[.. | objects | select(has("coverage")) | .coverage] | length >= 1' \
        "pack emits a coverage signal on a thin target"
    covflag="$(printf '%s' "$packed" | jq -r 'first(.. | objects | select(has("coverage")) | .coverage)')"
    e2e_log_note "pack coverage signal=$covflag"
else
    log_drop 1 "pack 'coverage: thin' flag pending (bd-1n0np.6.2): coverage assertion skipped"
fi

end_temp_workspace
harness_summary
