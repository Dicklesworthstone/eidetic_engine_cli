#!/usr/bin/env bash
# AFR1 - flight recorder status / doctor posture and redaction canary.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
epic_setup "flight_recorder"

export EE_FLIGHT_RECORDER=1
export EE_FLIGHT_RECORDER_DIR="$EPIC_WORKSPACE/obs/flight_recorder"
export EE_FLIGHT_RECORDER_RETENTION_DAYS=7

STATUS_JSON="$(ee_workspace status --json)"
assert_jq "$STATUS_JSON" '.data.flightRecorder.schema // empty' "ee.flight_recorder.status.v1" "flight_recorder_status_schema"
assert_jq "$STATUS_JSON" '.data.flightRecorder.status // empty' "enabled" "flight_recorder_status_enabled"
assert_jq "$STATUS_JSON" '.data.flightRecorder.retentionDays // 0' "7" "flight_recorder_status_retention"
assert_jq "$STATUS_JSON" '.data.flightRecorder.redactionLevel // empty' "strict" "flight_recorder_status_redaction"
assert_jq "$STATUS_JSON" '(.data.posture.subsystems // [] | map(.id) | index("flight_recorder") != null)' "true" "flight_recorder_status_posture_row"

DOCTOR_JSON="$(ee_workspace doctor --json)"
assert_jq "$DOCTOR_JSON" '.data.flightRecorder.schema // empty' "ee.flight_recorder.status.v1" "flight_recorder_doctor_schema"
assert_jq "$DOCTOR_JSON" '(.data.checks // [] | map(.name) | index("flight_recorder") != null)' "true" "flight_recorder_doctor_check"
assert_jq "$DOCTOR_JSON" '(.data.checks // [] | map(select(.name == "flight_recorder")) | .[0].message | contains("redaction=strict"))' "true" "flight_recorder_doctor_redaction"

TRACE_JSON="$(ee_workspace recorder flight append \
    --verb context \
    --flag-name=--json \
    --flag-name=--max-tokens \
    --positional-arity 1 \
    --output-format json \
    --exit-code 0 \
    --elapsed-ms 12 \
    --response-bytes 128 \
    --harness-program codex-cli \
    --model-family gpt-5 \
    --memory-hash blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
    --degraded-code index_stale \
    --redaction-level strict \
    --trace-dir "$EE_FLIGHT_RECORDER_DIR" \
    --retention-days "$EE_FLIGHT_RECORDER_RETENTION_DAYS" \
    --max-bytes 1048576 \
    --json)"
assert_jq "$TRACE_JSON" '.data.schema // empty' "ee.flight_recorder.append.v1" "flight_recorder_append_schema"

if grep -R -E 'OPENAI_API_KEY|sk-proj-|raw task|raw query|password' "$EE_FLIGHT_RECORDER_DIR"; then
    e2e_log_assert_eq "redacted" "raw-secret-leak" "flight_recorder_redaction_canary" || true
else
    e2e_log_assert_eq "redacted" "redacted" "flight_recorder_redaction_canary"
fi

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
