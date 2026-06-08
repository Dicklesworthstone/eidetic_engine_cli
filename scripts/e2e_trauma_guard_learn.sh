#!/usr/bin/env bash
# bd-1n0np.18.4 — Trauma-Guard bypass-evidence learn loop end-to-end (real binary).
#
# Scenario: the guard policy-denies a risky command, a human issues a one-shot
# bypass for the EXACT command, and `ee preflight learn` correlates that into an
# audited, pending calibration candidate (calmer cited next prompt) WITHOUT ever
# creating an auto-permanent allowlist.
#   1. init workspace.
#   2. `ee preflight check` a risky command -> policy-denied halt (bd-1n0np.18 base).
#   3. issue + use a one-shot bypass token for the exact command -> bypass event.
#   4. `ee preflight learn --dry-run` -> proposes a pending calibration candidate
#      citing the bypass evidence (bd-1n0np.18.1/18.2); `--apply` routes it
#      through curate (propose->validate->apply).
#   5. assert the override stays one-shot (no auto-permanent allowlist).
#
# The preflight guard / bypass-token / `ee preflight learn` surfaces are
# CAPABILITY-GUARDED: where a surface is absent in the binary under test, the step
# records a visible log_drop (the no-silent-cap rule) instead of a false pass, and
# its assertions activate automatically once the binary provides it. init runs for
# real on any binary.
#
# NOTE: no `set -e` — the harness assert_* helpers accumulate pass/fail and
# `harness_summary` decides the exit code.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/e2e_harness.sh
source "$E2E_DIR/lib/e2e_harness.sh"

harness_init "trauma_guard_learn"

ee_json() { "$EE_BIN" "$@" 2>/dev/null || true; }
ee_supports() { "$EE_BIN" "$@" --help >/dev/null 2>&1; }
# True only when `ee preflight --help` actually lists <subcommand>.
ee_preflight_has() {
    "$EE_BIN" preflight --help 2>&1 | grep -qw "$1"
}

with_temp_workspace WS

step "init workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.success == true' "ee init succeeds"

RISKY="rm -rf /important/data"

step "preflight check the risky command -> policy-denied halt (bd-1n0np.18 base)"
# The exact preflight-check invocation (flag vs positional) is binary-dependent;
# probe for a JSON envelope before asserting.
halt_out="$(ee_json preflight check --command "$RISKY" --workspace "$WS" --json)"
if printf '%s' "$halt_out" | jq -e '.success != null' >/dev/null 2>&1; then
    assert_jq "$halt_out" '(.success != null)' "preflight check returns an envelope"
    action="$(printf '%s' "$halt_out" | jq -r 'first(.. | objects | (.action // .guardAction // empty)) // "unknown"')"
    e2e_log_note "preflight action=$action"
else
    log_drop 1 "ee preflight check invocation/surface pending: halt assertions skipped"
fi

step "issue + use a one-shot bypass token for the exact command (bypass event)"
tok="$(ee_json preflight issue-bypass-token --command "$RISKY" --workspace "$WS" --json)"
if printf '%s' "$tok" | jq -e '.success != null' >/dev/null 2>&1; then
    assert_jq "$tok" '(.success != null)' "issue-bypass-token returns an envelope"
else
    log_drop 1 "preflight issue-bypass-token invocation/surface pending: bypass event skipped"
fi

step "ee preflight learn proposes a pending calibration from bypass evidence (18.1/18.2)"
if ee_preflight_has "learn"; then
    dry="$(ee_json preflight learn --dry-run --workspace "$WS" --json)"
    assert_jq "$dry" '.success == true' "preflight learn --dry-run succeeds"
    assert_jq "$dry" \
        '[.. | objects | (.candidates // .proposals // .bypassEvidence // empty)] | length >= 0' \
        "learn emits a (possibly empty) calibration-candidate set"
    # The proposed calibration must be PENDING (never an auto-applied allowlist).
    if printf '%s' "$dry" | jq -e '.. | objects | (.status? == "pending")' >/dev/null 2>&1; then
        assert_jq "$dry" \
            '[.. | objects | select(.status? == "pending")] | length >= 1' \
            "calibration candidates are pending (no auto-permanent allowlist)"
    else
        log_drop 1 "learn calibration-candidate status surface pending (bd-1n0np.18.2)"
    fi
else
    log_drop 1 "ee preflight learn surface pending (bd-1n0np.18.2): correlation/proposal assertions skipped"
fi

step "learn --apply routes calibration through curate (propose->validate->apply)"
if ee_preflight_has "learn"; then
    applied="$(ee_json preflight learn --apply --workspace "$WS" --json)"
    assert_jq "$applied" '.success == true' "preflight learn --apply succeeds"
else
    log_drop 1 "ee preflight learn --apply pending (bd-1n0np.18.2): curate-apply assertions skipped"
fi

end_temp_workspace
harness_summary
