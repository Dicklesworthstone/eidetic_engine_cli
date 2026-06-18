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

now_ms() {
    python3 -c 'import time; print(int(time.time() * 1000))'
}

log_event() {
    local phase="$1" status="$2" detail="${3:-}" command="${4:-}"
    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg beadId "bd-1et0v.22" \
        --arg surface "ergonomics_e2e" \
        --arg phase "${phase}" \
        --arg status "${status}" \
        --arg detail "${detail}" \
        --arg command "${command}" \
        --arg workspace "${WORKSPACE}" \
        --arg artifactDir "${ROOT}" \
        '{
          schema: $schema,
          beadId: $beadId,
          surface: $surface,
          phase: $phase,
          status: $status,
          detail: $detail,
          command: $command,
          workspace: $workspace,
          artifactDir: $artifactDir,
          ts: (now | todateiso8601)
        }' >>"${EVENT_LOG}"
}

fail() {
    local label="$1" detail="${2:-}"
    FAILURES=$((FAILURES + 1))
    log_event "${label}" "fail" "${detail}"
    printf '[ergonomics-e2e][FAIL] %s %s\n' "${label}" "${detail}" >&2
}

pass() {
    local label="$1" detail="${2:-}"
    log_event "${label}" "pass" "${detail}"
    printf '[ergonomics-e2e][PASS] %s %s\n' "${label}" "${detail}" >&2
}

assert_json_file() {
    local label="$1" file="$2" filter="$3"
    if jq -e "${filter}" "${file}" >/dev/null 2>&1; then
        pass "${label}" "${filter}"
    else
        fail "${label}" "filter failed: ${filter}; file=${file}"
    fi
}

run_ee_json() {
    local label="$1"
    shift
    local stdout_file="${ROOT}/${label}.stdout.json"
    local stderr_file="${ROOT}/${label}.stderr.txt"
    local start elapsed rc
    start="$(now_ms)"
    log_event "${label}" "start" "$*" "ee $*"
    "${REAL_EE}" "$@" >"${stdout_file}" 2>"${stderr_file}"
    rc=$?
    elapsed=$(( $(now_ms) - start ))
    if [[ "${rc}" -eq 0 ]]; then
        log_event "${label}" "ok" "rc=${rc} elapsed_ms=${elapsed}" "ee $*"
    else
        log_event "${label}" "fail" "rc=${rc} elapsed_ms=${elapsed} stderr=${stderr_file}" "ee $*"
        FAILURES=$((FAILURES + 1))
    fi
    LAST_STDOUT="${stdout_file}"
    LAST_STDERR="${stderr_file}"
    LAST_RC="${rc}"
}

log_event "ergonomics_e2e" "start" "root=${ROOT}"

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
PACK_COMMON=(
    --workspace "${WORKSPACE}"
    --json
    --source-mode lexical_only
    --profile compact
    --max-tokens 2000
    --candidate-pool 20
    --read-only
    --no-baseline-write
)

run_ee_json "03_pack" "${PACK_COMMON[@]}" pack "${TASK}"
PACK_JSON="${LAST_STDOUT}"
assert_json_file "pack_succeeds" "${PACK_JSON}" '.success == true and .data.pack.hash'
assert_json_file "pack_has_no_deprecated_alias" "${PACK_JSON}" \
    'all((.data.degraded // [])[]; .code != "deprecated_alias")'

run_ee_json "04_context_alias" "${PACK_COMMON[@]}" context "${TASK}"
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
    exit 42
    ;;
esac
EOS
chmod +x "${SHADOW_BIN_DIR}/ee"

DOCTOR_PATH="${SHADOW_BIN_DIR}:${REAL_BIN_DIR}:${PATH}"
run_ee_json "05_doctor_clean" --workspace "${WORKSPACE}" --json doctor
DOCTOR_CLEAN_JSON="${LAST_STDOUT}"
assert_json_file "doctor_clean_succeeds" "${DOCTOR_CLEAN_JSON}" '.success == true'

local_path_status="$(PATH="${DOCTOR_PATH}" "${REAL_EE}" --workspace "${WORKSPACE}" --json doctor >"${ROOT}/06_doctor_shadow.stdout.json" 2>"${ROOT}/06_doctor_shadow.stderr.txt"; echo "$?")"
log_event "06_doctor_shadow" "$([[ "${local_path_status}" == "0" ]] && echo ok || echo fail)" \
    "rc=${local_path_status}" "PATH=<shadow>:<real> ee doctor --json"
DOCTOR_SHADOW_JSON="${ROOT}/06_doctor_shadow.stdout.json"
if [[ "${local_path_status}" != "0" ]]; then
    FAILURES=$((FAILURES + 1))
fi
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
    log_event "path_shadow_core_health_stays_green" "skipped" \
        "clean doctor is not green yet; asserted non-degrading parity instead"
fi

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

log_event "ergonomics_e2e" "$([[ "${FAILURES}" -eq 0 ]] && echo ok || echo fail)" "summary=${SUMMARY}"
printf 'ergonomics e2e artifacts: %s\n' "${ROOT}" >&2
cat "${SUMMARY}"

if [[ "${FAILURES}" -ne 0 ]]; then
    exit 1
fi
