#!/usr/bin/env bash
# bd-1n0np.20.4 — Graph-Protected Causal Bridge Exemption end-to-end (real binary).
#
# Scenario: a failure -> bridge-memory -> solution structure where the bridge
# memory is the sole articulation point connecting the two halves. Decay
# maintenance must PROTECT the bridge memory (it is load-bearing) while letting a
# disconnected stale non-bridge memory decay; and the exemption must be CAPPED
# with honest degradation on a degenerate graph.
#   1. init + remember failure / bridge / solution memories + an isolated stale one.
#   2. link failure -> bridge -> solution so `bridge` is the sole cut vertex.
#   3. `ee maintenance run --job decay_sweep --include-decay --dry-run`: assert
#      the bridge memory's structuralAdjustment marks it an articulation point
#      (bd-1n0np.20.1/20.2).
#   4. assert the cap (no multiplier of 0 under the default policy) and that a
#      degenerate graph earns no protection (no silent over-protection).
#
# The decay-maintenance + bridge-exemption surfaces are CAPABILITY-GUARDED: where
# a surface is absent in the binary under test, the step records a visible
# log_drop (the no-silent-cap rule) instead of a false pass, and its assertions
# activate automatically once the binary provides it. init / remember / link run
# for real on any binary that exposes them.
#
# NOTE: no `set -e` — the harness assert_* helpers accumulate pass/fail and
# `harness_summary` decides the exit code.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/e2e_harness.sh
source "$E2E_DIR/lib/e2e_harness.sh"

harness_init "bridge_exemption"

ee_json() { "$EE_BIN" "$@" 2>/dev/null || true; }
ee_supports() { "$EE_BIN" "$@" --help >/dev/null 2>&1; }
# True only when `ee <cmd> --help` actually lists <token>.
ee_help_lists() {
    local cmd="$1" token="$2"
    "$EE_BIN" "$cmd" --help 2>&1 | grep -qw "$token"
}

with_temp_workspace WS

step "init workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.success == true' "ee init succeeds"

step "remember failure / bridge / solution + isolated stale memories"
fail_out="$(ee_json remember "Build fails with linker error LNK1120 on Windows." \
    --workspace "$WS" --level episodic --kind failure --tags build,win --json)"
assert_jq "$fail_out" '.success == true' "remember failure memory"
bridge_out="$(ee_json remember "The linker error is the missing /WHOLEARCHIVE flag." \
    --workspace "$WS" --level semantic --kind fact --tags build,bridge --json)"
assert_jq "$bridge_out" '.success == true' "remember bridge memory"
sol_out="$(ee_json remember "Add /WHOLEARCHIVE:lib.lib to the linker flags to fix LNK1120." \
    --workspace "$WS" --level procedural --kind rule --tags build,fix --json)"
assert_jq "$sol_out" '.success == true' "remember solution memory"
stale_out="$(ee_json remember "An unrelated note about an old config toggle." \
    --workspace "$WS" --level episodic --kind note --tags misc --json)"
assert_jq "$stale_out" '.success == true' "remember isolated stale memory"

fail_id="$(printf '%s' "$fail_out" | jq -r '(.data.id // .data.memory.id // .data.memoryId // empty)')"
bridge_id="$(printf '%s' "$bridge_out" | jq -r '(.data.id // .data.memory.id // .data.memoryId // empty)')"
sol_id="$(printf '%s' "$sol_out" | jq -r '(.data.id // .data.memory.id // .data.memoryId // empty)')"
e2e_log_note "fail=$fail_id bridge=$bridge_id solution=$sol_id"

step "link failure -> bridge -> solution (bridge = sole articulation point)"
if ee_supports link && [ -n "$fail_id" ] && [ -n "$bridge_id" ] && [ -n "$sol_id" ]; then
    l1="$(ee_json link "$fail_id" "$bridge_id" --relation supports --workspace "$WS" --json)"
    l2="$(ee_json link "$bridge_id" "$sol_id" --relation supports --workspace "$WS" --json)"
    assert_jq "$l1" '.success == true' "link failure -> bridge"
    assert_jq "$l2" '.success == true' "link bridge -> solution"
else
    log_drop 1 "ee link surface or memory ids unavailable: bridge structure skipped"
fi

step "decay maintenance protects the bridge memory (bd-1n0np.20.1/20.2)"
# Protection shipped as a per-memory structuralAdjustment on the decay sweep
# (src/steward/mod.rs), computed only with --include-decay and built from the
# memory links. This probe used to call bare `ee maintenance`, which is a
# command group, so it never got a success envelope. --dry-run keeps scores.
decayed="$(ee_json maintenance run --job decay_sweep --include-decay --dry-run --workspace "$WS" --json)"
if printf '%s' "$decayed" | jq -e '.success == true' >/dev/null 2>&1; then
    assert_jq "$decayed" '.success == true' "decay sweep dry run succeeds"
    bridge_adj='[.. | objects | select(has("structuralAdjustment")) | .structuralAdjustment | select(.memoryId == $b)]'
    if [ -n "$bridge_id" ] && printf '%s' "$decayed" | jq -e --arg b "$bridge_id" "$bridge_adj | length >= 1" >/dev/null 2>&1; then
        # isArticulationPoint is set for a cut vertex regardless of onion depth.
        if printf '%s' "$decayed" | jq -e --arg b "$bridge_id" "$bridge_adj | all(.isArticulationPoint == true)" >/dev/null 2>&1; then
            _harness_pass "decay sweep marks the bridge memory as an articulation point"
        else
            _harness_fail "decay sweep did not mark the bridge memory as an articulation point"
        fi
    else
        log_drop 1 "decay sweep listed no structuralAdjustment for the bridge memory: protection assertion skipped"
    fi
else
    log_drop 1 "ee maintenance run --job decay_sweep did not succeed in this binary: exemption assertions skipped"
fi

step "exemption cap + honest degradation on a degenerate graph (no over-protection)"
# There is no exemptionCap field. The cap is the multiplier itself: the
# default articulation_protection is 0.5, clamped to [0, 1], so under the
# default policy no memory is fully exempted (multiplier 0). A degenerate graph
# (maxLayer < 2) earns no protection at all and reports the baseline. The old
# form passed on any envelope, since every one has `degraded` and `length >= 0`
# is always true.
if printf '%s' "$decayed" | jq -e '[.. | objects | select(has("structuralAdjustment"))] | length >= 1' >/dev/null 2>&1; then
    assert_jq "$decayed" \
        '[.. | objects | select(has("structuralAdjustment")) | .structuralAdjustment] | all(.structuralMultiplier > 0)' \
        "no memory is fully exempted from decay under the default policy"
    assert_jq "$decayed" \
        '[.. | objects | select(has("structuralAdjustment")) | .structuralAdjustment | select(.maxLayer < 2)]
         | all(.structuralMultiplier == 1 and .rationale == "structural_decay_baseline")' \
        "a degenerate graph earns no structural protection"
else
    log_drop 1 "decay sweep reported no structuralAdjustment: cap and degenerate-graph assertions skipped"
fi

end_temp_workspace
harness_summary
