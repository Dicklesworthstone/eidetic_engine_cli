#!/usr/bin/env bash
# bd-1et0v.21 - doctor health E2E.
#
# Real-binary, no-Cargo harness. It proves the default doctor surface is green
# and compact on an initialized workspace while --full retains exhaustive
# advisory diagnostics, host calibration, and advisory-isolation evidence.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEST_ID="doctor_health_e2e"
RUN_ID="$(date -u +"%Y%m%dT%H%M%SZ").${BASHPID:-$$}"
ROOT_BASE="${EE_E2E_TMPDIR:-${TMPDIR:-/tmp}}"
ARTIFACT_ROOT="$(mktemp -d "${ROOT_BASE%/}/ee-doctor-health-e2e.XXXXXX")"
WORKSPACE="${ARTIFACT_ROOT}/workspace"
EVENT_LOG="${ARTIFACT_ROOT}/events.jsonl"
STDOUT_DIR="${ARTIFACT_ROOT}/stdout"
STDERR_DIR="${ARTIFACT_ROOT}/stderr"
TOOL_DIR="${ARTIFACT_ROOT}/tools"
FAILURES=0

mkdir -p "${WORKSPACE}" "${STDOUT_DIR}" "${STDERR_DIR}" "${TOOL_DIR}"

if ! command -v jq >/dev/null 2>&1; then
    printf '%s\n' '{"schema":"ee.test_event.v1","test_id":"doctor_health_e2e","kind":"assert_result","fields":{"label":"jq_available","status":"fail","first_failure_diagnosis":"jq executable missing before harness init","stdout_artifact_path":"","stderr_artifact_path":"stderr","schema_validation_status":"not_run","redaction_status":"not_run"}}' >&2
    exit 1
fi

resolve_ee_bin() {
    if [ -n "${EE_BIN:-}" ]; then
        printf '%s' "${EE_BIN}"
        return 0
    fi
    if [ -n "${EE_BINARY:-}" ]; then
        printf '%s' "${EE_BINARY}"
        return 0
    fi
    if command -v ee >/dev/null 2>&1; then
        command -v ee
        return 0
    fi
    printf ''
}

EE_BIN="$(resolve_ee_bin || true)"
if [ -z "${EE_BIN}" ] || [ ! -x "${EE_BIN}" ]; then
    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg test_id "${TEST_ID}" \
        --arg label "ee_binary_available" \
        --arg diag "prebuilt ee binary missing; set EE_BIN or EE_BINARY" \
        '{schema:$schema,test_id:$test_id,kind:"assert_result",fields:{label:$label,status:"fail",first_failure_diagnosis:$diag,stdout_artifact_path:"",stderr_artifact_path:"stderr",schema_validation_status:"not_run",redaction_status:"not_run"}}' \
        | tee -a "${EVENT_LOG}" >&2
    exit 1
fi

emit_event() {
    local kind="$1"
    local label="$2"
    local status="$3"
    local diagnosis="$4"
    local stdout_path="$5"
    local stderr_path="$6"
    local schema_status="$7"
    local redaction_status="$8"
    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg test_id "${TEST_ID}" \
        --arg kind "${kind}" \
        --arg label "${label}" \
        --arg status "${status}" \
        --arg diagnosis "${diagnosis}" \
        --arg stdout_path "${stdout_path}" \
        --arg stderr_path "${stderr_path}" \
        --arg schema_status "${schema_status}" \
        --arg redaction_status "${redaction_status}" \
        '{schema:$schema,test_id:$test_id,kind:$kind,fields:{label:$label,status:$status,first_failure_diagnosis:$diagnosis,stdout_artifact_path:$stdout_path,stderr_artifact_path:$stderr_path,schema_validation_status:$schema_status,redaction_status:$redaction_status}}' \
        | tee -a "${EVENT_LOG}" >&2
}

emit_note() {
    local label="$1"
    local detail="$2"
    printf '[doctor-health-e2e][STEP] %s %s\n' "${label}" "${detail}" >&2
    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg test_id "${TEST_ID}" \
        --arg label "${label}" \
        --arg detail "${detail}" \
        '{schema:$schema,test_id:$test_id,kind:"step",fields:{label:$label,detail:$detail}}' \
        | tee -a "${EVENT_LOG}" >&2
}

emit_metric() {
    local label="$1"
    local json="$2"
    printf '[doctor-health-e2e][METRIC] %s %s\n' "${label}" "${json}" >&2
    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg test_id "${TEST_ID}" \
        --arg label "${label}" \
        --argjson values "${json}" \
        '{schema:$schema,test_id:$test_id,kind:"metric",fields:{label:$label,values:$values}}' \
        | tee -a "${EVENT_LOG}" >&2
}

emit_assert_result() {
    local label="$1"
    local status="$2"
    local diagnosis="$3"
    local stdout_path="$4"
    local stderr_path="$5"
    local schema_status="$6"
    emit_event "assert_result" "${label}" "${status}" "${diagnosis}" "${stdout_path}" "${stderr_path}" "${schema_status}" "passed"
    if [ "${status}" = "pass" ]; then
        printf '[doctor-health-e2e][PASS] %s\n' "${label}" >&2
    else
        printf '[doctor-health-e2e][FAIL] %s %s\n' "${label}" "${diagnosis}" >&2
        FAILURES=$((FAILURES + 1))
    fi
}

emit_assert_ok() {
    local label="$1"
    local stdout_path="$2"
    local stderr_path="$3"
    local schema_status="$4"
    emit_assert_result "${label}" "pass" "none" "${stdout_path}" "${stderr_path}" "${schema_status}"
}

quote_command() {
    local part
    for part in "$@"; do
        printf '%q ' "${part}"
    done
}

run_ee_json() {
    local label="$1"
    shift
    run_ee_json_with_env "${label}" -- "$@"
}

run_ee_json_with_env() {
    local label="$1"
    shift
    local -a env_args=()
    if [ "${1:-}" = "--" ]; then
        shift
    fi
    while [ "$#" -gt 0 ] && [[ "$1" == *=* ]]; do
        env_args+=("$1")
        shift
    done
    local stdout_file="${STDOUT_DIR}/${label}.json"
    local stderr_file="${STDERR_DIR}/${label}.txt"
    local rc=0
    local command_display
    command_display="$(quote_command "${env_args[@]}" "${EE_BIN}" "$@")"
    emit_note "command_start_${label}" "${command_display}"
    env "${env_args[@]}" "${EE_BIN}" "$@" >"${stdout_file}" 2>"${stderr_file}" || rc=$?
    local schema_status="failed"
    local diagnosis="none"
    if jq -e 'type == "object" and (.schema? | type == "string")' "${stdout_file}" >/dev/null 2>&1; then
        schema_status="passed"
    elif [ "${rc}" -eq 0 ]; then
        diagnosis="stdout was not a JSON envelope"
    fi
    emit_event "command_end" "${label}" "$([ "${rc}" -eq 0 ] && printf pass || printf fail)" "${diagnosis}" "${stdout_file}" "${stderr_file}" "${schema_status}" "passed"
    if [ "${rc}" -ne 0 ]; then
        emit_assert_result "${label}_exit_zero" "fail" "command exited ${rc}" "${stdout_file}" "${stderr_file}" "${schema_status}"
    fi
    LAST_STDOUT="${stdout_file}"
}

assert_jq_file() {
    local label="$1"
    local file="$2"
    local filter="$3"
    if jq -e "${filter}" "${file}" >/dev/null 2>&1; then
        emit_assert_ok "${label}" "${file}" "" "passed"
    else
        emit_assert_result "${label}" "fail" "jq filter failed: ${filter}" "${file}" "" "failed"
    fi
}

assert_jq_files() {
    local label="$1"
    local filter="$2"
    shift 2
    if jq -s -e "${filter}" "$@" >/dev/null 2>&1; then
        emit_assert_ok "${label}" "$*" "" "passed"
    else
        emit_assert_result "${label}" "fail" "jq -s filter failed: ${filter}" "$*" "" "failed"
    fi
}

assert_less_than() {
    local label="$1"
    local actual="$2"
    local expected="$3"
    if [ "${actual}" -lt "${expected}" ]; then
        emit_assert_ok "${label}" "" "" "not_applicable"
    else
        emit_assert_result "${label}" "fail" "expected ${actual} < ${expected}" "" "" "not_applicable"
    fi
}

json_or_null() {
    local file="$1"
    local filter="$2"
    jq -c "${filter}" "${file}" 2>/dev/null || printf 'null'
}

write_fake_rch() {
    local fake="${TOOL_DIR}/rch"
    cat >"${fake}" <<'FAKE_RCH'
#!/usr/bin/env bash
if [ "$1" = "status" ] && [ "$2" = "--workers" ] && [ "$3" = "--jobs" ] && [ "$4" = "--json" ]; then
    cat <<'JSON'
{
  "schema": "rch.status.v1",
  "workers": [
    {
      "id": "e2e-worker-a",
      "pressureState": "critical",
      "admissionImpact": "blocked",
      "reasonCode": "disk_pressure_critical",
      "telemetryFreshness": "current",
      "freeGb": 1
    },
    {
      "id": "e2e-worker-b",
      "pressureState": "critical",
      "admissionImpact": "blocked",
      "reasonCode": "disk_pressure_critical",
      "telemetryFreshness": "current",
      "freeGb": 1
    }
  ],
  "jobs": []
}
JSON
    exit 0
fi
printf 'unexpected fake rch invocation: %s\n' "$*" >&2
exit 2
FAKE_RCH
    chmod +x "${fake}"
}

emit_note "harness_start" "run_id=${RUN_ID} ee_bin=${EE_BIN} artifacts=${ARTIFACT_ROOT} repo=${ROOT}"
emit_note "sanitized_env" "workspace_artifacts_retained=true raw_environment_not_logged=true"

emit_note "init_workspace" "initializing retained workspace"
run_ee_json "00_init" --workspace "${WORKSPACE}" --json init
INIT_JSON="${LAST_STDOUT}"
assert_jq_file "init_response_envelope" "${INIT_JSON}" '.schema == "ee.response.v2" and .success == true'

emit_note "doctor_default_green" "running compact default doctor JSON on initialized workspace"
run_ee_json "01_doctor_default" --workspace "${WORKSPACE}" --json doctor
DEFAULT_JSON="${LAST_STDOUT}"
assert_jq_file "default_response_envelope" "${DEFAULT_JSON}" '.schema == "ee.response.v2" and .success == true'
assert_jq_file "default_mode_concise" "${DEFAULT_JSON}" '.fields == "doctor_concise" and .data.mode == "concise"'
assert_jq_file "default_topline_green" "${DEFAULT_JSON}" '.data.healthy == true and (.data.posture == "ok" or .data.posture == "ready")'
assert_jq_file "default_core_focused" "${DEFAULT_JSON}" '(.data.coreChecks | type) == "array" and (0 < (.data.coreChecks | length)) and ((.data.coreChecks | length) <= 6) and all(.data.coreChecks[]; .tier == "core" and .severity == "ok")'
assert_jq_file "default_actionable_core_only" "${DEFAULT_JSON}" '(.data.actionable | type) == "array" and all(.data.actionable[]?; .tier == "core")'
assert_jq_file "default_advisory_summary_compact" "${DEFAULT_JSON}" '(.data.advisorySummary.total | type) == "number" and (.data.advisorySummary.summary | contains("ee doctor --full --json"))'
assert_jq_file "default_omits_full_firehose" "${DEFAULT_JSON}" '(.data | has("checks") | not) and (.data | has("advisories") | not) and (.data | has("meshAutoEnrollment") | not) and (.data | has("rchWorkerPressure") | not) and (.data | has("verificationPosture") | not) and (.data | has("verificationLedger") | not) and (.data | has("hostCalibration") | not)'
assert_jq_file "default_no_calibration_downgrade_reason" "${DEFAULT_JSON}" '(tostring | contains("swarm->portable") | not) and (tostring | contains("lower_to_recommended_profile") | not) and (tostring | contains("conservative_calibration_missing") | not)'

DEFAULT_BYTES="$(wc -c <"${DEFAULT_JSON}" | tr -d '[:space:]')"
DEFAULT_KEY_COUNT="$(jq '.data | keys | length' "${DEFAULT_JSON}")"
DEFAULT_METRICS="$(jq -c --argjson bytes "${DEFAULT_BYTES}" --argjson key_count "${DEFAULT_KEY_COUNT}" '{posture:.data.posture,healthy:.data.healthy,coreCheckCount:(.data.coreChecks|length),actionableCount:(.data.actionable|length),advisorySummary:.data.advisorySummary,bytes:$bytes,dataKeyCount:$key_count}' "${DEFAULT_JSON}")"
emit_metric "default_doctor_summary" "${DEFAULT_METRICS}"

emit_note "doctor_full_parity" "running exhaustive doctor JSON with --full"
run_ee_json "02_doctor_full" --workspace "${WORKSPACE}" --json doctor --full
FULL_JSON="${LAST_STDOUT}"
assert_jq_file "full_response_envelope" "${FULL_JSON}" '.schema == "ee.response.v2" and .success == true'
assert_jq_file "full_topline_green" "${FULL_JSON}" '.data.healthy == true and (.data.posture == "ok" or .data.posture == "ready")'
assert_jq_file "full_uses_exhaustive_profile" "${FULL_JSON}" '.fields == "full" and (.data.checks | type) == "array" and (.data.advisories | type) == "array"'
assert_jq_file "full_mesh_auto_enrollment_complete" "${FULL_JSON}" '.data.meshAutoEnrollment.schema == "ee.doctor.mesh_auto_enrollment.v1" and (.data.meshAutoEnrollment.checks | length) == 15'
assert_jq_file "full_rch_worker_pressure_present" "${FULL_JSON}" '.data.rchWorkerPressure.schema == "ee.rch.worker_pressure.v1" and (.data.rchWorkerPressure.workers | type) == "array"'
assert_jq_file "full_verification_posture_present" "${FULL_JSON}" '(.data.verificationPosture | type) == "object" and (.data.verificationLedger | type) == "object"'
assert_jq_file "full_host_calibration_budget_deltas_present" "${FULL_JSON}" '.data.hostCalibration.schema == "ee.host_calibration.posture.v1" and (.data.hostCalibration.effectiveProfile | type) == "string" and (.data.hostCalibration.budgetDeltas | type) == "array" and (.data.hostCalibration.budgetDeltas | length) >= 5 and all(.data.hostCalibration.budgetDeltas[]; (.surface | type) == "string" and (.reasonCode | type) == "string")'
assert_jq_file "full_host_calibration_auto_calibrated_fresh" "${FULL_JSON}" '.data.hostCalibration.calibrationFreshness == "fresh" and ((.data.hostCalibration.reasonCodes // []) | index("calibration_missing") | not) and ((.data.hostCalibration.reasonCodes // []) | index("conservative_calibration_missing") | not)'
# shellcheck disable=SC2016
assert_jq_files "full_retains_default_core_check_verdicts" '.[1].data.checks as $full | all(.[0].data.coreChecks[]?; . as $core | any($full[]?; .name == $core.name and .tier == $core.tier and .severity == $core.severity))' "${DEFAULT_JSON}" "${FULL_JSON}"

FULL_BYTES="$(wc -c <"${FULL_JSON}" | tr -d '[:space:]')"
FULL_METRICS="$(jq -c --argjson bytes "${FULL_BYTES}" '{posture:.data.posture,healthy:.data.healthy,checkCount:(.data.checks|length),advisoryCount:(.data.advisories|length),rchStatus:.data.rchWorkerPressure.status,hostCalibration:{freshness:.data.hostCalibration.calibrationFreshness,effectiveProfile:.data.hostCalibration.effectiveProfile,budgetDeltaCount:(.data.hostCalibration.budgetDeltas|length)},bytes:$bytes}' "${FULL_JSON}")"
emit_metric "full_doctor_summary" "${FULL_METRICS}"
assert_less_than "default_smaller_than_full" "${DEFAULT_BYTES}" "${FULL_BYTES}"
assert_less_than "default_under_12kb_budget" "${DEFAULT_BYTES}" "12000"
assert_jq_file "full_exceeds_default_sections" "${FULL_JSON}" "(.data | keys | length) > ${DEFAULT_KEY_COUNT}"

emit_note "diag_host_profile" "proving the raw host-profile diagnostic remains real-binary JSON"
run_ee_json "03_diag_host_profile" --workspace "${WORKSPACE}" --json diag host-profile
HOST_PROFILE_JSON="${LAST_STDOUT}"
assert_jq_file "host_profile_response_envelope" "${HOST_PROFILE_JSON}" '.schema == "ee.response.v2" and .success == true'
assert_jq_file "host_profile_raw_probe_shape" "${HOST_PROFILE_JSON}" '.data.schema == "ee.host_profile.v1" and .data.sideEffectFree == true and (.data.redaction | test("paths_presence_only_env")) and (.data.paths | type) == "array" and (.data.topology.rch.status | type) == "string"'

emit_note "cass_limited_advisory_probe" "forcing invalid EE_CASS_BINARY to simulate limited optional CASS capability"
BAD_CASS="${TOOL_DIR}/not-cass"
printf '#!/usr/bin/env bash\nexit 0\n' >"${BAD_CASS}"
chmod +x "${BAD_CASS}"
run_ee_json_with_env "04_doctor_full_cass_limited" -- "EE_CASS_BINARY=${BAD_CASS}" --workspace "${WORKSPACE}" --json doctor --full
CASS_LIMITED_JSON="${LAST_STDOUT}"
assert_jq_file "cass_limited_topline_green" "${CASS_LIMITED_JSON}" '.data.healthy == true and (.data.posture == "ok" or .data.posture == "ready")'
assert_jq_file "cass_limited_is_advisory_ok" "${CASS_LIMITED_JSON}" 'any(.data.checks[]?; .name == "cass" and .tier == "advisory" and .severity == "ok" and ((.message // "") | contains("cass_limited")) and ((.repair // "") | contains("cass health")))'
assert_jq_file "cass_limited_not_actionable_default" "${CASS_LIMITED_JSON}" 'all(.data.checks[]?; (.tier == "advisory" and .name == "cass") or .name != "cass")'
emit_metric "cass_limited_summary" "$(json_or_null "${CASS_LIMITED_JSON}" '{posture:.data.posture,healthy:.data.healthy,cassCheck:(.data.checks[] | select(.name=="cass"))}')"

emit_note "rch_blocked_advisory_probe" "shadowing rch with deterministic pressure-blocked worker JSON"
write_fake_rch
run_ee_json_with_env "05_doctor_full_rch_blocked" -- "PATH=${TOOL_DIR}:${PATH}" --workspace "${WORKSPACE}" --json doctor --full
RCH_BLOCKED_JSON="${LAST_STDOUT}"
assert_jq_file "rch_blocked_topline_green" "${RCH_BLOCKED_JSON}" '.data.healthy == true and (.data.posture == "ok" or .data.posture == "ready")'
assert_jq_file "rch_blocked_pressure_status" "${RCH_BLOCKED_JSON}" '.data.rchWorkerPressure.status == "healthy_but_pressure_blocked" and .data.rchWorkerPressure.blockedWorkerCount == 2 and .data.rchWorkerPressure.usableWorkerCount == 0'
assert_jq_file "rch_blocked_is_advisory_ok" "${RCH_BLOCKED_JSON}" 'any(.data.checks[]?; .name == "rch_worker_pressure" and .tier == "advisory" and .severity == "ok" and ((.message // "") | contains("rch_worker_pressure_advisory")) and ((.repair // "") | contains("rch status --workers --jobs --json")))'
emit_metric "rch_blocked_summary" "$(json_or_null "${RCH_BLOCKED_JSON}" '{posture:.data.posture,healthy:.data.healthy,rchWorkerPressure:.data.rchWorkerPressure,rchCheck:(.data.checks[] | select(.name=="rch_worker_pressure"))}')"

emit_note "default_under_advisory_probes" "default doctor remains concise under synthetic advisory inputs"
run_ee_json_with_env "06_doctor_default_cass_and_rch" -- "EE_CASS_BINARY=${BAD_CASS}" "PATH=${TOOL_DIR}:${PATH}" --workspace "${WORKSPACE}" --json doctor
DEFAULT_ADVISORY_JSON="${LAST_STDOUT}"
assert_jq_file "default_advisory_probe_topline_green" "${DEFAULT_ADVISORY_JSON}" '.data.healthy == true and (.data.posture == "ok" or .data.posture == "ready")'
assert_jq_file "default_advisory_probe_no_firehose" "${DEFAULT_ADVISORY_JSON}" '.fields == "doctor_concise" and (.data | has("checks") | not) and (.data | has("rchWorkerPressure") | not) and (.data | has("hostCalibration") | not)'
assert_jq_file "default_advisory_probe_has_nonzero_summary" "${DEFAULT_ADVISORY_JSON}" '(.data.advisorySummary.total | type) == "number" and (.data.advisorySummary.total > 0)'

jq -s -e 'length > 0 and all(.[]; .schema == "ee.test_event.v1" and (.test_id == "doctor_health_e2e") and (.kind | type == "string") and (.fields | type == "object"))' "${EVENT_LOG}" >/dev/null 2>&1
EVENT_LOG_RC=$?
if [ "${EVENT_LOG_RC}" -eq 0 ]; then
    emit_assert_ok "event_log_schema_shape" "${EVENT_LOG}" "" "passed"
else
    emit_assert_result "event_log_schema_shape" "fail" "event log contains a non ee.test_event.v1 entry" "${EVENT_LOG}" "" "failed"
fi

jq -cn \
    --arg schema "ee.test_event.v1" \
    --arg test_id "${TEST_ID}" \
    --arg artifact_root "${ARTIFACT_ROOT}" \
    --arg event_log "${EVENT_LOG}" \
    --argjson failures "${FAILURES}" \
    '{schema:$schema,test_id:$test_id,kind:"summary",fields:{artifact_root:$artifact_root,event_log:$event_log,failures:$failures,status:(if $failures == 0 then "pass" else "fail" end)}}' \
    | tee -a "${EVENT_LOG}" >&2

printf 'doctor health e2e artifacts: %s\n' "${ARTIFACT_ROOT}" >&2
if [ "${FAILURES}" -ne 0 ]; then
    emit_assert_result "doctor_health_e2e_summary" "fail" "one or more doctor health e2e assertions failed" "${DEFAULT_JSON:-}" "${FULL_JSON:-}" "not_applicable"
    exit 1
fi
