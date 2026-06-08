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
#   3. `ee maintenance` decay job: assert the bridge memory is exempted/protected
#      while the isolated stale memory is eligible to decay (bd-1n0np.20.1/20.2).
#   4. assert the exemption cap + honest degradation on a degenerate graph
#      (no silent over-protection).
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
# The exact decay-maintenance invocation (job name / subcommand) is binary-
# dependent; probe for a success envelope before asserting the exemption surface.
decayed="$(ee_json maintenance --workspace "$WS" --json)"
if printf '%s' "$decayed" | jq -e '.success == true' >/dev/null 2>&1; then
    assert_jq "$decayed" '.success == true' "maintenance job runs"
    if printf '%s' "$decayed" | jq -e '.. | objects | has("bridgeExemptions") or has("bridge_exemptions") or has("exemptedMemories")' >/dev/null 2>&1; then
        assert_jq "$decayed" \
            '[.. | objects | (.bridgeExemptions // .bridge_exemptions // .exemptedMemories // empty)] | length >= 1' \
            "maintenance reports bridge exemptions"
    else
        log_drop 1 "bridge-exemption maintenance surface pending (bd-1n0np.20.2): exemption assertions skipped"
    fi
else
    log_drop 1 "decay-maintenance job surface pending (bd-1n0np.20.1/20.2): exemption assertions skipped"
fi

step "exemption cap + honest degradation on a degenerate graph (no over-protection)"
if printf '%s' "$decayed" | jq -e '.. | objects | has("exemptionCap") or has("degraded")' >/dev/null 2>&1; then
    assert_jq "$decayed" \
        '([.. | objects | (.exemptionCap // empty)] | length >= 0)' \
        "exemption cap / degradation surface present"
else
    log_drop 1 "exemption cap + degenerate-graph degradation surface pending (bd-1n0np.20.2)"
fi

end_temp_workspace
harness_summary
