#!/usr/bin/env bash
# bd-1et0v.22 - ergonomics E2E for context alias parity and PATH shadow advisory.
#
# Real-binary, no-Cargo E2E:
#   - `ee context` and `ee pack` select the same pack content, while context
#     carries only the expected deprecated_alias info row.
#   - a shadowed/stale `ee` earlier on PATH is reported as an advisory-only
#     doctor finding with an offline/no-network repair hint.
#
# The script intentionally retains artifacts. AGENTS.md forbids implicit
# recursive cleanup during agent sessions.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

if ! command -v jq >/dev/null 2>&1; then
    echo "e2e_ergonomics: jq is required" >&2
    printf '%s\n' '{"schema":"ee.test_event.v1","test_id":"ergonomics_e2e","kind":"assert_result","fields":{"label":"jq_available","status":"fail","first_failure_diagnosis":"jq executable missing before harness init","stdout_artifact_path":"","stderr_artifact_path":"stderr","schema_validation_status":"not_run","redaction_status":"not_run"}}' >&2
    exit 3
fi

EE_BIN="${EE_BIN:-${EE_BINARY:-ee}}"
if [[ "${EE_BIN}" == */* ]]; then
    REAL_EE="${EE_BIN}"
else
    REAL_EE="$(command -v "${EE_BIN}" 2>/dev/null || true)"
fi
if [[ -z "${REAL_EE}" || ! -x "${REAL_EE}" ]]; then
    echo "e2e_ergonomics: ee binary not found or not executable: ${EE_BIN}" >&2
    echo "e2e_ergonomics: provide EE_BIN or EE_BINARY pointing at a prebuilt ee" >&2
    printf '%s\n' '{"schema":"ee.test_event.v1","test_id":"ergonomics_e2e","kind":"assert_result","fields":{"label":"ee_binary_available","status":"fail","first_failure_diagnosis":"prebuilt ee binary missing before harness init","stdout_artifact_path":"","stderr_artifact_path":"stderr","schema_validation_status":"not_run","redaction_status":"not_run"}}' >&2
    exit 3
fi

ROOT_BASE="${EE_E2E_TMPDIR:-${TMPDIR:-/tmp}}"
ROOT="$(mktemp -d "${ROOT_BASE%/}/ee-ergonomics-e2e.XXXXXX")"
WORKSPACE="${ROOT}/workspace"
LOG_DIR="${ROOT}/logs"
EVENT_LOG="${LOG_DIR}/events.jsonl"
mkdir -p "${WORKSPACE}" "${LOG_DIR}"
: >"${EVENT_LOG}"

FAILURES=0
TEST_ID="ergonomics_e2e"
REDACTION_STATUS="local_workspace_artifacts_retained"

now_ms() {
    python3 -c 'import time; print(int(time.time() * 1000))'
}

now_iso() {
    date -u +"%Y-%m-%dT%H:%M:%SZ"
}

args_json() {
    if [[ "$#" -eq 0 ]]; then
        printf '[]'
        return
    fi
    printf '%s\0' "$@" | jq -Rs 'split("\u0000")[:-1]'
}

sanitized_env_json() {
    local path_mode="default"
    if [[ -n "${SHADOW_BIN_DIR:-}" && ":${PATH}:" == *":${SHADOW_BIN_DIR}:"* ]]; then
        path_mode="shadow_first"
    fi
    jq -cn \
        --arg eeBin "$(basename "${REAL_EE}")" \
        --arg pathMode "${path_mode}" \
        --arg eeE2eTmpdir "$([[ -n "${EE_E2E_TMPDIR:-}" ]] && echo set || echo unset)" \
        --arg tmpdir "$([[ -n "${TMPDIR:-}" ]] && echo set || echo unset)" \
        --arg cargoTarget "$([[ -n "${CARGO_TARGET_DIR:-}" ]] && echo set || echo unset)" \
        '{
          HOME: "[unchanged]",
          PATH: $pathMode,
          EE_BIN_BASENAME: $eeBin,
          EE_E2E_TMPDIR: $eeE2eTmpdir,
          TMPDIR: $tmpdir,
          CARGO_TARGET_DIR: $cargoTarget
        }'
}

log_note() {
    local label="$1" status="$2" detail="${3:-}"
    local sanitized_env
    sanitized_env="$(sanitized_env_json)"
    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg ts "$(now_iso)" \
        --arg testId "${TEST_ID}" \
        --arg label "${label}" \
        --arg status "${status}" \
        --arg detail "${detail}" \
        --arg workspace "${WORKSPACE}" \
        --arg artifactDir "${ROOT}" \
        --argjson sanitizedEnv "${sanitized_env}" \
        '{
          schema: $schema,
          ts: $ts,
          test_id: $testId,
          kind: "note",
          fields: {
            bead_id: "bd-1et0v.22",
            surface: "ergonomics_e2e",
            label: $label,
            status: $status,
            detail: $detail,
            workspace: $workspace,
            artifact_dir: $artifactDir,
            sanitized_env: $sanitizedEnv,
            redaction_status: "local_workspace_artifacts_retained"
          }
        }' >>"${EVENT_LOG}"
}

emit_command_start() {
    local label="$1"
    shift
    local args sanitized_env
    args="$(args_json "$@")"
    sanitized_env="$(sanitized_env_json)"
    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg ts "$(now_iso)" \
        --arg testId "${TEST_ID}" \
        --argjson args "${args}" \
        --arg label "${label}" \
        --arg workspace "${WORKSPACE}" \
        --argjson sanitizedEnv "${sanitized_env}" \
        '{
          schema: $schema,
          ts: $ts,
          test_id: $testId,
          kind: "command_start",
          command: "ee",
          args: $args,
          fields: {
            bead_id: "bd-1et0v.22",
            surface: "ergonomics_e2e",
            label: $label,
            cwd: "[REPO_ROOT]",
            workspace: $workspace,
            sanitized_env: $sanitizedEnv
          }
        }' >>"${EVENT_LOG}"
}

emit_command_end() {
    local label="$1" exit_code="$2" elapsed_ms="$3" stdout_file="$4" stderr_file="$5"
    local schema_validation_status="$6" redaction_status="$7" first_failure_diagnosis="$8"
    shift 8
    local args sanitized_env stdout_bytes stderr_bytes
    args="$(args_json "$@")"
    sanitized_env="$(sanitized_env_json)"
    stdout_bytes="$(wc -c <"${stdout_file}" | tr -d ' ')"
    stderr_bytes="$(wc -c <"${stderr_file}" | tr -d ' ')"
    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg ts "$(now_iso)" \
        --arg testId "${TEST_ID}" \
        --argjson args "${args}" \
        --arg label "${label}" \
        --argjson exitCode "${exit_code}" \
        --argjson elapsedMs "${elapsed_ms}" \
        --arg stdoutArtifact "${stdout_file}" \
        --arg stderrArtifact "${stderr_file}" \
        --arg schemaValidationStatus "${schema_validation_status}" \
        --arg redactionStatus "${redaction_status}" \
        --arg firstFailureDiagnosis "${first_failure_diagnosis}" \
        --argjson stdoutBytes "${stdout_bytes}" \
        --argjson stderrBytes "${stderr_bytes}" \
        --argjson sanitizedEnv "${sanitized_env}" \
        '{
          schema: $schema,
          ts: $ts,
          test_id: $testId,
          kind: "command_end",
          command: "ee",
          args: $args,
          exit_code: $exitCode,
          elapsed_ms: $elapsedMs,
          fields: {
            bead_id: "bd-1et0v.22",
            surface: "ergonomics_e2e",
            label: $label,
            cwd: "[REPO_ROOT]",
            workspace: "[WORKSPACE]",
            sanitized_env: $sanitizedEnv,
            stdout_artifact_path: $stdoutArtifact,
            stderr_artifact_path: $stderrArtifact,
            stdout_bytes: $stdoutBytes,
            stderr_bytes: $stderrBytes,
            schema_validation_status: $schemaValidationStatus,
            redaction_status: $redactionStatus,
            first_failure_diagnosis: $firstFailureDiagnosis,
            rch_status: "not_run_by_harness"
          }
        }' >>"${EVENT_LOG}"
}

emit_assert_result() {
    local kind="$1" label="$2" status="$3" detail="$4" stdout_file="${5:-}" stderr_file="${6:-}"
    local schema_validation_status="${7:-not_run}" first_failure_diagnosis="${8:-none}"
    local sanitized_env
    sanitized_env="$(sanitized_env_json)"
    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg ts "$(now_iso)" \
        --arg testId "${TEST_ID}" \
        --arg kind "${kind}" \
        --arg label "${label}" \
        --arg status "${status}" \
        --arg detail "${detail}" \
        --arg workspace "${WORKSPACE}" \
        --arg stdoutArtifact "${stdout_file}" \
        --arg stderrArtifact "${stderr_file}" \
        --arg schemaValidationStatus "${schema_validation_status}" \
        --arg redactionStatus "${REDACTION_STATUS}" \
        --arg firstFailureDiagnosis "${first_failure_diagnosis}" \
        --argjson sanitizedEnv "${sanitized_env}" \
        '{
          schema: $schema,
          ts: $ts,
          test_id: $testId,
          kind: $kind,
          fields: {
            bead_id: "bd-1et0v.22",
            surface: "ergonomics_e2e",
            label: $label,
            status: $status,
            detail: $detail,
            cwd: "[REPO_ROOT]",
            workspace: $workspace,
            sanitized_env: $sanitizedEnv,
            stdout_artifact_path: $stdoutArtifact,
            stderr_artifact_path: $stderrArtifact,
            schema_validation_status: $schemaValidationStatus,
            redaction_status: $redactionStatus,
            first_failure_diagnosis: $firstFailureDiagnosis
          }
        }' >>"${EVENT_LOG}"
}

fail() {
    local label="$1" detail="${2:-}"
    FAILURES=$((FAILURES + 1))
    emit_assert_result "assert_result" "${label}" "fail" "${detail}" "" "" "not_run" "${detail:-assertion failed}"
    printf '[ergonomics-e2e][FAIL] %s %s\n' "${label}" "${detail}" >&2
}

pass() {
    local label="$1" detail="${2:-}"
    emit_assert_result "assert_ok" "${label}" "pass" "${detail}" "" "" "not_run" "none"
    printf '[ergonomics-e2e][PASS] %s %s\n' "${label}" "${detail}" >&2
}

assert_json_file() {
    local label="$1" file="$2" filter="$3"
    if jq -e "${filter}" "${file}" >/dev/null 2>&1; then
        emit_assert_result "assert_ok" "${label}" "pass" "${filter}" "${file}" "" "passed" "none"
        printf '[ergonomics-e2e][PASS] %s %s\n' "${label}" "${filter}" >&2
    else
        FAILURES=$((FAILURES + 1))
        emit_assert_result "assert_result" "${label}" "fail" "filter failed: ${filter}" "${file}" "" "failed" "json assertion failed for ${label}"
        printf '[ergonomics-e2e][FAIL] %s filter failed: %s; file=%s\n' "${label}" "${filter}" "${file}" >&2
    fi
}

run_ee_json() {
    local label="$1"
    shift
    local stdout_file="${ROOT}/${label}.stdout.json"
    local stderr_file="${ROOT}/${label}.stderr.txt"
    local start elapsed rc
    start="$(now_ms)"
    emit_command_start "${label}" "$@"
    "${REAL_EE}" "$@" >"${stdout_file}" 2>"${stderr_file}"
    rc=$?
    elapsed=$(( $(now_ms) - start ))
    local schema_validation_status="failed"
    local first_failure_diagnosis="none"
    if jq -e 'type == "object" and (.schema? | type == "string")' "${stdout_file}" >/dev/null 2>&1; then
        schema_validation_status="passed"
    elif [[ "${rc}" -eq 0 ]]; then
        first_failure_diagnosis="stdout was not an ee JSON envelope"
    fi
    if [[ "${rc}" -eq 0 ]]; then
        emit_command_end "${label}" "${rc}" "${elapsed}" "${stdout_file}" "${stderr_file}" \
            "${schema_validation_status}" "${REDACTION_STATUS}" "${first_failure_diagnosis}" "$@"
    else
        first_failure_diagnosis="command exited non-zero"
        emit_command_end "${label}" "${rc}" "${elapsed}" "${stdout_file}" "${stderr_file}" \
            "${schema_validation_status}" "${REDACTION_STATUS}" "${first_failure_diagnosis}" "$@"
        FAILURES=$((FAILURES + 1))
    fi
    LAST_STDOUT="${stdout_file}"
    LAST_STDERR="${stderr_file}"
    LAST_RC="${rc}"
}

log_note "ergonomics_e2e" "start" "root=${ROOT}"

run_ee_json "00_init" --workspace "${WORKSPACE}" --json init
INIT_JSON="${LAST_STDOUT}"
assert_json_file "init_succeeds" "${INIT_JSON}" '.success == true'

run_ee_json "01_remember_alias_rule" --workspace "${WORKSPACE}" --json remember \
    --level procedural \
    --kind rule \
    --confidence 0.99 \
    --tags ergonomics,e2e \
    --no-auto-link \
    --no-propose-candidates \
    "Ergonomics alias parity fixture: use ee pack as the canonical context command."
REMEMBER_JSON="${LAST_STDOUT}"
assert_json_file "remember_succeeds" "${REMEMBER_JSON}" '.success == true'

run_ee_json "02_index_rebuild" --workspace "${WORKSPACE}" --json index rebuild
INDEX_JSON="${LAST_STDOUT}"
assert_json_file "index_rebuild_succeeds" "${INDEX_JSON}" '.success == true'

TASK="ergonomics alias parity canonical pack command"
EE_GLOBAL_ARGS=(
    --workspace "${WORKSPACE}"
    --json
)
PACK_COMMON=(
    --source-mode lexical_only
    --profile compact
    --max-tokens 2000
    --candidate-pool 20
    --read-only
    --no-baseline-write
)

run_ee_json "03_pack" "${EE_GLOBAL_ARGS[@]}" pack "${PACK_COMMON[@]}" "${TASK}"
PACK_JSON="${LAST_STDOUT}"
assert_json_file "pack_succeeds" "${PACK_JSON}" '.success == true and .data.pack.hash'
assert_json_file "pack_has_no_deprecated_alias" "${PACK_JSON}" \
    'all((.data.degraded // [])[]; .code != "deprecated_alias")'

run_ee_json "04_context_alias" "${EE_GLOBAL_ARGS[@]}" context "${PACK_COMMON[@]}" "${TASK}"
CONTEXT_JSON="${LAST_STDOUT}"
assert_json_file "context_succeeds" "${CONTEXT_JSON}" '.success == true and .data.pack.hash'
assert_json_file "context_reports_deprecated_alias" "${CONTEXT_JSON}" \
    'any((.data.degraded // [])[]; .code == "deprecated_alias" and .severity == "info")'
assert_json_file "context_names_pack_as_canonical" "${CONTEXT_JSON}" \
    'any((.data.degraded // [])[]; .code == "deprecated_alias" and ((.message // "") | contains("canonical `ee pack`")) and ((.repair // "") | contains("ee pack")))'

if jq -e --slurp '
    .[0].data.pack.hash == .[1].data.pack.hash
    and .[0].data.pack.items == .[1].data.pack.items
    and (.[0].data.pack.skipped // []) == (.[1].data.pack.skipped // [])
' "${PACK_JSON}" "${CONTEXT_JSON}" >/dev/null 2>&1; then
    pass "alias_pack_content_parity" "hash/items/skipped match"
else
    fail "alias_pack_content_parity" "pack=${PACK_JSON} context=${CONTEXT_JSON}"
fi

SHADOW_BIN_DIR="${ROOT}/shadow-bin"
REAL_BIN_DIR="$(dirname "${REAL_EE}")"
mkdir -p "${SHADOW_BIN_DIR}" "${ROOT}/install-dir"
cat >"${SHADOW_BIN_DIR}/ee" <<'EOS'
#!/usr/bin/env bash
case "${1:-}" in
  --version|version)
    echo "ee 0.5.0"
    ;;
  *)
    echo "shadow fixture should only be used for version probing" >&2
    echo '{"schema":"ee.test_event.v1","test_id":"ergonomics_e2e","kind":"assert_result","fields":{"label":"shadow_fixture_misuse","status":"fail","first_failure_diagnosis":"shadow ee fixture received a non-version command","stdout_artifact_path":"","stderr_artifact_path":"stderr","schema_validation_status":"not_run","redaction_status":"not_run"}}' >&2
    exit 42
    ;;
esac
EOS
chmod +x "${SHADOW_BIN_DIR}/ee"

DOCTOR_PATH="${SHADOW_BIN_DIR}:${REAL_BIN_DIR}:${PATH}"
run_ee_json "05_doctor_clean" --workspace "${WORKSPACE}" --json doctor
DOCTOR_CLEAN_JSON="${LAST_STDOUT}"
assert_json_file "doctor_clean_succeeds" "${DOCTOR_CLEAN_JSON}" '.success == true'

PATH="${DOCTOR_PATH}" run_ee_json "06_doctor_shadow" --workspace "${WORKSPACE}" --json doctor
local_path_status="${LAST_RC}"
DOCTOR_SHADOW_JSON="${ROOT}/06_doctor_shadow.stdout.json"
assert_json_file "doctor_shadow_succeeds" "${DOCTOR_SHADOW_JSON}" '.success == true'
assert_json_file "path_shadow_advisory_present" "${DOCTOR_SHADOW_JSON}" \
    'any(.data.checks[]?; .name == "ee_install_path" and .severity == "warning" and ((.message // "") | contains("current_binary_shadowed")) and ((.message // "") | contains("0.5.0")) and ((.message // "") | contains("no network lookup")))'
assert_json_file "path_shadow_repair_hint_present" "${DOCTOR_SHADOW_JSON}" \
    'any(.data.checks[]?; .name == "ee_install_path" and ((.repair // "") | contains("ee install check --json --offline")) and ((.repair // "") | contains("do not use local Cargo")))'

if jq -e --slurp '.[0].data.posture == .[1].data.posture and .[0].data.healthy == .[1].data.healthy' \
    "${DOCTOR_CLEAN_JSON}" "${DOCTOR_SHADOW_JSON}" >/dev/null 2>&1; then
    pass "path_shadow_does_not_change_top_line" "posture and healthy unchanged"
else
    fail "path_shadow_does_not_change_top_line" "clean=${DOCTOR_CLEAN_JSON} shadow=${DOCTOR_SHADOW_JSON}"
fi

if jq -e '.data.posture == "ok" or .data.posture == "ready" or .data.posture.overall == "ok" or .data.posture.overall == "ready"' \
    "${DOCTOR_CLEAN_JSON}" >/dev/null 2>&1; then
    assert_json_file "path_shadow_core_health_stays_green" "${DOCTOR_SHADOW_JSON}" \
        '.data.posture == "ok" or .data.posture == "ready" or .data.posture.overall == "ok" or .data.posture.overall == "ready"'
else
    log_note "path_shadow_core_health_stays_green" "skipped" \
        "clean doctor is not green yet; asserted non-degrading parity instead"
fi

assert_event_log_contract() {
    local label="event_log_contract_complete"
    local expected_commands=7
    if jq -s -e --arg testId "${TEST_ID}" --arg redactionStatus "${REDACTION_STATUS}" --argjson expectedCommands "${expected_commands}" '
        length > 0
        and all(.[]; .schema == "ee.test_event.v1" and .test_id == $testId and (.kind | type == "string"))
        and ([.[] | select(.kind == "command_start")] | length) == $expectedCommands
        and ([.[] | select(.kind == "command_end")] | length) == $expectedCommands
        and all(.[] | select(.kind == "command_start");
            .command == "ee"
            and (.args | type == "array")
            and (.fields.label | type == "string" and length > 0)
            and (.fields.sanitized_env | type == "object")
        )
        and all(.[] | select(.kind == "command_end");
            .command == "ee"
            and (.args | type == "array")
            and (.exit_code | type == "number")
            and (.elapsed_ms | type == "number")
            and (.fields.stdout_artifact_path | type == "string" and length > 0)
            and (.fields.stderr_artifact_path | type == "string" and length > 0)
            and ((.fields.schema_validation_status == "passed") or (.fields.schema_validation_status == "failed"))
            and .fields.redaction_status == $redactionStatus
            and (.fields.first_failure_diagnosis | type == "string" and length > 0)
            and .fields.rch_status == "not_run_by_harness"
            and (.fields.sanitized_env | type == "object")
        )
        and all(.[] | select(.kind == "assert_ok" or .kind == "assert_result");
            (.fields.label | type == "string" and length > 0)
            and ((.fields.status == "pass") or (.fields.status == "fail"))
            and (.fields.schema_validation_status | type == "string")
            and (.fields.redaction_status | type == "string")
            and (.fields.first_failure_diagnosis | type == "string" and length > 0)
        )
        and any(.[]; .kind == "assert_ok" and .fields.label == "alias_pack_content_parity")
        and any(.[]; .kind == "assert_ok" and .fields.label == "path_shadow_does_not_change_top_line")
    ' "${EVENT_LOG}" >/dev/null 2>&1; then
        pass "${label}" "event log has complete command/assert evidence"
    else
        fail "${label}" "event log missing required command/assert evidence; event_log=${EVENT_LOG}"
    fi
}

assert_event_log_contract

SUMMARY="${ROOT}/summary.json"
jq -cn \
    --arg schema "ee.test_event.v1.summary" \
    --arg beadId "bd-1et0v.22" \
    --arg surface "ergonomics_e2e" \
    --arg eventLog "${EVENT_LOG}" \
    --arg artifactDir "${ROOT}" \
    --argjson failures "${FAILURES}" \
    '{
      schema: $schema,
      beadId: $beadId,
      surface: $surface,
      eventLog: $eventLog,
      artifactDir: $artifactDir,
      failures: $failures,
      verdict: (if $failures == 0 then "PASS" else "FAIL" end)
    }' >"${SUMMARY}"

log_note "ergonomics_e2e" "$([[ "${FAILURES}" -eq 0 ]] && echo ok || echo fail)" "summary=${SUMMARY}"
printf 'ergonomics e2e artifacts: %s\n' "${ROOT}" >&2
cat "${SUMMARY}"

if [[ "${FAILURES}" -ne 0 ]]; then
    emit_assert_result "assert_result" "ergonomics_e2e_summary" "fail" \
        "stdout_artifact_path=${SUMMARY} stderr_artifact_path=${EVENT_LOG}" \
        "${SUMMARY}" "${EVENT_LOG}" "passed" "one or more ergonomics assertions failed"
    exit 1
fi
