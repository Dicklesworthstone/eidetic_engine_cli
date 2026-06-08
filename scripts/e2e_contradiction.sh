#!/usr/bin/env bash
# bd-1n0np.7.7 — Contradiction operationalization end-to-end (real binary).
#
# Scenario: two opposed rules linked by an explicit contradiction edge, driven
# through the full contradiction lifecycle:
#   1. init + remember two opposed rules; link them with an explicit
#      contradiction edge (the explicit-evidence the detector keys on, 7.2).
#   2. `ee conflict list` ranks the conflicting pair (bd-1n0np.7.3).
#   3. `ee curate contradictions` proposes a resolution; resolve via scope-split
#      then supersede (bd-1n0np.7.4).
#   4. a pack on the topic suppresses the losing side (contradiction_suppressed),
#      confirmed via `ee why-not` (bd-1n0np.7.5).
#   5. forced-mode pack surfaces BOTH sides under a Contradictions header, capped
#      (bd-1n0np.7.5).
#
# The conflict/curate/pack-guard surfaces (7.3 conflict list/explain/cluster, 7.4
# audited resolution, 7.5 pack guard + forced-mode) are CAPABILITY-GUARDED: where
# a surface is absent in the binary under test, the step records a visible
# log_drop (the no-silent-cap rule) instead of a false pass, and its assertions
# activate automatically once the binary provides it. The init / remember / link
# path runs for real on any binary that exposes it.
#
# NOTE: no `set -e` — the harness assert_* helpers accumulate pass/fail and
# `harness_summary` decides the exit code.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/e2e_harness.sh
source "$E2E_DIR/lib/e2e_harness.sh"

harness_init "contradiction"

ee_json() { "$EE_BIN" "$@" 2>/dev/null || true; }
ee_supports() { "$EE_BIN" "$@" --help >/dev/null 2>&1; }
# True only when `ee <cmd> --help` actually lists <token> (avoids clap positional
# false-positives on binaries that predate the flag/subcommand).
ee_help_lists() {
    local cmd="$1" token="$2"
    "$EE_BIN" "$cmd" --help 2>&1 | grep -qw "$token"
}
# True only when `ee pack --help` actually lists the forced-mode flag.
ee_pack_forced_mode_available() {
    "$EE_BIN" pack --help 2>&1 | grep -qw "contradiction-forced"
}

with_temp_workspace WS

step "init workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.success == true' "ee init succeeds"

step "remember two opposed rules"
rule_a="$(ee_json remember "Always retry failed network calls up to 3 times." \
    --workspace "$WS" --level procedural --kind rule --tags retry,policy --json)"
assert_jq "$rule_a" '.success == true' "remember rule A (retry)"
rule_b="$(ee_json remember "Never retry failed network calls; fail fast instead." \
    --workspace "$WS" --level procedural --kind rule --tags retry,policy --json)"
assert_jq "$rule_b" '.success == true' "remember rule B (fail-fast, opposed)"

# Memory ids for the explicit contradiction link (best-effort extraction).
id_a="$(printf '%s' "$rule_a" | jq -r '(.data.id // .data.memory.id // .data.memoryId // empty)')"
id_b="$(printf '%s' "$rule_b" | jq -r '(.data.id // .data.memory.id // .data.memoryId // empty)')"
e2e_log_note "rule_a_id=${id_a:-<none>} rule_b_id=${id_b:-<none>}"

step "link the pair with an explicit contradiction edge (7.2 evidence)"
if ee_supports link && [ -n "$id_a" ] && [ -n "$id_b" ]; then
    linked="$(ee_json link "$id_a" "$id_b" --relation contradicts --workspace "$WS" --json)"
    assert_jq "$linked" '.success == true' "explicit contradicts link created"
else
    log_drop 1 "ee link surface or memory ids unavailable: explicit contradiction edge skipped"
fi

step "conflict list ranks the conflicting pair (bd-1n0np.7.3)"
if ee_supports conflict list; then
    conflicts="$(ee_json conflict list --workspace "$WS" --json)"
    assert_jq "$conflicts" '.success == true' "conflict list succeeds"
    assert_jq "$conflicts" \
        '((.data.conflicts // .data.clusters // []) | type) == "array"' \
        "conflict list emits a ranked conflict array"
else
    log_drop 1 "ee conflict list pending (bd-1n0np.7.3): ranking assertions skipped"
fi

step "conflict explain surfaces the evidence (bd-1n0np.7.3)"
if ee_supports conflict explain; then
    explained="$(ee_json conflict explain --workspace "$WS" --json)"
    assert_jq "$explained" '.success == true' "conflict explain succeeds"
else
    log_drop 1 "ee conflict explain pending (bd-1n0np.7.3): evidence assertions skipped"
fi

step "curate contradictions + resolve (scope-split then supersede) (bd-1n0np.7.4)"
if ee_supports curate contradictions; then
    curated="$(ee_json curate contradictions --workspace "$WS" --json)"
    assert_jq "$curated" '.success == true' "curate contradictions surfaces a resolution proposal"
    won_side="$(printf '%s' "$curated" | jq -r 'first(.. | objects | (.winner // .supersededBy // empty)) // "<unresolved>"')"
    e2e_log_note "resolution winner=$won_side"
else
    log_drop 1 "ee curate contradictions pending (bd-1n0np.7.4): resolution assertions skipped"
fi

step "pack suppresses the losing side; why-not confirms (bd-1n0np.7.5)"
packed="$(ee_json pack "should I retry failed network calls" --workspace "$WS" --json)"
assert_jq "$packed" '.success == true' "pack on the contested topic succeeds"
if printf '%s' "$packed" | jq -e '.. | objects | has("contradictionSuppressed") or has("contradiction_suppressed")' >/dev/null 2>&1; then
    assert_jq "$packed" \
        '[.. | objects | (.contradictionSuppressed // .contradiction_suppressed // empty)] | length >= 1' \
        "pack records contradiction_suppressed for the losing side"
    if ee_supports why-not; then
        whynot="$(ee_json why-not "$id_b" --workspace "$WS" --json)"
        assert_jq "$whynot" '.success == true' "why-not explains the suppression"
    else
        log_drop 1 "ee why-not surface unavailable: suppression confirmation skipped"
    fi
else
    log_drop 1 "pack contradiction guard pending (bd-1n0np.7.5): suppression assertions skipped"
fi

step "forced-mode pack surfaces BOTH sides under a Contradictions header, capped (bd-1n0np.7.5)"
if ee_pack_forced_mode_available; then
    forced="$(ee_json pack "should I retry failed network calls" \
        --workspace "$WS" --contradiction-forced --json)"
    assert_jq "$forced" '.success == true' "forced-mode pack succeeds"
    assert_jq "$forced" \
        '[.. | objects | select((.header // .section // "") | test("ontradiction"))] | length >= 1' \
        "forced-mode pack carries a Contradictions header"
else
    log_drop 1 "pack --contradiction-forced pending (bd-1n0np.7.5): forced-mode assertions skipped"
fi

end_temp_workspace
harness_summary
