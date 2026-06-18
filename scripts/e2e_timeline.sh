#!/usr/bin/env bash
# bd-2vq2z.16 - Time-travel memory audit UX e2e.
#
# Scenario: run a real ee binary against an isolated workspace, seed memories
# with deterministic validity windows, and assert `ee timeline` reconstructs
# the as-of state plus added/superseded changes without using wall-clock sleeps.
#
# NOTE: no `set -e` - assert_* helpers accumulate failures and harness_summary
# decides the exit code, so a single failed assertion cannot prevent artifacts
# and the summary from being written.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Avoid the shared harness's cargo-metadata fallback in code-first swarm lanes.
EE_BIN="${EE_BIN:-ee}"
export EE_BIN

# shellcheck source=scripts/e2e_lib.sh
# shellcheck disable=SC1091
source "$E2E_DIR/e2e_lib.sh"

harness_init "timeline"

ee_json() {
    e2e_log_command "$EE_BIN" "$@" || true
}

ee_has_timeline_cli() {
    "$EE_BIN" timeline --help >/dev/null 2>&1
}

with_temp_workspace WS

step "init isolated workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.schema == "ee.response.v2" and .success == true' \
    "ee init returns a success response envelope"

step "seed deterministic timeline memories"
old_policy="$(
    ee_json remember "Timeline audit policy: use RCH before release." \
        --workspace "$WS" \
        --level procedural \
        --kind rule \
        --tags timeline,audit \
        --source "test://bd-2vq2z.16/old-policy" \
        --valid-from "2026-05-01T00:00:00Z" \
        --valid-to "2026-05-03T00:00:00Z" \
        --json
)"
new_policy="$(
    ee_json remember "Timeline audit policy: central batch verify owns release proof." \
        --workspace "$WS" \
        --level procedural \
        --kind rule \
        --tags timeline,audit \
        --source "test://bd-2vq2z.16/new-policy" \
        --valid-from "2026-05-03T00:00:00Z" \
        --json
)"
decision="$(
    ee_json remember "Decision: timeline audit uses memory validity windows." \
        --workspace "$WS" \
        --level procedural \
        --kind decision \
        --tags timeline,decision \
        --source "test://bd-2vq2z.16/decision" \
        --valid-from "2026-05-02T00:00:00Z" \
        --json
)"
assert_jq "$old_policy" '.schema == "ee.response.v2" and .success == true' \
    "old policy memory stores with a bounded validity window"
assert_jq "$new_policy" '.schema == "ee.response.v2" and .success == true' \
    "new policy memory stores with a future as-of validity window"
assert_jq "$decision" '.schema == "ee.response.v2" and .success == true' \
    "decision memory stores with deterministic valid-from"
OLD_ID="$(printf '%s' "$old_policy" | jq -r '.data.memoryId // .data.memory_id // .data.id')"
NEW_ID="$(printf '%s' "$new_policy" | jq -r '.data.memoryId // .data.memory_id // .data.id')"
DECISION_ID="$(printf '%s' "$decision" | jq -r '.data.memoryId // .data.memory_id // .data.id')"
log_event "timeline_fixture_seeded" \
    bead "bd-2vq2z.16" \
    oldMemory "$OLD_ID" \
    newMemory "$NEW_ID" \
    decisionMemory "$DECISION_ID" \
    asOf "2026-05-02T12:00:00Z"

step "timeline reconstructs earlier state and changes since"
if ee_has_timeline_cli; then
    timeline_out="$(
        ee_json timeline "timeline audit" \
            --workspace "$WS" \
            --as-of "2026-05-02T12:00:00Z" \
            --limit 20 \
            --json
    )"
    log_event "timeline_json_observed" \
        bead "bd-2vq2z.16" \
        oldMemory "$OLD_ID" \
        newMemory "$NEW_ID" \
        decisionMemory "$DECISION_ID"
    assert_jq "$timeline_out" '.schema == "ee.response.v2" and .success == true' \
        "timeline returns a success response envelope"
    assert_jq "$timeline_out" '.data.schema == "ee.timeline.v1" and .data.command == "timeline"' \
        "timeline emits the stable ee.timeline.v1 data schema"
    assert_jq "$timeline_out" '.data.topic == "timeline audit" and .data.asOf == "2026-05-02T12:00:00Z"' \
        "timeline echoes topic and normalized as-of timestamp"
    assert_jq "$timeline_out" \
        "([.data.memoriesThen[].memoryId] | index(\"$OLD_ID\") != null)
            and ([.data.decisionsInEffect[].memoryId] | index(\"$DECISION_ID\") != null)" \
        "timeline includes memories and decisions in effect at as-of"
    assert_jq "$timeline_out" \
        "([.data.changesSince[] | select(.memoryId == \"$NEW_ID\" and .changeType == \"added\")] | length) == 1
            and ([.data.changesSince[] | select(.memoryId == \"$OLD_ID\" and .changeType == \"superseded\")] | length) == 1" \
        "timeline reports added and superseded changes since as-of"

    limited_out="$(
        ee_json timeline "timeline audit" \
            --workspace "$WS" \
            --as-of "2026-05-02T12:00:00Z" \
            --limit 1 \
            --json
    )"
    log_event "timeline_limit_assertion" \
        bead "bd-2vq2z.16" \
        limit "1"
    assert_jq "$limited_out" '.data.truncated == true and .data.totalMemoriesThen >= 2' \
        "timeline limit preserves totals and marks truncation"
else
    log_drop 1 "bd-2vq2z.16 installed ee binary lacks timeline route; source-built ee asserts ee.timeline.v1 memoriesThen, changesSince, decisionsInEffect, and limit truncation"
fi

end_temp_workspace
harness_summary
