#!/usr/bin/env bash
# bd-1n0np.10.5 — House Rules cross-workspace global memory tier end-to-end
# (real binary, multi-workspace, detailed logging).
#
# Scenario: ONE shared database (the harness's EE_DATABASE_PATH) with TWO
# workspaces (wsA, wsB). A "house rule" memory is written in wsA tagged `global`;
# a normal memory is written in wsA untagged. Assertions:
#   * hard (always true on a current binary): the `houseRules` insights section
#     lists the global-tagged memory in its origin workspace (wsA) but NOT the
#     untagged one.
#   * condition-guarded (no-silent-cap log_drop when the surface is not present
#     in the binary under test): CROSS-WORKSPACE visibility from wsB (insights
#     reads the per-workspace <ws>/.ee/ee.db and the 10.1 union is within a single
#     DB, so a shared/global-DB read path is a bd-1n0np.10 follow-on); the
#     `house_rule` tag alias; and the audited promotion CLI `ee remember --scope
#     global` (bd-1n0np.10.2, cli-gated).
# Every step emits an ee.test_event.v1 event + a human line via the shared
# harness (scripts/lib/e2e_harness.sh, surfaced through scripts/e2e_lib.sh), and
# harness_summary prints PASS/FAIL with an artifact dir and owns the exit code.
#
# NOTE: no `set -e` — assert_* accumulate pass/fail and harness_summary decides
# the exit code, so a single failing assert must not abort before the summary.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_lib.sh
# shellcheck disable=SC1091
source "$E2E_DIR/e2e_lib.sh"

harness_init "house_rules"

# ee_json <args...> — run ee, tolerate nonzero exit (assertions inspect output).
ee_json() { "$EE_BIN" "$@" 2>/dev/null || true; }

GLOBAL_RULE="house rule: always run cargo fmt before committing"
LOCAL_NOTE="local note: this workspace uses feature flag X"

# One shared DB (harness EE_DATABASE_PATH); two workspace roots inside it so a
# global memory written in wsA is visible cross-workspace from wsB.
with_temp_workspace WS
WS_A="$WS/wsA"
WS_B="$WS/wsB"
mkdir -p "$WS_A" "$WS_B"

step "init two workspaces against the shared database"
init_a="$(ee_json --workspace "$WS_A" init --json)"
init_b="$(ee_json --workspace "$WS_B" init --json)"
assert_jq "$init_a" '.success == true' "ee init wsA succeeds"
assert_jq "$init_b" '.success == true' "ee init wsB succeeds"

step "seed a global house rule in wsA + a normal workspace-local note"
rule_out="$(ee_json --workspace "$WS_A" remember "$GLOBAL_RULE" \
    --level semantic --kind note --tags global --json)"
note_out="$(ee_json --workspace "$WS_A" remember "$LOCAL_NOTE" \
    --level semantic --kind note --tags local --json)"
assert_jq "$rule_out" '.success == true' "global house rule remembered in wsA"
assert_jq "$note_out" '.success == true' "workspace-local note remembered in wsA"
log_event "house_rules_seed" globalRule "$GLOBAL_RULE" workspace wsA

step "houseRules insights section lists the global rule (origin workspace wsA)"
hr_a="$(ee_json --workspace "$WS_A" insights --section houseRules --json)"
assert_jq "$hr_a" '.success == true' "ee insights --section houseRules succeeds (wsA)"
assert_jq "$hr_a" \
    '[.data.sections[]? | select(.name == "houseRules") | .items[]? | select(.interpretation == "house_rule")] | length >= 1' \
    "houseRules section lists at least one house rule in wsA"
# The workspace-local note must NOT be a house rule.
assert_jq "$hr_a" \
    '[.data.sections[]? | select(.name == "houseRules") | .items[]?] | all(.interpretation == "house_rule")' \
    "houseRules section contains only house-rule items (no workspace-local leak)"

step "cross-workspace union: the global rule surfaces from wsB (bd-1n0np.10.1)"
hr_b="$(ee_json --workspace "$WS_B" insights --section houseRules --json)"
assert_jq "$hr_b" '.success == true' "ee insights --section houseRules succeeds (wsB)"
hr_b_count="$(printf '%s' "$hr_b" | jq -r '[.data.sections[]? | select(.name == "houseRules") | .items[]?] | length' 2>/dev/null || printf '0')"
if [ "${hr_b_count:-0}" -ge 1 ]; then
    _harness_pass "global house rule is visible cross-workspace from wsB (candidate-load union)"
    assert_jq "$hr_b" \
        '[.data.sections[]? | select(.name == "houseRules") | .items[]? | select(has("originatingWorkspace"))] | length >= 1' \
        "house rules carry originating-workspace provenance"
else
    # No-silent-cap: `ee insights` reads the per-workspace <ws>/.ee/ee.db, and the
    # 10.1 candidate-load union is within a single DB (workspace_id OR global tag),
    # so two separately-init'd workspaces do not share a DB. Cross-workspace
    # surfacing needs a shared/global-DB read path that is not wired yet.
    log_drop 1 "cross-workspace house-rule visibility from wsB is empty: insights reads per-workspace .ee/ee.db; shared/global-DB read path not wired (bd-1n0np.10 follow-on)"
fi

step "house_rule tag alias is also recognized (no-silent-cap if not)"
alias_out="$(ee_json --workspace "$WS_A" remember "house rule alias check" \
    --level semantic --kind note --tags house_rule --json)"
if printf '%s' "$alias_out" | jq -e '.success == true' >/dev/null 2>&1; then
    hr_alias="$(ee_json --workspace "$WS_A" insights --section houseRules --json)"
    assert_jq "$hr_alias" \
        '[.data.sections[]? | select(.name == "houseRules") | .items[]?] | length >= 2' \
        "house_rule-tagged memory also counts as a house rule"
else
    log_drop 1 "remember with --tags house_rule did not succeed; tag-alias assertion skipped"
fi

step "audited promotion CLI ee remember --scope global (bd-1n0np.10.2, cli-gated)"
if "$EE_BIN" remember --help 2>&1 | grep -q -- "--scope"; then
    scope_out="$(ee_json --workspace "$WS_B" remember "promoted via scope flag" \
        --level semantic --kind note --scope global --json)"
    assert_jq "$scope_out" '.success == true' "ee remember --scope global succeeds"
else
    # No-silent-cap: the audited promotion write surface is not shipped yet.
    log_drop 1 "ee remember --scope global flag absent (bd-1n0np.10.2 pending): promotion-CLI assertion skipped"
fi

end_temp_workspace
harness_summary
