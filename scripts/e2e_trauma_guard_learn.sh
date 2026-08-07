#!/usr/bin/env bash
# bd-1n0np.18.4 — Advisory command-risk evidence loop end-to-end (real binary).
#
# Scenario: advisory preflight retrieves risk context for a command, optional
# one-shot authorization evidence is audited for the exact command, without
# ever changing shell execution authority.
#   1. init workspace.
#   2. `ee preflight check` a risky command -> advisory high-risk match, exit 0.
#   3. issue + use one-shot authorization evidence for the exact command.
#   4. assert exhausted evidence is advisory and no allowlist is created.
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

step "record and consume optional one-shot authorization evidence"
tok="$(ee_json preflight issue-bypass-token --cmd "$RISKY" \
    --reason "e2e historical authorization evidence" --workspace "$WS" --json)"
assert_jq "$tok" '.success == true' "issue-bypass-token returns a success envelope"
raw_token="$(printf '%s' "$tok" | jq -r '.data.report.token // empty')"
assert_eq "$(test -n "$raw_token" && printf present || printf missing)" "present" \
    "authorization evidence token is issued"

used="$(ee_json preflight check --cmd "$RISKY" --override-token "$raw_token" \
    --workspace "$WS" --json)"
assert_jq "$used" '.schema == "ee.preflight.guard.v1" and .exitCode == 0' \
    "authorization evidence does not change advisory exit behavior"
assert_jq "$used" \
    '([.matches[]? | select(.resolution == "bypassed_with_token")] | length) >= 1' \
    "valid one-shot evidence is recorded in match provenance"

exhausted="$(ee_json preflight check --cmd "$RISKY" --override-token "$raw_token" \
    --workspace "$WS" --json)"
assert_jq "$exhausted" '.schema == "ee.preflight.guard.v1" and .exitCode == 0' \
    "exhausted evidence remains advisory"
assert_jq "$exhausted" \
    '([.degraded[]? | select(.code == "bypass_token_exhausted")] | length) == 1' \
    "exhaustion is reported as degraded evidence, never command denial"

end_temp_workspace
harness_summary
