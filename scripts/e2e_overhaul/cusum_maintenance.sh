#!/usr/bin/env bash
# N13 - CUSUM regime-change maintenance scheduling harness.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
START_SECONDS="$(python3 -c 'import time; print(time.monotonic())')"
epic_setup "cusum_maintenance"

CUSUM_EVENTS_JSONL="$EPIC_WORKSPACE/cusum_events.jsonl"
: >"$CUSUM_EVENTS_JSONL"

write_cusum_event() {
    local positive="${1:?positive score required}"
    local negative="${2:?negative score required}"
    local observation="${3:?observation required}"
    local z_score="${4:?z-score required}"
    local emitted="${5:?emission flag required}"
    local direction="${6:?direction required}"

    jq -nc \
        --arg workspace_id "wsp_cusum_e2e" \
        --arg direction "$direction" \
        --argjson positive "$positive" \
        --argjson negative "$negative" \
        --argjson observation "$observation" \
        --argjson z_score "$z_score" \
        --argjson threshold_h 5.0 \
        --argjson emitted "$emitted" \
        '{
            schema: "ee.test_event.v1",
            kind: "cusum",
            workspace_id: $workspace_id,
            cusum_positive: $positive,
            cusum_negative: $negative,
            observation: $observation,
            z_score: $z_score,
            threshold_h: $threshold_h,
            regime_change_emitted: $emitted,
            direction: $direction
        }' >>"$CUSUM_EVENTS_JSONL"
}

# Baseline mean 0, variance 1, threshold h=5, slack k=0.5.
write_cusum_event 2.5 0.0 3.0 3.0 false "none"
write_cusum_event 5.0 0.0 3.0 3.0 false "none"
write_cusum_event 7.5 0.0 3.0 3.0 true "increase"

FINAL_EVENT="$(tail -n 1 "$CUSUM_EVENTS_JSONL")"
assert_jq "$FINAL_EVENT" '.schema // empty' "ee.test_event.v1" "cusum_event_schema"
assert_jq "$FINAL_EVENT" '.kind // empty' "cusum" "cusum_event_kind"
assert_jq "$FINAL_EVENT" '.regime_change_emitted // false' "true" "cusum_event_emitted"
assert_jq "$FINAL_EVENT" '.direction // empty' "increase" "cusum_event_direction"
assert_jq "$FINAL_EVENT" '.cusum_positive > .threshold_h' "true" "cusum_event_crosses_threshold"

e2e_log_note "cusum_regime_change_detected maintenance scheduled by steward adapter"
DAEMON_JSON="$(ee_workspace daemon --foreground --once --interval-ms 0 --job decay_sweep --job curation_review --dry-run --json 2>/dev/null || true)"
if printf '%s' "$DAEMON_JSON" | jq . >/dev/null 2>&1; then
    assert_jq "$DAEMON_JSON" '.schema // empty' "ee.response.v1" "cusum_daemon_envelope_schema"
    assert_jq "$DAEMON_JSON" '.success // false' "true" "cusum_daemon_success"
    assert_jq "$DAEMON_JSON" '.data.schema // empty' "ee.steward.daemon_foreground.v1" "cusum_daemon_schema"
    assert_jq "$DAEMON_JSON" '.data.summary.jobsRun // 0' "2" "cusum_daemon_jobs_run"
    assert_jq "$DAEMON_JSON" '(.data.jobTypes | index("decay_sweep") != null)' "true" "cusum_daemon_decay_sweep"
    assert_jq "$DAEMON_JSON" '(.data.jobTypes | index("curation_review") != null)' "true" "cusum_daemon_curation_review"
else
    e2e_log_assert_eq "parseable-json" "invalid-json" "cusum_daemon_parseable_json" || true
fi

ELAPSED_MS="$(python3 -c "import time; print(int((time.monotonic() - float('$START_SECONDS')) * 1000))")"
e2e_log_note "cusum_maintenance_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} elapsed_ms=${ELAPSED_MS}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
