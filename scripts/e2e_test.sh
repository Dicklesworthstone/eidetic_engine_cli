#!/usr/bin/env bash
# EE-TST-004 Walking Skeleton End-to-End Test Script
#
# Validates the core EE workflow against the real binary in an isolated
# temporary workspace. Tests: init equivalents, status, health, capabilities,
# remember, search, context, why, and doctor commands.
#
# Usage:
#   ./scripts/e2e_test.sh              # Run all scenarios
#   ./scripts/e2e_test.sh status       # Run only status scenario
#   ./scripts/e2e_test.sh --list       # List available scenarios
#   ./scripts/e2e_test.sh --help       # Show this help
#
# Exit codes:
#   0 — all scenarios passed
#   1 — usage error or scenario not found
#   2 — one or more scenarios failed
#   3 — required tool missing or build failed
#
# Environment:
#   EE_BINARY      Override path to ee binary (default: Cargo target/debug/ee)
#   EE_KEEP_TEMP   Deprecated; temp workspace is always preserved
#   EE_VERBOSE     If set, show command output during tests

set -euo pipefail

# ============================================================================
# Configuration
# ============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
DEFAULT_AGENT_BUILD_ROOT="/Volumes/USBNVME16TB/temp_agent_space"

if [[ -d "${DEFAULT_AGENT_BUILD_ROOT}" ]]; then
    mkdir -p "${DEFAULT_AGENT_BUILD_ROOT}/cargo-target" "${DEFAULT_AGENT_BUILD_ROOT}/tmp" 2>/dev/null || true
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${DEFAULT_AGENT_BUILD_ROOT}/cargo-target}"
    export TMPDIR="${EE_AGENT_TMPDIR:-${DEFAULT_AGENT_BUILD_ROOT}/tmp}"
fi

# shellcheck source=scripts/lib/ee_binary_resolution.sh
source "${REPO_ROOT}/scripts/lib/ee_binary_resolution.sh"
EE_BINARY="$(ee_resolve_binary debug)"

# Test workspace (created fresh per run)
TEST_WORKSPACE=""
TEST_HOME=""
ARTIFACTS_DIR=""
LOG_FILE=""
EVENT_LOG_FILE=""

# Counters
TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0
START_TIME=""

# Available scenarios
SCENARIOS=(
    "walking_skeleton"
    "status"
    "health"
    "capabilities"
    "introspect"
    "lab_swarm_replay"
    "help"
)

# ============================================================================
# Utility Functions
# ============================================================================

log_info() {
    echo "[INFO] $*" >&2
}

log_step() {
    echo "[STEP] $*" >&2
}

log_pass() {
    echo "[PASS] $*" >&2
}

log_fail() {
    echo "[FAIL] $*" >&2
}

log_error() {
    echo "[ERROR] $*" >&2
}

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
    end=$(now_ms)
    echo $((end - start))
}

cleanup() {
    local exit_code=$?
    if [[ -n "${TEST_WORKSPACE}" && -d "${TEST_WORKSPACE}" ]]; then
        log_info "Preserved test workspace: ${TEST_WORKSPACE}"
    fi
    exit ${exit_code}
}

setup_workspace() {
    TEST_WORKSPACE=$(mktemp -d -t ee-e2e.XXXXXX)
    TEST_HOME="${TEST_WORKSPACE}/home"
    ARTIFACTS_DIR="${TEST_WORKSPACE}/artifacts"
    LOG_FILE="${ARTIFACTS_DIR}/e2e.log"
    EVENT_LOG_FILE="${ARTIFACTS_DIR}/e2e-events.jsonl"

    mkdir -p "${TEST_HOME}" "${ARTIFACTS_DIR}"

    # Create minimal workspace structure
    mkdir -p "${TEST_WORKSPACE}/ws/.ee"

    log_info "Test workspace: ${TEST_WORKSPACE}"
    log_info "Artifacts: ${ARTIFACTS_DIR}"

    # Initialize log file
    {
        echo "# EE E2E Test Log"
        echo "# Started: $(date -Iseconds)"
        echo "# Binary: ${EE_BINARY}"
        echo "# Workspace: ${TEST_WORKSPACE}"
        echo ""
    } > "${LOG_FILE}"
    : > "${EVENT_LOG_FILE}"
}

check_binary() {
    if [[ ! -x "${EE_BINARY}" ]]; then
        log_info "Binary not found at ${EE_BINARY}, attempting build..."
        if ! cargo build --manifest-path "${REPO_ROOT}/Cargo.toml" 2>&1; then
            log_error "Failed to build ee binary"
            exit 3
        fi
    fi

    if [[ ! -x "${EE_BINARY}" ]]; then
        log_error "ee binary not found at ${EE_BINARY}"
        exit 3
    fi

    log_info "Using binary: ${EE_BINARY}"
}

# ============================================================================
# Test Execution Framework
# ============================================================================

# Run a command and capture results
# Usage: run_ee <scenario> <step> <args...>
run_ee() {
    local scenario="$1"
    local step="$2"
    shift 2
    local cmd_args=("$@")

    local stdout_file="${ARTIFACTS_DIR}/${scenario}_${step}_stdout.txt"
    local stderr_file="${ARTIFACTS_DIR}/${scenario}_${step}_stderr.txt"
    local step_start
    step_start=$(now_ms)

    local exit_code=0

    # Run with isolated environment
    env \
        HOME="${TEST_HOME}" \
        EE_WORKSPACE="${TEST_WORKSPACE}/ws" \
        EE_EMBED_DOWNLOAD="${EE_EMBED_DOWNLOAD:-off}" \
        NO_COLOR=1 \
        "${EE_BINARY}" "${cmd_args[@]}" \
        >"${stdout_file}" 2>"${stderr_file}" || exit_code=$?

    local elapsed
    elapsed=$(elapsed_ms "${step_start}")

    # Log result
    {
        echo "## ${scenario}/${step}"
        echo "Command: ee ${cmd_args[*]}"
        echo "Exit code: ${exit_code}"
        echo "Elapsed: ${elapsed}ms"
        echo "Stdout: ${stdout_file}"
        echo "Stderr: ${stderr_file}"
        echo ""
    } >> "${LOG_FILE}"

    if [[ -n "${EE_VERBOSE:-}" ]]; then
        log_step "ee ${cmd_args[*]} -> exit ${exit_code} (${elapsed}ms)"
    fi

    # Return values via globals (bash limitation)
    LAST_EXIT_CODE="${exit_code}"
    LAST_STDOUT_FILE="${stdout_file}"
    LAST_STDERR_FILE="${stderr_file}"
    LAST_ELAPSED="${elapsed}"
}

# Assert exit code
assert_exit() {
    local expected="$1"
    local context="${2:-}"

    if [[ "${LAST_EXIT_CODE}" -ne "${expected}" ]]; then
        log_fail "${context}: expected exit ${expected}, got ${LAST_EXIT_CODE}"
        return 1
    fi
    return 0
}

# Assert stdout contains string
assert_stdout_contains() {
    local needle="$1"
    local context="${2:-}"

    if ! grep -q "${needle}" "${LAST_STDOUT_FILE}"; then
        log_fail "${context}: stdout missing '${needle}'"
        return 1
    fi
    return 0
}

# Assert stdout is valid JSON
assert_stdout_json() {
    local context="${1:-}"

    if ! python3 -m json.tool "${LAST_STDOUT_FILE}" >/dev/null 2>&1; then
        log_fail "${context}: stdout is not valid JSON"
        return 1
    fi
    return 0
}

# Assert stdout has JSON schema field
assert_json_schema() {
    local expected_schema="$1"
    local context="${2:-}"

    local actual
    actual=$(python3 -c "import json; print(json.load(open('${LAST_STDOUT_FILE}')).get('schema', ''))" 2>/dev/null || echo "")

    if [[ "${actual}" != "${expected_schema}" ]]; then
        log_fail "${context}: expected schema '${expected_schema}', got '${actual}'"
        return 1
    fi
    return 0
}

# Assert stderr is empty (no diagnostic leakage to stdout)
assert_stdout_clean() {
    local context="${1:-}"

    # Check stdout doesn't contain common diagnostic patterns
    if grep -qE '^\[INFO\]|^\[WARN\]|^\[ERROR\]|^warning:|^error:' "${LAST_STDOUT_FILE}"; then
        log_fail "${context}: stdout contains diagnostic content"
        return 1
    fi
    return 0
}

assert_stderr_empty() {
    local context="${1:-}"

    if [[ -s "${LAST_STDERR_FILE}" ]]; then
        log_fail "${context}: stderr must be empty on success"
        return 1
    fi
    return 0
}

validate_response_command() {
    local response_file="$1"
    local expected_command="$2"
    python3 - "${response_file}" "${expected_command}" <<'PY'
import json
import sys

value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["schema"] == "ee.response.v2"
assert value["success"] is True
assert isinstance(value["data"], dict)
assert value["data"]["command"] == sys.argv[2]
PY
}

response_memory_id() {
    local response_file="$1"
    python3 - "${response_file}" <<'PY'
import json
import sys

value = json.load(open(sys.argv[1], encoding="utf-8"))
memory_id = value["data"].get("memory_id") or value["data"].get("memoryId")
assert isinstance(memory_id, str) and memory_id.startswith("mem_")
print(memory_id)
PY
}

validate_remember_persisted() {
    local response_file="$1"
    local workspace="$2"
    validate_response_command "${response_file}" remember
    response_memory_id "${response_file}" >/dev/null
    [[ -s "${workspace}/.ee/ee.db" ]]
}

validate_search_hit() {
    local response_file="$1"
    local memory_id="$2"
    local expected_content="$3"
    python3 - "${response_file}" "${memory_id}" "${expected_content}" <<'PY'
import json
import sys

value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["schema"] == "ee.response.v2"
assert value["success"] is True
hits = value["data"]["results"]
matching = [
    hit for hit in hits
    if (hit.get("memoryId") or hit.get("memory_id") or hit.get("docId")) == sys.argv[2]
]
assert len(matching) == 1
assert matching[0]["content"] == sys.argv[3]
assert matching[0]["source"] == "lexical"
PY
}

validate_pack_item() {
    local response_file="$1"
    local memory_id="$2"
    local expected_content="$3"
    python3 - "${response_file}" "${memory_id}" "${expected_content}" <<'PY'
import json
import sys

value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["schema"] == "ee.response.v2"
assert value["success"] is True
pack = value["data"]["pack"]
assert pack["schema"] == "ee.pack.v2"
assert isinstance(pack["hash"], str) and pack["hash"].startswith("blake3:")
matching = [item for item in pack["items"] if item.get("memoryId") == sys.argv[2]]
assert len(matching) == 1
assert matching[0]["content"] == sys.argv[3]
assert isinstance(matching[0]["provenance"], list) and matching[0]["provenance"]
PY
}

validate_why_explanation() {
    local response_file="$1"
    local memory_id="$2"
    python3 - "${response_file}" "${memory_id}" <<'PY'
import json
import sys

value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["schema"] == "ee.response.v2"
assert value["success"] is True
data = value["data"]
assert all(isinstance(data[field], dict) for field in ("storage", "retrieval", "selection"))
assert data["selection"].get("latestPackSelection") is not None
memory = data.get("memory") or {}
explained_id = memory.get("id") or memory.get("memoryId") or data.get("memoryId")
assert explained_id in (None, sys.argv[2])
PY
}

validate_status_capabilities() {
    local response_file="$1"
    python3 - "${response_file}" <<'PY'
import json
import sys

value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["schema"] == "ee.response.v2"
assert value["success"] is True
assert value["data"]["command"] == "status"
capabilities = value["data"]["capabilities"]
assert all(field in capabilities for field in ("runtime", "storage", "search"))
PY
}

emit_test_event() {
    local scenario="$1"
    local phase="$2"
    local kind="$3"
    local command_label="$4"
    local schema_validation_status="$5"
    local redaction_status="$6"
    local first_failure_diagnosis="$7"

    EVENT_LOG_FILE="${EVENT_LOG_FILE}" \
    TEST_WORKSPACE="${TEST_WORKSPACE}" \
    ARTIFACTS_DIR="${ARTIFACTS_DIR}" \
    SCENARIO="${scenario}" \
    PHASE="${phase}" \
    KIND="${kind}" \
    COMMAND_LABEL="${command_label}" \
    EXIT_CODE="${LAST_EXIT_CODE:-0}" \
    ELAPSED_MS="${LAST_ELAPSED:-0}" \
    STDOUT_FILE="${LAST_STDOUT_FILE:-}" \
    STDERR_FILE="${LAST_STDERR_FILE:-}" \
    SCHEMA_VALIDATION_STATUS="${schema_validation_status}" \
    REDACTION_STATUS="${redaction_status}" \
    FIRST_FAILURE_DIAGNOSIS="${first_failure_diagnosis}" \
    python3 - <<'PY'
import hashlib
import json
import os
from pathlib import Path


def sha256_text(value: str) -> str:
    return "sha256:" + hashlib.sha256(value.encode("utf-8")).hexdigest()


def sha256_file(path: str):
    if not path:
        return None
    file_path = Path(path)
    if not file_path.exists():
        return None
    digest = hashlib.sha256()
    with file_path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(65536), b""):
            digest.update(chunk)
    return "sha256:" + digest.hexdigest()


def artifact_tail(path: str):
    if not path:
        return None
    file_path = Path(path)
    artifacts = Path(os.environ["ARTIFACTS_DIR"])
    try:
        return str(file_path.relative_to(artifacts))
    except ValueError:
        return file_path.name


workspace = os.environ["TEST_WORKSPACE"]
event = {
    "schema": "ee.test_event.v1",
    "surface": "basic_e2e",
    "scenario": os.environ["SCENARIO"],
    "phase": os.environ["PHASE"],
    "kind": os.environ["KIND"],
    "command": os.environ["COMMAND_LABEL"],
    "cwdHash": sha256_text(os.getcwd()),
    "workspaceHash": sha256_text(workspace),
    "sanitizedEnv": {
        "HOME": sha256_text(str(Path(workspace) / "home")),
        "EE_WORKSPACE": sha256_text(str(Path(workspace) / "ws")),
        "NO_COLOR": "1",
    },
    "elapsedMs": int(os.environ["ELAPSED_MS"]),
    "exitCode": int(os.environ["EXIT_CODE"]),
    "stdoutArtifactPath": artifact_tail(os.environ["STDOUT_FILE"]),
    "stderrArtifactPath": artifact_tail(os.environ["STDERR_FILE"]),
    "stdoutArtifactHash": sha256_file(os.environ["STDOUT_FILE"]),
    "stderrArtifactHash": sha256_file(os.environ["STDERR_FILE"]),
    "schemaValidationStatus": os.environ["SCHEMA_VALIDATION_STATUS"],
    "redactionStatus": os.environ["REDACTION_STATUS"],
    "firstFailureDiagnosis": os.environ["FIRST_FAILURE_DIAGNOSIS"] or None,
}
with Path(os.environ["EVENT_LOG_FILE"]).open("a", encoding="utf-8") as handle:
    handle.write(json.dumps(event, sort_keys=True, separators=(",", ":")) + "\n")
PY
}

run_walking_step() {
    local step="$1"
    local command_label="$2"
    local validator="$3"
    shift 3
    local validator_args=()
    while [[ $# -gt 0 && "$1" != "--" ]]; do
        validator_args+=("$1")
        shift
    done
    if [[ $# -eq 0 ]]; then
        log_fail "walking skeleton ${step}: missing command separator"
        return 1
    fi
    shift

    run_ee walking_skeleton "${step}" "$@"
    if assert_exit 0 "walking skeleton ${step} exit" && \
       assert_stdout_json "walking skeleton ${step} JSON" && \
       "${validator}" "${LAST_STDOUT_FILE}" "${validator_args[@]}" && \
       assert_stdout_clean "walking skeleton ${step} stdout" && \
       assert_stderr_empty "walking skeleton ${step} stderr"; then
        emit_test_event walking_skeleton "${step}" command_end \
            "${command_label}" passed passed ""
        log_pass "walking skeleton ${step}"
        return 0
    fi

    emit_test_event walking_skeleton "${step}" command_end \
        "${command_label}" failed not_checked "${step}_contract_failed"
    return 1
}

validate_swarm_workload_trace() {
    local trace_file="$1"
    python3 - "${trace_file}" <<'PY'
import json
import sys

value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["schema"] == "ee.swarm_workload.v1"
assert value["sideEffectFree"] is True
assert value["fixtureSeed"] == "stable_small_replay_smoke_001"
assert value["resourceProfileHints"]["profile"] == "ci_smoke"
assert len(value["commandSequence"]) > 0
assert value["provenance"]["kind"] == "synthetic"
PY
}

validate_swarm_replay_result() {
    local replay_file="$1"
    python3 - "${replay_file}" <<'PY'
import json
import sys

value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["schema"] == "ee.swarm_replay_result.v1"
assert value["sideEffectFree"] is True
assert value["status"] in {"degraded", "blocked", "pass"}
assert value["verification"]["rchRequired"] is True
assert value["verification"]["rchStatus"] == "blocked_before_cargo"
aggregate = value["aggregate"]
command_count = aggregate["commandCount"]
assert command_count > 0
slo_total = (
    aggregate["sloPassCount"]
    + aggregate["sloWarningCount"]
    + aggregate["sloFailureCount"]
    + aggregate["sloExemptCount"]
)
assert slo_total == command_count
redaction = value["redactionStatus"]
for field in [
    "rawTaskStringPresent",
    "rawQueryTextPresent",
    "rawMemoryBodyPresent",
    "rawMailBodyPresent",
    "absoluteHostPathPresent",
    "secretsPresent",
    "environmentDumpPresent",
    "fullFileListingPresent",
]:
    assert redaction[field] is False, field
assert redaction["redactionProbesPassed"] is True
assert "swarm_replay_rch_proof_missing" in json.dumps(value["warnings"])
PY
}

# ============================================================================
# Test Scenarios
# ============================================================================

scenario_walking_skeleton() {
    log_step "Running scenario: walking_skeleton"
    local passed=0
    local failed=0
    local memory_id=""
    local content="Run cargo fmt --check before release."
    local workspace="${TEST_WORKSPACE}/ws"

    if run_walking_step init \
        "ee init --workspace <workspace> --skip-boilerplate --json" \
        validate_response_command init -- \
        init --workspace "${workspace}" --skip-boilerplate --json; then
        ((passed++))
    else
        ((failed++))
    fi

    if run_walking_step remember \
        "ee remember <content> --workspace <workspace> --level procedural --kind rule --json" \
        validate_remember_persisted "${workspace}" -- \
        remember "${content}" --workspace "${workspace}" \
        --level procedural --kind rule --tags release,format --confidence 0.95 \
        --no-auto-link --no-propose-candidates --json; then
        memory_id="$(response_memory_id "${LAST_STDOUT_FILE}")"
        ((passed++))
    else
        ((failed++))
    fi

    if run_walking_step index_rebuild \
        "ee index rebuild --workspace <workspace> --json" \
        validate_response_command index_rebuild -- \
        index rebuild --workspace "${workspace}" --json; then
        ((passed++))
    else
        ((failed++))
    fi

    if run_walking_step search \
        "ee search <query> --workspace <workspace> --source-mode lexical_only --json" \
        validate_search_hit "${memory_id:-mem_missing}" "${content}" -- \
        search "format before release" --workspace "${workspace}" \
        --source-mode lexical_only --relevance-floor 0.0 --json; then
        ((passed++))
    else
        ((failed++))
    fi

    if run_walking_step pack \
        "ee pack <task> --workspace <workspace> --source-mode lexical_only --json" \
        validate_pack_item "${memory_id:-mem_missing}" "${content}" -- \
        pack "prepare release" --workspace "${workspace}" \
        --source-mode lexical_only --max-tokens 2000 --json; then
        ((passed++))
    else
        ((failed++))
    fi

    if run_walking_step why \
        "ee why <memory-id> --workspace <workspace> --json" \
        validate_why_explanation "${memory_id:-mem_missing}" -- \
        why "${memory_id:-mem_missing}" --workspace "${workspace}" --json; then
        ((passed++))
    else
        ((failed++))
    fi

    if run_walking_step status \
        "ee status --workspace <workspace> --json" \
        validate_status_capabilities -- \
        status --workspace "${workspace}" --json; then
        ((passed++))
    else
        ((failed++))
    fi

    TESTS_RUN=$((TESTS_RUN + passed + failed))
    TESTS_PASSED=$((TESTS_PASSED + passed))
    TESTS_FAILED=$((TESTS_FAILED + failed))

    [[ ${failed} -eq 0 ]]
}

scenario_status() {
    log_step "Running scenario: status"
    local passed=0
    local failed=0

    # Test: status --json
    run_ee status json_output status --json
    if assert_exit 0 "status --json exit" && \
       assert_stdout_json "status --json format" && \
       assert_json_schema "ee.response.v2" "status schema" && \
       assert_stdout_clean "status stdout clean"; then
        ((passed++))
        log_pass "status --json"
    else
        ((failed++))
    fi

    # Test: status (human output)
    run_ee status human_output status
    if assert_exit 0 "status human exit"; then
        ((passed++))
        log_pass "status (human)"
    else
        ((failed++))
    fi

    TESTS_RUN=$((TESTS_RUN + passed + failed))
    TESTS_PASSED=$((TESTS_PASSED + passed))
    TESTS_FAILED=$((TESTS_FAILED + failed))

    [[ ${failed} -eq 0 ]]
}

scenario_health() {
    log_step "Running scenario: health"
    local passed=0
    local failed=0

    # Test: health --json
    run_ee health json_output health --json
    if assert_exit 0 "health --json exit" && \
       assert_stdout_json "health --json format" && \
       assert_stdout_clean "health stdout clean"; then
        ((passed++))
        log_pass "health --json"
    else
        ((failed++))
    fi

    TESTS_RUN=$((TESTS_RUN + passed + failed))
    TESTS_PASSED=$((TESTS_PASSED + passed))
    TESTS_FAILED=$((TESTS_FAILED + failed))

    [[ ${failed} -eq 0 ]]
}

scenario_capabilities() {
    log_step "Running scenario: capabilities"
    local passed=0
    local failed=0

    # Test: capabilities --json
    run_ee capabilities json_output capabilities --json
    if assert_exit 0 "capabilities --json exit" && \
       assert_stdout_json "capabilities --json format" && \
       assert_stdout_clean "capabilities stdout clean"; then
        ((passed++))
        log_pass "capabilities --json"
    else
        ((failed++))
    fi

    TESTS_RUN=$((TESTS_RUN + passed + failed))
    TESTS_PASSED=$((TESTS_PASSED + passed))
    TESTS_FAILED=$((TESTS_FAILED + failed))

    [[ ${failed} -eq 0 ]]
}

scenario_introspect() {
    log_step "Running scenario: introspect"
    local passed=0
    local failed=0

    # Test: introspect --json exposes command inventory
    run_ee introspect commands_json introspect --json
    if assert_exit 0 "introspect commands exit" && \
       assert_stdout_json "introspect commands format" && \
       assert_stdout_contains "commands" "introspect has commands"; then
        ((passed++))
        log_pass "introspect command inventory"
    else
        ((failed++))
    fi

    # Test: introspect --json exposes schema inventory
    run_ee introspect schemas_json introspect --json
    if assert_exit 0 "introspect schemas exit" && \
       assert_stdout_json "introspect schemas format" && \
       assert_stdout_contains "schemas" "introspect has schemas"; then
        ((passed++))
        log_pass "introspect schema inventory"
    else
        ((failed++))
    fi

    TESTS_RUN=$((TESTS_RUN + passed + failed))
    TESTS_PASSED=$((TESTS_PASSED + passed))
    TESTS_FAILED=$((TESTS_FAILED + failed))

    [[ ${failed} -eq 0 ]]
}

scenario_help() {
    log_step "Running scenario: help"
    local passed=0
    local failed=0

    # Test: --help
    run_ee help help_flag --help
    if assert_exit 0 "ee --help exit" && \
       assert_stdout_contains "Usage" "help has Usage"; then
        ((passed++))
        log_pass "ee --help"
    else
        ((failed++))
    fi

    # Test: help subcommand
    run_ee help help_cmd help
    if assert_exit 0 "ee help exit"; then
        ((passed++))
        log_pass "ee help"
    else
        ((failed++))
    fi

    TESTS_RUN=$((TESTS_RUN + passed + failed))
    TESTS_PASSED=$((TESTS_PASSED + passed))
    TESTS_FAILED=$((TESTS_FAILED + failed))

    [[ ${failed} -eq 0 ]]
}

scenario_lab_swarm_replay() {
    log_step "Running scenario: lab_swarm_replay"
    local passed=0
    local failed=0
    local trace_file="${ARTIFACTS_DIR}/lab_swarm_replay_trace.json"

    run_ee lab_swarm_replay generate_trace \
        --workspace "${TEST_WORKSPACE}/ws" \
        --json \
        lab generate-workload \
        --fixture-seed stable_small_replay_smoke_001 \
        --profile small
    if assert_exit 0 "lab generate-workload exit" && \
       assert_stdout_json "lab generate-workload JSON" && \
       validate_swarm_workload_trace "${LAST_STDOUT_FILE}"; then
        cp "${LAST_STDOUT_FILE}" "${trace_file}"
        emit_test_event \
            lab_swarm_replay \
            generate_trace \
            command_end \
            "ee lab generate-workload --fixture-seed <seed> --profile small" \
            passed \
            passed \
            ""
        ((passed++))
        log_pass "lab generate-workload smoke trace"
    else
        emit_test_event \
            lab_swarm_replay \
            generate_trace \
            command_end \
            "ee lab generate-workload --fixture-seed <seed> --profile small" \
            failed \
            not_checked \
            "generate_workload_schema_validation_failed"
        ((failed++))
    fi

    if [[ -f "${trace_file}" ]]; then
        run_ee lab_swarm_replay replay_dry_run \
            --workspace "${TEST_WORKSPACE}/ws" \
            --json \
            lab swarm replay \
            --trace "${trace_file}" \
            --dry-run
        if assert_exit 6 "lab swarm replay missing-proof degraded exit" && \
           assert_stdout_json "lab swarm replay JSON" && \
           validate_swarm_replay_result "${LAST_STDOUT_FILE}"; then
            emit_test_event \
                lab_swarm_replay \
                replay_dry_run \
                command_end \
                "ee lab swarm replay --trace <trace> --dry-run" \
                passed \
                passed \
                ""
            ((passed++))
            log_pass "lab swarm replay dry-run ledger"
        else
            emit_test_event \
                lab_swarm_replay \
                replay_dry_run \
                command_end \
                "ee lab swarm replay --trace <trace> --dry-run" \
                failed \
                not_checked \
                "swarm_replay_dry_run_validation_failed"
            ((failed++))
        fi
    else
        log_fail "lab_swarm_replay: missing generated trace artifact"
        ((failed++))
    fi

    if [[ -s "${EVENT_LOG_FILE}" ]] && \
       python3 - "${EVENT_LOG_FILE}" <<'PY'
import json
import sys

events = [
    json.loads(line)
    for line in open(sys.argv[1], encoding="utf-8")
    if line.strip()
]
scenario_events = [event for event in events if event.get("scenario") == "lab_swarm_replay"]
assert len(scenario_events) >= 2
for event in scenario_events:
    assert event["schema"] == "ee.test_event.v1"
    assert event["command"].startswith("ee lab ")
    assert event["cwdHash"].startswith("sha256:")
    assert event["workspaceHash"].startswith("sha256:")
    assert event["sanitizedEnv"]["HOME"].startswith("sha256:")
    assert event["sanitizedEnv"]["EE_WORKSPACE"].startswith("sha256:")
    assert event["stdoutArtifactPath"]
    assert event["stderrArtifactPath"]
    assert event["schemaValidationStatus"] == "passed"
    assert event["redactionStatus"] == "passed"
PY
    then
        ((passed++))
        log_pass "lab swarm replay ee.test_event.v1 log"
    else
        log_fail "lab_swarm_replay: event log validation failed"
        ((failed++))
    fi

    TESTS_RUN=$((TESTS_RUN + passed + failed))
    TESTS_PASSED=$((TESTS_PASSED + passed))
    TESTS_FAILED=$((TESTS_FAILED + failed))

    [[ ${failed} -eq 0 ]]
}

# ============================================================================
# Main Entry Point
# ============================================================================

show_help() {
    cat <<EOF
EE Walking Skeleton E2E Test Script

Usage:
  $0              Run all scenarios
  $0 <scenario>   Run specific scenario
  $0 --list       List available scenarios
  $0 --help       Show this help

Available scenarios:
$(printf '  %s\n' "${SCENARIOS[@]}")

Environment variables:
  EE_BINARY      Path to ee binary (default: Cargo target/debug/ee)
  EE_KEEP_TEMP   Deprecated; temp workspace is always preserved
  EE_VERBOSE     If set, show command output during tests

Exit codes:
  0  All scenarios passed
  1  Usage error or scenario not found
  2  One or more scenarios failed
  3  Required tool missing or build failed
EOF
}

list_scenarios() {
    echo "Available scenarios:"
    printf '  %s\n' "${SCENARIOS[@]}"
}

run_scenario() {
    local name="$1"

    case "${name}" in
        walking_skeleton) scenario_walking_skeleton ;;
        status)      scenario_status ;;
        health)      scenario_health ;;
        capabilities) scenario_capabilities ;;
        introspect)  scenario_introspect ;;
        lab_swarm_replay) scenario_lab_swarm_replay ;;
        help)        scenario_help ;;
        *)
            log_error "Unknown scenario: ${name}"
            log_error "Use --list to see available scenarios"
            exit 1
            ;;
    esac
}

main() {
    trap cleanup EXIT

    # Parse arguments
    local scenarios_to_run=()

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --help|-h)
                show_help
                exit 0
                ;;
            --list|-l)
                list_scenarios
                exit 0
                ;;
            *)
                scenarios_to_run+=("$1")
                shift
                ;;
        esac
    done

    # Default to all scenarios if none specified
    if [[ ${#scenarios_to_run[@]} -eq 0 ]]; then
        scenarios_to_run=("${SCENARIOS[@]}")
    fi

    # Check prerequisites
    if ! command -v python3 >/dev/null 2>&1; then
        log_error "python3 is required for JSON validation"
        exit 3
    fi

    # Setup
    check_binary
    setup_workspace
    START_TIME=$(now_ms)

    log_info "Running ${#scenarios_to_run[@]} scenario(s): ${scenarios_to_run[*]}"
    echo ""

    # Run scenarios
    local failed_scenarios=()
    for scenario in "${scenarios_to_run[@]}"; do
        if ! run_scenario "${scenario}"; then
            failed_scenarios+=("${scenario}")
        fi
        echo ""
    done

    # Summary
    local total_elapsed
    total_elapsed=$(elapsed_ms "${START_TIME}")

    echo "============================================"
    echo "E2E Test Summary"
    echo "============================================"
    echo "Total tests:  ${TESTS_RUN}"
    echo "Passed:       ${TESTS_PASSED}"
    echo "Failed:       ${TESTS_FAILED}"
    echo "Elapsed:      ${total_elapsed}ms"
    echo "Artifacts:    ${ARTIFACTS_DIR}"
    echo "Log:          ${LOG_FILE}"
    echo "Event log:    ${EVENT_LOG_FILE}"

    if [[ ${#failed_scenarios[@]} -gt 0 ]]; then
        echo ""
        echo "Failed scenarios: ${failed_scenarios[*]}"
        exit 2
    fi

    echo ""
    log_pass "All scenarios passed!"
    exit 0
}

main "$@"
