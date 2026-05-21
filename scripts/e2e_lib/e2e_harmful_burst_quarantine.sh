#!/usr/bin/env bash
# bd-3qs2i.3.6 — F3.6 e2e for `harmful_burst_quarantine`.
#
# This exercises the full agent flow against a real ee binary in a fresh
# workspace. The production quarantine queue is exposed through
# `ee outcome quarantine list`; `ee curate candidates` does not currently
# materialize feedback-quarantine rows.
#
# Main burst assertions pin a 5-minute window so ordinary CI timing cannot
# expire prior events. The explicit window-boundary check uses a separate
# source-id with a 1-second per-call window to avoid a long sleep.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

WORKSPACE="${WORKSPACE:-$(mktemp -d -t ee-e2e-f3-XXXX)}"
export WORKSPACE

# shellcheck source=scripts/e2e_lib/agent_ergonomics_lib.sh
# shellcheck disable=SC1091
source "$SCRIPT_DIR/agent_ergonomics_lib.sh"

export EE_HARMFUL_PER_SOURCE_PER_HOUR=3
export EE_HARMFUL_BURST_WINDOW_SECONDS=300

finalize_with_harmful_burst_env_cleanup() {
    local rc=$?
    unset EE_HARMFUL_PER_SOURCE_PER_HOUR
    unset EE_HARMFUL_BURST_WINDOW_SECONDS

    if [ "$rc" -eq 0 ]; then
        finalize
        return $?
    fi

    set +e
    false
    finalize
    return "$rc"
}

trap finalize_with_harmful_burst_env_cleanup EXIT

ee_workspace() {
    "$EE_BIN" --workspace "$WORKSPACE" "$@"
}

has_harmful_burst='((.data.degraded // []) | map(.code) | contains(["harmful_burst_quarantine"]))'

send_harmful() {
    local source_id="${1:?source_id required}"
    local reason="${2:?reason required}"
    local cap="${3:-$EE_HARMFUL_PER_SOURCE_PER_HOUR}"
    local window_seconds="${4:-$EE_HARMFUL_BURST_WINDOW_SECONDS}"

    ee_workspace outcome "$mem_id" \
        --signal harmful \
        --source-id "$source_id" \
        --reason "$reason" \
        --actor agent-e2e \
        --harmful-per-source-per-hour "$cap" \
        --harmful-burst-window-seconds "$window_seconds" \
        --json
}

# ---------------------------------------------------------------------------
log_step "Initialize fresh workspace"
init_out=$(ee_workspace init --json)
assert_jq "$init_out" '.success' "true" "init returns success=true"

# ---------------------------------------------------------------------------
log_step "Verify workspace is fresh (no prior outcomes)"
fresh_out=$(ee_workspace memory list --json)
assert_jq "$fresh_out" '(.data.memories // []) | length' "0" \
    "workspace has zero memories"

# ---------------------------------------------------------------------------
log_step "Plant a victim memory to protect from burst noise"
remember_out=$(ee_workspace remember "Victim memory for harmful burst quarantine." \
    --level procedural \
    --kind rule \
    --no-propose-candidates \
    --json)
assert_jq "$remember_out" '.success' "true" "remember returns success=true"
mem_id=$(printf '%s' "$remember_out" | jq -r '.data.memory_id // .data.memoryId // empty')
if [ -z "$mem_id" ]; then
    record_failure "memory_id_present" "remember response did not include data.memory_id"
    exit 1
fi
record_pass "memory_id_present"

before_why=$(ee_workspace why "$mem_id" --json)
trust_before=$(printf '%s' "$before_why" | jq -r '.data.storage.trustClass // empty')
if [ -z "$trust_before" ]; then
    record_failure "initial_trust_class_present" "why response did not include data.storage.trustClass"
    exit 1
fi
record_pass "initial_trust_class_present"

# ---------------------------------------------------------------------------
log_step "Send 3 harmful events under cap"
for i in 1 2 3; do
    under_cap_out=$(send_harmful stress "under-cap harmful signal $i")
    assert_jq "$under_cap_out" "$has_harmful_burst" "false" \
        "event $i is not quarantined under cap"
done

# ---------------------------------------------------------------------------
log_step "4th harmful event trips quarantine"
trip_out=$(send_harmful stress "trip harmful burst quarantine")
assert_jq "$trip_out" '.success' "true" "quarantined outcome call succeeds"
assert_jq "$trip_out" '.data.status' "feedback_quarantined" \
    "outcome status is feedback_quarantined"
assert_jq "$trip_out" "$has_harmful_burst" "true" \
    "harmful_burst_quarantine degraded code fires"
assert_jq "$trip_out" \
    '(.data.degraded[] | select(.code == "harmful_burst_quarantine") | .details.observedRate >= 4)' \
    "true" "observed rate includes the quarantined event"
assert_jq "$trip_out" \
    '(.data.degraded[] | select(.code == "harmful_burst_quarantine") | .details.configuredCap)' \
    "3" "configured cap is surfaced"
assert_jq "$trip_out" \
    '(.data.degraded[] | select(.code == "harmful_burst_quarantine") | .details.windowSeconds)' \
    "300" "window seconds is surfaced"
assert_jq "$trip_out" \
    '(.data.degraded[] | select(.code == "harmful_burst_quarantine") | .details.recovery | length)' \
    "3" "recovery has three actions"
assert_jq "$trip_out" \
    '(.data.degraded[] | select(.code == "harmful_burst_quarantine") | .details.recovery | map(.kind) | join(","))' \
    "narrow,config,flag" "recovery kinds are narrow/config/flag"

# ---------------------------------------------------------------------------
log_step "Quarantine does not alter target trust class"
after_why=$(ee_workspace why "$mem_id" --json)
assert_jq "$after_why" '.data.storage.trustClass' "$trust_before" \
    "trust class unchanged by quarantined event"

# ---------------------------------------------------------------------------
log_step "Quarantined feedback row is inspectable"
queue_out=$(ee_workspace outcome quarantine list --json)
assert_jq "$queue_out" '.data.queueDepth >= 1' "true" \
    "feedback quarantine queue has a pending row"
assert_jq "$queue_out" \
    "(.data.records // []) | map(select(.targetId == \"$mem_id\" and .status == \"pending\")) | length >= 1" \
    "true" "pending quarantine row targets victim memory"
quarantine_id=$(printf '%s' "$queue_out" \
    | jq -r --arg mem_id "$mem_id" \
        '(.data.records // [])[] | select(.targetId == $mem_id and .status == "pending") | .id' \
    | head -n 1)
if [ -z "$quarantine_id" ]; then
    record_failure "quarantine_id_present" "pending quarantine row had no id"
    exit 1
fi
record_pass "quarantine_id_present"

# ---------------------------------------------------------------------------
log_step "Different source-id is isolated from the burst"
isolated_out=$(send_harmful different "isolated source should not inherit stress burst")
assert_jq "$isolated_out" "$has_harmful_burst" "false" \
    "different source-id is not quarantined"

# ---------------------------------------------------------------------------
log_step "Per-call cap override takes effect"
override_first=$(send_harmful override "override cap first event" 1)
assert_jq "$override_first" "$has_harmful_burst" "false" \
    "first event under cap=1 is live"
override_second=$(send_harmful override "override cap second event" 1)
assert_jq "$override_second" "$has_harmful_burst" "true" \
    "second event under cap=1 is quarantined"

# ---------------------------------------------------------------------------
log_step "After window expiry, burst quarantine clears"
window_first=$(send_harmful window-boundary "window boundary first event" 1 1)
assert_jq "$window_first" "$has_harmful_burst" "false" \
    "first 1-second-window event is live"
sleep 2
window_second=$(send_harmful window-boundary "window boundary second event after expiry" 1 1)
assert_jq "$window_second" "$has_harmful_burst" "false" \
    "second event after 1-second window expiry is live"

# ---------------------------------------------------------------------------
log_step "Tracing target fires on a quarantined burst"
trace_log="$LOG_DIR/trace.log"
EE_LOG_JSON=1 "$EE_BIN" --workspace "$WORKSPACE" outcome "$mem_id" \
    --signal harmful \
    --source-id trace-target \
    --reason "trace first event" \
    --actor agent-e2e \
    --harmful-per-source-per-hour 1 \
    --harmful-burst-window-seconds "$EE_HARMFUL_BURST_WINDOW_SECONDS" \
    --json >"$LOG_DIR/trace_first.json" 2>"$trace_log"
EE_LOG_JSON=1 "$EE_BIN" --workspace "$WORKSPACE" outcome "$mem_id" \
    --signal harmful \
    --source-id trace-target \
    --reason "trace second event" \
    --actor agent-e2e \
    --harmful-per-source-per-hour 1 \
    --harmful-burst-window-seconds "$EE_HARMFUL_BURST_WINDOW_SECONDS" \
    --json >"$LOG_DIR/trace_second.json" 2>>"$trace_log"
assert_contains "$(cat "$trace_log")" "ee::outcome::harmful_burst" \
    "tracing target ee::outcome::harmful_burst emitted"
assert_contains "$(cat "$trace_log")" "harmful burst quarantined" \
    "trace event describes harmful burst quarantine"

unset EE_HARMFUL_PER_SOURCE_PER_HOUR
unset EE_HARMFUL_BURST_WINDOW_SECONDS
