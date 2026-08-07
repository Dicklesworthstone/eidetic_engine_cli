#!/usr/bin/env bash
# bd-1n0np.18.4 — Advisory command-risk evidence loop end-to-end (real binary).
#
# Scenario: advisory preflight retrieves remembered risk context for a command
# without ever changing shell execution authority.
#   1. init workspace.
#   2. remember a provenance-bearing risk.
#   3. `ee preflight check` a risky command -> advisory match + memory, exit 0.
#   4. assert retired bypass/override control-plane surfaces are absent.
#
# The script never executes the inspected command. It verifies only memory,
# provenance, and stable machine-output behavior.
#
# NOTE: no `set -e` — the harness assert_* helpers accumulate pass/fail and
# `harness_summary` decides the exit code.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/e2e_harness.sh
source "$E2E_DIR/lib/e2e_harness.sh"

harness_init "trauma_guard_learn"

ee_json() { "$EE_BIN" "$@" 2>/dev/null || true; }
with_temp_workspace WS

step "init workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.success == true' "ee init succeeds"

RISKY="rm -rf /important/data"

step "remember provenance-bearing command risk"
remembered="$(ee_json remember \
    "Prior failure: rm -rf recursively removed important data." \
    --kind risk --level procedural --source "cass-session://incident-rm-rf#L1-L3" \
    --workspace "$WS" --json)"
assert_jq "$remembered" '.success == true' "risk memory is stored"

step "preflight check retrieves advisory risk context without command denial"
advisory_out="$(ee_json preflight check --cmd "$RISKY" --workspace "$WS" --json)"
assert_jq "$advisory_out" '.schema == "ee.preflight.guard.v1"' \
    "preflight check returns the advisory risk-memory schema"
assert_jq "$advisory_out" '.exitCode == 0' \
    "high-risk matches never become a policy-denied process status"
assert_jq "$advisory_out" \
    '([.matches[]? | select(.action == "high_risk")] | length) >= 1' \
    "destructive pattern remains explainable as advisory high-risk context"
assert_jq "$advisory_out" \
    '([.matches[]? | select(.resolution == "matched")] | length) >= 1' \
    "matched rules use non-enforcement vocabulary"
assert_jq "$advisory_out" \
    '([.matchedMemories[]? | select(.kind == "risk")] | length) >= 1' \
    "advisory check cites remembered risk with provenance"

step "assert bypass and override control-plane surfaces are absent"
assert_exit 2 "issue-bypass-token is not a command" -- \
    "$EE_BIN" preflight issue-bypass-token --workspace "$WS"
assert_exit 2 "revoke-bypass-token is not a command" -- \
    "$EE_BIN" preflight revoke-bypass-token --workspace "$WS"
assert_exit 2 "list-bypass-tokens is not a command" -- \
    "$EE_BIN" preflight list-bypass-tokens --workspace "$WS"
assert_exit 2 "override-token is not accepted" -- \
    "$EE_BIN" preflight check --cmd "$RISKY" --override-token retired --workspace "$WS"
assert_exit 2 "bypass is not accepted" -- \
    "$EE_BIN" preflight check --cmd "$RISKY" --bypass rule:retired --workspace "$WS"

end_temp_workspace
harness_summary
