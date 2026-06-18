#!/usr/bin/env bash
# bd-1n0np.3 — Code-anchoring substrate end-to-end (real binary).
#
# Scenario (ADR 0056): temp workspace -> remember a memory whose backtick code
# spans yield precision anchors (path, command, schema, env_var) -> assert
# `ee impact <surface>` returns the memory as an exact-anchor hit for each
# surface kind, that the response carries the stable ee.response.v2 envelope and
# documented phase structure, that the graph-neighbors tier degrades honestly
# (deferred to bd-1n0np.3.4) instead of fabricating neighbors, and that impact
# output is deterministic. Then exercise the read-only `ee memory drift`
# freshness surface.
#
# Surfaces NOT yet wired into the binary under test are CAPABILITY-GUARDED: a
# missing surface records a visible `log_drop` (the no-silent-cap rule) instead
# of a false pass, and the assertion activates automatically once the surface
# exists. Specifically the live anchor-match BOOST re-rank in core/search.rs and
# the per-pack freshness `symbol_drift` facet are landed only as primitives
# (query_anchor_match_context / anchor_match_score / freshness_drift_multiplier /
# MemoryAnchorFreshnessTransition) and are not exposed as observable CLI
# behavior yet (the live integration is bd-1n0np.3.3/3.7 + the SearchScoringSignals
# prerequisite), so this script documents them as drops rather than asserting.
#
# NOTE: no `set -e` — the harness assert_* helpers accumulate pass/fail and
# `harness_summary` decides the exit code, so a single failing assert must not
# abort the run before the summary is written.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/e2e_harness.sh
source "$E2E_DIR/lib/e2e_harness.sh"

harness_init "anchors"

# ee_supports <subcommand words...> — true when `<words> --help` is accepted.
ee_supports() { "$EE_BIN" "$@" --help >/dev/null 2>&1; }

# ee_json <args...> — run ee, tolerate nonzero exit (assertions inspect output).
ee_json() { "$EE_BIN" "$@" 2>/dev/null || true; }

with_temp_workspace WS

step "init workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.success == true' "ee init succeeds"

step "remember a rule carrying precision code-span anchors"
remembered="$(ee_json remember \
    "Always run \`cargo fmt --check\` before editing \`src/db/mod.rs\`; the envelope is \`ee.response.v2\` and honor \`EE_PACK_TRACE\`." \
    --workspace "$WS" --level procedural --kind rule --json)"
assert_jq "$remembered" '.success == true' "remember succeeds"
mem_id="$(printf '%s' "$remembered" | jq -r '.data.memory_id // empty')"
assert_eq "$([ -n "$mem_id" ] && echo present || echo missing)" "present" \
    "remember returns a memory id"

if ! ee_supports impact; then
    log_drop 1 "ee impact surface absent: binary under test predates impact (bd-1n0np.3.5); impact assertions skipped"
else
    step "ee impact <path> returns the anchored memory as an exact-anchor hit"
    imp_path="$(ee_json impact "src/db/mod.rs" --workspace "$WS" --limit 1 --json)"
    assert_jq "$imp_path" '.schema == "ee.response.v2"' "impact envelope is ee.response.v2"
    assert_jq "$imp_path" '.success == true' "ee impact succeeds"
    assert_jq "$imp_path" '.data.command == "impact"' "impact data.command"
    assert_jq "$imp_path" '.data.surface.kind == "path"' "impact path surface kind"
    assert_jq "$imp_path" '.data.phases.exactAnchor.status == "ok"' \
        "impact exact-anchor phase ok"
    assert_jq "$imp_path" '(.data.phases.exactAnchor.resultCount // 0) >= 1' \
        "impact exact-anchor finds >=1 anchor"
    assert_jq "$imp_path" '(.data.results | length) >= 1' "impact returns results"
    assert_jq "$imp_path" "any(.data.results[]; .memoryId == \"$mem_id\")" \
        "impact results include the remembered memory"
    assert_jq "$imp_path" \
        "any(.data.results[]; .memoryId == \"$mem_id\" and .matchType == \"exact_anchor\")" \
        "remembered memory is an exact_anchor match"
    # Anchor identity stays hashed in the surface block (redaction policy).
    assert_jq "$imp_path" '.data.surface.anchorValueHash | startswith("blake3:")' \
        "impact surface carries a hashed anchor identity"
    # Graph tier degrades honestly (deferred to bd-1n0np.3.4), never fabricates.
    assert_jq "$imp_path" '.data.phases.graphNeighbors.status == "not_available"' \
        "graph-neighbors tier is honest-degraded"

    step "ee impact --command / --schema-id resolve their typed surfaces"
    imp_cmd="$(ee_json impact --command "cargo fmt --check" --workspace "$WS" --json)"
    assert_jq "$imp_cmd" '.data.surface.kind == "command"' "impact command surface kind"
    imp_schema="$(ee_json impact --schema-id "ee.response.v2" --workspace "$WS" --json)"
    assert_jq "$imp_schema" '.data.surface.kind == "schema"' "impact schema surface kind"

    step "ee impact is deterministic across identical queries"
    imp_a="$(ee_json impact "src/db/mod.rs" --workspace "$WS" --limit 1 --json \
        | jq -S 'del(.data.elapsedMs)')"
    imp_b="$(ee_json impact "src/db/mod.rs" --workspace "$WS" --limit 1 --json \
        | jq -S 'del(.data.elapsedMs)')"
    assert_eq "$imp_a" "$imp_b" "impact JSON is deterministic (elapsedMs removed)"
fi

if ! ee_supports memory drift; then
    log_drop 1 "ee memory drift surface absent: binary predates it; drift assertion skipped"
else
    step "ee memory drift is a read-only freshness surface"
    drift_out="$(ee_json memory drift --workspace "$WS" --json)"
    assert_jq "$drift_out" '.success == true' "ee memory drift succeeds (read-only)"
fi

printf '[anchors-e2e] step: stale-anchor recall/scoring guardrails\n' >&2
step "stale-anchor recall/scoring guardrails are pinned"
memory_show="$(ee_json memory show "$mem_id" --workspace "$WS" --json)"
assert_jq "$memory_show" '.schema == "ee.response.v2"' "memory show envelope is ee.response.v2"
assert_jq "$memory_show" '.success == true' "memory show succeeds for remembered anchor memory"
assert_eq "$(grep -q 'fn max_stale_anchor_penalty_keeps_drifted_memory_visible' "$REPO_ROOT/src/search/scoring.rs" && echo present || echo missing)" \
    "present" "scoring unit pins max opt-in stale-anchor visibility"
assert_eq "$(grep -q 'fn invalid_stale_anchor_penalty_fails_closed_to_neutral' "$REPO_ROOT/src/search/scoring.rs" && echo present || echo missing)" \
    "present" "scoring unit pins invalid penalty neutrality"
printf '[anchors-e2e] assert: stale-anchor suspect boundary unit is present\n' >&2
assert_eq "$(grep -q 'fn max_penalty_suspect_anchor_stays_between_current_and_stale' "$REPO_ROOT/src/search/scoring.rs" && echo present || echo missing)" \
    "present" "scoring unit pins max-penalty suspect midpoint"
assert_eq "$(grep -q 'fn default_stale_anchor_survives_tight_budget_tie' "$REPO_ROOT/src/core/recall.rs" && echo present || echo missing)" \
    "present" "recall unit pins tight-budget stale-anchor visibility"
assert_eq "$(grep -q 'fn invalid_stale_anchor_penalty_is_neutral_in_recall' "$REPO_ROOT/src/core/recall.rs" && echo present || echo missing)" \
    "present" "recall unit pins invalid penalty neutrality"
e2e_log_note "stale_anchor_guard source=src/search/scoring.rs,src/core/recall.rs coverage=default_neutral,opt_in_visible,invalid_penalty,tight_budget,suspect_boundary"

# No-silent-cap: record the anchors+freshness behavior that exists only as
# library primitives, not yet as observable CLI behavior, so a green run is not
# mistaken for full coverage.
log_drop 1 "anchor-match boost (live re-rank) not yet observable: query_anchor_match_context + anchor_match_score are landed primitives; the core/search.rs re-rank is bd-1n0np.3.3 + the SearchScoringSignals integration prerequisite"
log_drop 1 "freshness symbol_drift surfacing not yet observable in CLI ranking: MemoryAnchorFreshnessTransition + freshness_drift_multiplier + stale-anchor recall/scoring tests are pinned; the steward drift job and per-pack symbol_drift facet are bd-1n0np.3.7/3.8"

harness_summary
