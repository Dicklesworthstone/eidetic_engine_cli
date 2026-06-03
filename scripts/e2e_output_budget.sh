#!/usr/bin/env bash
# bd-kua65 output-budget E2E driver.
#
# Measures default/summary/full JSON output for the status and swarm brief
# surfaces and emits a compact ee.e2e.output_budget.v1 artifact. This script
# intentionally never builds the binary; callers must provide EE_BINARY or run
# it after a build/test gate has produced the debug binary.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
DEFAULT_AGENT_BUILD_ROOT="/Volumes/USBNVME16TB/temp_agent_space"

if [[ -d "${DEFAULT_AGENT_BUILD_ROOT}" ]]; then
    mkdir -p "${DEFAULT_AGENT_BUILD_ROOT}/cargo-target" "${DEFAULT_AGENT_BUILD_ROOT}/tmp" 2>/dev/null || true
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${DEFAULT_AGENT_BUILD_ROOT}/cargo-target}"
    export TMPDIR="${EE_AGENT_TMPDIR:-${DEFAULT_AGENT_BUILD_ROOT}/tmp}"
fi

# shellcheck source=scripts/lib/ee_binary_resolution.sh
# shellcheck disable=SC1091
source "${REPO_ROOT}/scripts/lib/ee_binary_resolution.sh"

if ! command -v jq >/dev/null 2>&1; then
    echo "output_budget: jq is required" >&2
    exit 3
fi

EE_BINARY="$(ee_resolve_binary debug)"
if [[ ! -x "${EE_BINARY}" ]]; then
    echo "output_budget: ee binary not found or not executable: ${EE_BINARY}" >&2
    echo "output_budget: build via the normal RCH-backed gate first or set EE_BINARY" >&2
    exit 3
fi

STATUS_DEFAULT_MAX_BYTES="${EE_OUTPUT_BUDGET_STATUS_DEFAULT_MAX_BYTES:-8192}"
SWARM_DEFAULT_MAX_BYTES="${EE_OUTPUT_BUDGET_SWARM_DEFAULT_MAX_BYTES:-32768}"

ARTIFACTS_DIR="${EE_OUTPUT_BUDGET_ARTIFACT_DIR:-}"
if [[ -z "${ARTIFACTS_DIR}" ]]; then
    ARTIFACTS_DIR="$(mktemp -d -t ee-output-budget.XXXXXX)"
else
    mkdir -p "${ARTIFACTS_DIR}"
fi

RESULTS_JSONL="${ARTIFACTS_DIR}/results.jsonl"
ASSERTIONS_JSONL="${ARTIFACTS_DIR}/assertions.jsonl"
SUMMARY_JSON="${ARTIFACTS_DIR}/output_budget_summary.json"
: >"${RESULTS_JSONL}"
: >"${ASSERTIONS_JSONL}"

FAILURES=0
LAST_STDOUT_FILE=""

now_ms() {
    local value
    value=$(date +%s%3N 2>/dev/null || true)
    if [[ "${value}" =~ ^[0-9]+$ ]]; then
        echo "${value}"
    else
        echo "$(date +%s)000"
    fi
}

elapsed_ms() {
    local start="$1"
    local end
    end="$(now_ms)"
    echo $((end - start))
}

sha256_text() {
    printf '%s' "$1" | shasum -a 256 | awk '{print "sha256:" $1}'
}

log_pass() {
    echo "[PASS] $*" >&2
}

log_fail() {
    echo "[FAIL] $*" >&2
    FAILURES=$((FAILURES + 1))
}

record_assertion() {
    local name="$1"
    local status="$2"
    local detail="$3"
    jq -nc \
        --arg name "${name}" \
        --arg status "${status}" \
        --arg detail "${detail}" \
        '{name:$name,status:$status,detail:$detail}' >>"${ASSERTIONS_JSONL}"
}

command_text() {
    local text="ee"
    local arg
    for arg in "$@"; do
        text="${text} $(printf '%q' "${arg}")"
    done
    printf '%s\n' "${text}"
}

run_case() {
    local surface="$1"
    local preset="$2"
    local placement="$3"
    local max_bytes="$4"
    shift 4

    local name="${surface}_${preset}_${placement}"
    local stdout_file="${ARTIFACTS_DIR}/${name}.stdout.json"
    local stderr_file="${ARTIFACTS_DIR}/${name}.stderr.txt"
    local start
    local elapsed
    local exit_code=0
    local byte_count=0
    local valid_json=false
    local fields_value=""
    local section_list="[]"
    local within_budget=true
    local cmd_text

    cmd_text="$(command_text "$@")"
    start="$(now_ms)"
    env NO_COLOR=1 "${EE_BINARY}" "$@" >"${stdout_file}" 2>"${stderr_file}" || exit_code=$?
    elapsed="$(elapsed_ms "${start}")"
    byte_count="$(wc -c <"${stdout_file}" | tr -d '[:space:]')"

    if [[ "${exit_code}" -eq 0 ]] &&
        jq -e '.schema == "ee.response.v2" and .success == true and (.data | type == "object")' \
            "${stdout_file}" >/dev/null 2>>"${stderr_file}"; then
        valid_json=true
        fields_value="$(jq -r '.fields // "default"' "${stdout_file}")"
        section_list="$(jq -c '(.data // {}) | keys | sort' "${stdout_file}")"
    else
        log_fail "${name}: command failed or did not emit ee.response.v2 JSON"
        record_assertion "${name}_json" "fail" "exit=${exit_code}"
    fi

    if [[ "${max_bytes}" -gt 0 && "${byte_count}" -gt "${max_bytes}" ]]; then
        within_budget=false
        log_fail "${name}: ${byte_count} bytes exceeds budget ${max_bytes}"
        record_assertion "${name}_budget" "fail" "${byte_count} > ${max_bytes}"
    elif [[ "${max_bytes}" -gt 0 ]]; then
        log_pass "${name}: ${byte_count} bytes <= ${max_bytes}"
        record_assertion "${name}_budget" "pass" "${byte_count} <= ${max_bytes}"
    fi

    jq -nc \
        --arg surface "${surface}" \
        --arg preset "${preset}" \
        --arg placement "${placement}" \
        --arg command "${cmd_text}" \
        --arg stdoutPath "${stdout_file}" \
        --arg stderrPath "${stderr_file}" \
        --arg fields "${fields_value}" \
        --argjson exitCode "${exit_code}" \
        --argjson elapsedMs "${elapsed}" \
        --argjson byteCount "${byte_count}" \
        --argjson maxBytes "${max_bytes}" \
        --argjson validJson "${valid_json}" \
        --argjson withinBudget "${within_budget}" \
        --argjson sectionList "${section_list}" \
        '{
          surface:$surface,
          preset:$preset,
          placement:$placement,
          command:$command,
          exitCode:$exitCode,
          elapsedMs:$elapsedMs,
          byteCount:$byteCount,
          maxBytes:$maxBytes,
          validJson:$validJson,
          withinBudget:$withinBudget,
          fields:$fields,
          sectionList:$sectionList,
          stdoutPath:$stdoutPath,
          stderrPath:$stderrPath
        }' >>"${RESULTS_JSONL}"

    echo "[INFO] ${name}: exit=${exit_code} bytes=${byte_count} elapsed=${elapsed}ms sections=${section_list}" >&2

    LAST_STDOUT_FILE="${stdout_file}"
}

assert_fields_indicator() {
    local file="$1"
    local expected="$2"
    local label="$3"

    if jq -e --arg expected "${expected}" '.fields == $expected' "${file}" >/dev/null; then
        log_pass "${label}: fields=${expected}"
        record_assertion "${label}_fields" "pass" "fields=${expected}"
    else
        log_fail "${label}: expected fields=${expected}"
        record_assertion "${label}_fields" "fail" "expected fields=${expected}"
    fi
}

assert_sections() {
    local file="$1"
    local label="$2"
    shift 2

    local section
    for section in "$@"; do
        if jq -e --arg section "${section}" '(.data // {}) | has($section)' "${file}" >/dev/null; then
            log_pass "${label}: section ${section} present"
            record_assertion "${label}_section_${section}" "pass" "present"
        else
            log_fail "${label}: section ${section} missing"
            record_assertion "${label}_section_${section}" "fail" "missing"
        fi
    done
}

assert_sections_absent() {
    local file="$1"
    local label="$2"
    shift 2

    local section
    for section in "$@"; do
        if jq -e --arg section "${section}" '(.data // {}) | has($section) | not' "${file}" >/dev/null; then
            log_pass "${label}: section ${section} absent"
            record_assertion "${label}_section_${section}_absent" "pass" "absent"
        else
            log_fail "${label}: section ${section} unexpectedly present"
            record_assertion "${label}_section_${section}_absent" "fail" "present"
        fi
    done
}

assert_same_shape() {
    local left="$1"
    local right="$2"
    local label="$3"
    local left_shape="${ARTIFACTS_DIR}/${label}.left.shape.json"
    local right_shape="${ARTIFACTS_DIR}/${label}.right.shape.json"

    jq -S '{schema,success,fields,dataKeys:((.data // {}) | keys | sort)}' "${left}" >"${left_shape}"
    jq -S '{schema,success,fields,dataKeys:((.data // {}) | keys | sort)}' "${right}" >"${right_shape}"

    if diff -u "${left_shape}" "${right_shape}" >/dev/null; then
        log_pass "${label}: pre/post --fields placement has matching canonical shape"
        record_assertion "${label}_placement_shape" "pass" "canonical shape matches"
    else
        log_fail "${label}: pre/post --fields placement canonical shape differs"
        record_assertion "${label}_placement_shape" "fail" "canonical shape differs"
        diff -u "${left_shape}" "${right_shape}" >&2 || true
    fi
}

assert_same_bytes() {
    local left="$1"
    local right="$2"
    local label="$3"
    local left_bytes
    local right_bytes
    left_bytes="$(wc -c <"${left}" | tr -d '[:space:]')"
    right_bytes="$(wc -c <"${right}" | tr -d '[:space:]')"

    if cmp -s "${left}" "${right}"; then
        log_pass "${label}: pre/post --fields placement bytes match (${left_bytes})"
        record_assertion "${label}_placement_bytes" "pass" "${left_bytes} == ${right_bytes}"
    else
        log_fail "${label}: pre/post --fields placement bytes differ (${left_bytes} != ${right_bytes})"
        record_assertion "${label}_placement_bytes" "fail" "${left_bytes} != ${right_bytes}"
        diff -u "${left}" "${right}" >&2 || true
    fi
}

finish_summary() {
    local passed=false
    if [[ "${FAILURES}" -eq 0 ]]; then
        passed=true
    fi

    jq -n \
        --arg generatedAt "$(date -Iseconds)" \
        --arg workspaceHash "$(sha256_text "${REPO_ROOT}")" \
        --arg binaryPath "${EE_BINARY}" \
        --arg binaryHash "$(shasum -a 256 "${EE_BINARY}" | awk '{print "sha256:" $1}')" \
        --argjson statusDefaultMaxBytes "${STATUS_DEFAULT_MAX_BYTES}" \
        --argjson swarmDefaultMaxBytes "${SWARM_DEFAULT_MAX_BYTES}" \
        --argjson passed "${passed}" \
        --argjson failureCount "${FAILURES}" \
        --slurpfile results "${RESULTS_JSONL}" \
        --slurpfile assertions "${ASSERTIONS_JSONL}" \
        '{
          schema:"ee.e2e.output_budget.v1",
          generatedAt:$generatedAt,
          workspaceHash:$workspaceHash,
          binary:{path:$binaryPath, hash:$binaryHash},
          thresholds:{
            statusDefaultMaxBytes:$statusDefaultMaxBytes,
            swarmDefaultMaxBytes:$swarmDefaultMaxBytes
          },
          passed:$passed,
          failureCount:$failureCount,
          results:$results,
          assertions:$assertions
        }' >"${SUMMARY_JSON}"

    echo "Artifact: ${SUMMARY_JSON}"
    echo "Artifacts: ${ARTIFACTS_DIR}"
}

echo "[INFO] Using binary: ${EE_BINARY}" >&2
echo "[INFO] Artifacts: ${ARTIFACTS_DIR}" >&2

run_case status default default "${STATUS_DEFAULT_MAX_BYTES}" \
    status --workspace "${REPO_ROOT}" --json
STATUS_DEFAULT_STDOUT="${LAST_STDOUT_FILE}"

run_case status summary global 0 \
    --fields summary --workspace "${REPO_ROOT}" --json status
STATUS_SUMMARY_GLOBAL_STDOUT="${LAST_STDOUT_FILE}"

run_case status summary post 0 \
    status --fields summary --workspace "${REPO_ROOT}" --json
STATUS_SUMMARY_POST_STDOUT="${LAST_STDOUT_FILE}"

run_case status full post 0 \
    status --fields full --workspace "${REPO_ROOT}" --json
STATUS_FULL_STDOUT="${LAST_STDOUT_FILE}"

run_case swarm_brief default default "${SWARM_DEFAULT_MAX_BYTES}" \
    swarm brief --workspace "${REPO_ROOT}" --json
SWARM_DEFAULT_STDOUT="${LAST_STDOUT_FILE}"

run_case swarm_brief summary global 0 \
    --fields summary swarm brief --workspace "${REPO_ROOT}" --json
SWARM_SUMMARY_GLOBAL_STDOUT="${LAST_STDOUT_FILE}"

run_case swarm_brief summary post 0 \
    swarm brief --fields summary --workspace "${REPO_ROOT}" --json
SWARM_SUMMARY_POST_STDOUT="${LAST_STDOUT_FILE}"

run_case swarm_brief full post 0 \
    swarm brief --fields full --workspace "${REPO_ROOT}" --json
SWARM_FULL_STDOUT="${LAST_STDOUT_FILE}"

assert_fields_indicator "${STATUS_DEFAULT_STDOUT}" "summary" "status_default"

assert_same_shape "${STATUS_SUMMARY_GLOBAL_STDOUT}" "${STATUS_SUMMARY_POST_STDOUT}" "status_summary"
assert_same_shape "${SWARM_SUMMARY_GLOBAL_STDOUT}" "${SWARM_SUMMARY_POST_STDOUT}" "swarm_brief_summary"
assert_same_bytes "${STATUS_SUMMARY_GLOBAL_STDOUT}" "${STATUS_SUMMARY_POST_STDOUT}" "status_summary"
assert_same_bytes "${SWARM_SUMMARY_GLOBAL_STDOUT}" "${SWARM_SUMMARY_POST_STDOUT}" "swarm_brief_summary"

assert_sections "${STATUS_DEFAULT_STDOUT}" "status_default" command version workspace posture
assert_sections "${STATUS_FULL_STDOUT}" "status_full" command version workspace posture runtime search derivedAssets
assert_sections "${SWARM_DEFAULT_STDOUT}" "swarm_brief_default" schema workspace redactionStatus recommendations
assert_sections_absent "${SWARM_DEFAULT_STDOUT}" "swarm_brief_default" topRecommendations
assert_sections "${SWARM_FULL_STDOUT}" "swarm_brief_full" schema workspace redactionStatus recommendations

finish_summary

if [[ "${FAILURES}" -ne 0 ]]; then
    echo "[FAIL] output budget e2e failed with ${FAILURES} failure(s)" >&2
    exit 2
fi

echo "[PASS] output budget e2e passed"
