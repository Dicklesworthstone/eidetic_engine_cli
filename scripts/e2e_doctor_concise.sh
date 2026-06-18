#!/usr/bin/env bash
# bd-1et0v.15 - doctor concise-default E2E.
#
# Real-binary, no-Cargo harness. It proves default `ee doctor --json` is compact
# while `ee doctor --full --json` still exposes the exhaustive diagnostic blocks.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEST_ID="doctor_concise_e2e"
RUN_ID="$(date -u +"%Y%m%dT%H%M%SZ").${BASHPID:-$$}"
ROOT_BASE="${EE_E2E_TMPDIR:-${TMPDIR:-/tmp}}"
ARTIFACT_ROOT="$(mktemp -d "${ROOT_BASE%/}/ee-doctor-concise-e2e.XXXXXX")"
WORKSPACE="${ARTIFACT_ROOT}/workspace"
EVENT_LOG="${ARTIFACT_ROOT}/events.jsonl"
STDOUT_DIR="${ARTIFACT_ROOT}/stdout"
STDERR_DIR="${ARTIFACT_ROOT}/stderr"
FAILURES=0

mkdir -p "${WORKSPACE}" "${STDOUT_DIR}" "${STDERR_DIR}"

if ! command -v jq >/dev/null 2>&1; then
    printf '%s\n' '{"schema":"ee.test_event.v1","test_id":"doctor_concise_e2e","kind":"assert_result","fields":{"label":"jq_available","status":"fail","first_failure_diagnosis":"jq executable missing before harness init","stdout_artifact_path":"","stderr_artifact_path":"stderr","schema_validation_status":"not_run","redaction_status":"not_run"}}' >&2
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
    printf '[doctor-concise-e2e][STEP] %s %s\n' "${label}" "${detail}" >&2
    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg test_id "${TEST_ID}" \
        --arg label "${label}" \
        --arg detail "${detail}" \
        '{schema:$schema,test_id:$test_id,kind:"step",fields:{label:$label,detail:$detail}}' \
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
        printf '[doctor-concise-e2e][PASS] %s\n' "${label}" >&2
    else
        printf '[doctor-concise-e2e][FAIL] %s %s\n' "${label}" "${diagnosis}" >&2
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

run_ee_json() {
    local label="$1"
    shift
    local stdout_file="${STDOUT_DIR}/${label}.json"
    local stderr_file="${STDERR_DIR}/${label}.txt"
    local rc=0
    emit_note "command_start_${label}" "$*"
    "${EE_BIN}" "$@" >"${stdout_file}" 2>"${stderr_file}" || rc=$?
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
    LAST_STDERR="${stderr_file}"
    LAST_RC="${rc}"
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

emit_note "harness_start" "run_id=${RUN_ID} ee_bin=${EE_BIN} artifacts=${ARTIFACT_ROOT}"
emit_note "sanitized_env" "workspace_artifacts_retained=true raw_environment_not_logged=true"

emit_note "init_workspace" "initializing retained workspace"
run_ee_json "00_init" --workspace "${WORKSPACE}" --json init
INIT_JSON="${LAST_STDOUT}"
assert_jq_file "init_response_envelope" "${INIT_JSON}" '.schema == "ee.response.v2" and .success == true'

emit_note "doctor_default" "running compact default doctor JSON"
run_ee_json "01_doctor_default" --workspace "${WORKSPACE}" --json doctor
DEFAULT_JSON="${LAST_STDOUT}"
assert_jq_file "default_response_envelope" "${DEFAULT_JSON}" '.schema == "ee.response.v2" and .success == true'
assert_jq_file "default_mode_concise" "${DEFAULT_JSON}" '.fields == "doctor_concise" and .data.mode == "concise"'
assert_jq_file "default_core_checks_present" "${DEFAULT_JSON}" '(.data.coreChecks | type) == "array" and all(.data.coreChecks[]; .tier == "core" and (.severity | type == "string"))'
assert_jq_file "default_actionable_present" "${DEFAULT_JSON}" '(.data.actionable | type) == "array"'
assert_jq_file "default_advisory_summary_points_to_full" "${DEFAULT_JSON}" '(.data.advisorySummary.summary | contains("ee doctor --full --json")) and .data.advisorySummary.fullCommand == "ee doctor --full --json"'
assert_jq_file "default_omits_full_firehose" "${DEFAULT_JSON}" '(.data | has("checks") | not) and (.data | has("meshAutoEnrollment") | not) and (.data | has("rchWorkerPressure") | not) and (.data | has("verificationPosture") | not) and (.data | has("hostCalibration") | not)'

emit_note "doctor_full" "running exhaustive doctor JSON with --full"
run_ee_json "02_doctor_full" --workspace "${WORKSPACE}" --json doctor --full
FULL_JSON="${LAST_STDOUT}"
assert_jq_file "full_response_envelope" "${FULL_JSON}" '.schema == "ee.response.v2" and .success == true'
assert_jq_file "full_uses_exhaustive_profile" "${FULL_JSON}" '.fields == "full" and (.data.checks | type) == "array" and (.data.advisories | type) == "array"'
assert_jq_file "full_mesh_auto_enrollment_complete" "${FULL_JSON}" '.data.meshAutoEnrollment.schema == "ee.doctor.mesh_auto_enrollment.v1" and (.data.meshAutoEnrollment.checks | length) == 15'
assert_jq_file "full_rch_worker_pressure_present" "${FULL_JSON}" '.data.rchWorkerPressure.schema == "ee.rch.worker_pressure.v1"'
assert_jq_file "full_verification_posture_present" "${FULL_JSON}" '(.data.verificationPosture | type) == "object" and (.data.verificationLedger | type) == "object"'

DEFAULT_BYTES="$(wc -c <"${DEFAULT_JSON}" | tr -d '[:space:]')"
FULL_BYTES="$(wc -c <"${FULL_JSON}" | tr -d '[:space:]')"
emit_note "size_budget" "default_bytes=${DEFAULT_BYTES} full_bytes=${FULL_BYTES}"
assert_less_than "default_smaller_than_full" "${DEFAULT_BYTES}" "${FULL_BYTES}"
assert_less_than "default_under_12kb_budget" "${DEFAULT_BYTES}" "12000"

jq -cn \
    --arg schema "ee.test_event.v1" \
    --arg test_id "${TEST_ID}" \
    --arg artifact_root "${ARTIFACT_ROOT}" \
    --arg event_log "${EVENT_LOG}" \
    --argjson failures "${FAILURES}" \
    '{schema:$schema,test_id:$test_id,kind:"summary",fields:{artifact_root:$artifact_root,event_log:$event_log,failures:$failures,status:(if $failures == 0 then "pass" else "fail" end)}}' \
    | tee -a "${EVENT_LOG}" >&2

printf 'doctor concise e2e artifacts: %s\n' "${ARTIFACT_ROOT}" >&2
if [ "${FAILURES}" -ne 0 ]; then
    summary_stdout_artifact_path="${DEFAULT_JSON:-}"
    summary_stderr_artifact_path="${FULL_JSON:-}"
    emit_assert_result "doctor_concise_e2e_summary" "fail" "one or more doctor concise e2e assertions failed" "${summary_stdout_artifact_path}" "${summary_stderr_artifact_path}" "not_applicable"
    exit 1
fi
