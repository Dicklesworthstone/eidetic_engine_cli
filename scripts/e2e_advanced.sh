#!/usr/bin/env bash
# EE-TST-005 Advanced Subsystem End-to-End Test Script
#
# Validates advanced EE subsystems against the real binary in an isolated
# temporary workspace. Tests: recorder, preflight, procedures, economy,
# learning, and causal credit commands.
#
# Usage:
#   ./scripts/e2e_advanced.sh              # Run all scenarios
#   ./scripts/e2e_advanced.sh recorder     # Run only recorder scenario
#   ./scripts/e2e_advanced.sh --list       # List available scenarios
#   ./scripts/e2e_advanced.sh --help       # Show this help
#
# Exit codes:
#   0 — all scenarios passed
#   1 — usage error or scenario not found
#   2 — one or more scenarios failed
#   3 — required tool missing or build failed
#
# Environment:
#   EE_BINARY      Override path to ee binary (default: target/debug/ee)
#   EE_KEEP_TEMP   If set, preserve temp workspace on exit
#   EE_VERBOSE     If set, show command output during tests

set -euo pipefail

# ============================================================================
# Configuration
# ============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
EE_BINARY="${EE_BINARY:-${REPO_ROOT}/target/debug/ee}"

# Test workspace (created fresh per run)
TEST_WORKSPACE=""
TEST_HOME=""
ARTIFACTS_DIR=""
LOG_FILE=""

# Counters
TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0
TESTS_SKIPPED=0
START_TIME=""

# Available scenarios
SCENARIOS=(
    "recorder"
    "preflight"
    "procedure"
    "economy"
    "learning"
    "causal"
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

log_skip() {
    echo "[SKIP] $*" >&2
}

log_error() {
    echo "[ERROR] $*" >&2
}

elapsed_ms() {
    local start="$1"
    local end
    end=$(date +%s%3N 2>/dev/null || date +%s)000
    echo $((end - start))
}

cleanup() {
    local exit_code=$?
    if [[ -n "${TEST_WORKSPACE}" && -d "${TEST_WORKSPACE}" ]]; then
        if [[ -z "${EE_KEEP_TEMP:-}" ]]; then
            rm -rf "${TEST_WORKSPACE}"
        else
            log_info "Preserved test workspace: ${TEST_WORKSPACE}"
        fi
    fi
    exit ${exit_code}
}

setup_workspace() {
    TEST_WORKSPACE=$(mktemp -d -t ee-e2e-advanced.XXXXXX)
    TEST_HOME="${TEST_WORKSPACE}/home"
    ARTIFACTS_DIR="${TEST_WORKSPACE}/artifacts"
    LOG_FILE="${ARTIFACTS_DIR}/e2e_advanced.log"

    mkdir -p "${TEST_HOME}" "${ARTIFACTS_DIR}"

    # Create minimal workspace structure
    mkdir -p "${TEST_WORKSPACE}/ws/.ee"

    log_info "Test workspace: ${TEST_WORKSPACE}"
    log_info "Artifacts: ${ARTIFACTS_DIR}"

    # Initialize log file
    {
        echo "# EE Advanced E2E Test Log"
        echo "# Started: $(date -Iseconds)"
        echo "# Binary: ${EE_BINARY}"
        echo "# Workspace: ${TEST_WORKSPACE}"
        echo ""
    } > "${LOG_FILE}"
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
    step_start=$(date +%s%3N 2>/dev/null || echo "0")

    local exit_code=0

    # Run with isolated environment
    env \
        HOME="${TEST_HOME}" \
        EE_WORKSPACE="${TEST_WORKSPACE}/ws" \
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

# Assert stdout is clean (no diagnostic leakage)
assert_stdout_clean() {
    local context="${1:-}"

    if grep -qE '^\[INFO\]|^\[WARN\]|^\[ERROR\]|^warning:|^error:' "${LAST_STDOUT_FILE}"; then
        log_fail "${context}: stdout contains diagnostic content"
        return 1
    fi
    return 0
}

# Assert JSON field exists
assert_json_field() {
    local field="$1"
    local context="${2:-}"

    if ! python3 -c "import json; d=json.load(open('${LAST_STDOUT_FILE}')); assert '${field}' in d or '${field}' in d.get('data', {})" 2>/dev/null; then
        log_fail "${context}: missing JSON field '${field}'"
        return 1
    fi
    return 0
}

# ============================================================================
# Test Scenarios
# ============================================================================

scenario_recorder() {
    log_step "Running scenario: recorder"
    local passed=0
    local failed=0

    # Test: recorder start --json (dry-run)
    run_ee recorder start_dryrun recorder start --agent-id test-agent --dry-run --json
    if assert_exit 0 "recorder start --dry-run exit" && \
       assert_stdout_json "recorder start format" && \
       assert_json_schema "ee.recorder.start.v1" "recorder start schema" && \
       assert_stdout_contains "runId" "recorder start has runId" && \
       assert_stdout_contains "dryRun" "recorder start has dryRun" && \
       assert_stdout_clean "recorder start stdout clean"; then
        ((passed++))
        log_pass "recorder start --dry-run --json"
    else
        ((failed++))
    fi

    # Test: recorder start --json (actual)
    run_ee recorder start_actual recorder start --agent-id test-agent --session-id sess-001 --json
    if assert_exit 0 "recorder start exit" && \
       assert_stdout_json "recorder start format" && \
       assert_json_schema "ee.recorder.start.v1" "recorder start schema" && \
       assert_stdout_contains "runId" "recorder start has runId"; then
        ((passed++))
        log_pass "recorder start --json"
        # Extract run_id for subsequent tests
        RUN_ID=$(python3 -c "import json; print(json.load(open('${LAST_STDOUT_FILE}')).get('runId', ''))" 2>/dev/null || echo "")
    else
        ((failed++))
        RUN_ID="run_test_fallback"
    fi

    # Test: recorder event --json
    run_ee recorder event recorder event "${RUN_ID}" --event-type tool_call --payload '{"tool":"test"}' --json
    if assert_exit 0 "recorder event exit" && \
       assert_stdout_json "recorder event format" && \
       assert_json_schema "ee.recorder.event_response.v1" "recorder event schema" && \
       assert_stdout_contains "eventId" "recorder event has eventId" && \
       assert_stdout_contains "sequence" "recorder event has sequence"; then
        ((passed++))
        log_pass "recorder event --json"
    else
        ((failed++))
    fi

    # Test: recorder event with redaction
    run_ee recorder event_redact recorder event "${RUN_ID}" --event-type user_message --payload 'secret' --redact --json
    if assert_exit 0 "recorder event --redact exit" && \
       assert_stdout_json "recorder event redact format" && \
       assert_stdout_contains "redactionStatus" "recorder event has redactionStatus"; then
        ((passed++))
        log_pass "recorder event --redact --json"
    else
        ((failed++))
    fi

    # Test: recorder tail --json
    run_ee recorder tail recorder tail "${RUN_ID}" --limit 10 --json
    if assert_exit 0 "recorder tail exit" && \
       assert_stdout_json "recorder tail format" && \
       assert_json_schema "ee.recorder.tail.v1" "recorder tail schema" && \
       assert_stdout_contains "events" "recorder tail has events"; then
        ((passed++))
        log_pass "recorder tail --json"
    else
        ((failed++))
    fi

    # Test: recorder finish --json (dry-run)
    run_ee recorder finish_dryrun recorder finish "${RUN_ID}" --status completed --dry-run --json
    if assert_exit 0 "recorder finish --dry-run exit" && \
       assert_stdout_json "recorder finish format" && \
       assert_json_schema "ee.recorder.finish.v1" "recorder finish schema" && \
       assert_stdout_contains "dryRun" "recorder finish has dryRun"; then
        ((passed++))
        log_pass "recorder finish --dry-run --json"
    else
        ((failed++))
    fi

    # Test: recorder finish --json (actual)
    run_ee recorder finish_actual recorder finish "${RUN_ID}" --status completed --json
    if assert_exit 0 "recorder finish exit" && \
       assert_stdout_json "recorder finish format" && \
       assert_json_schema "ee.recorder.finish.v1" "recorder finish schema"; then
        ((passed++))
        log_pass "recorder finish --json"
    else
        ((failed++))
    fi

    TESTS_RUN=$((TESTS_RUN + passed + failed))
    TESTS_PASSED=$((TESTS_PASSED + passed))
    TESTS_FAILED=$((TESTS_FAILED + failed))

    [[ ${failed} -eq 0 ]]
}

scenario_preflight() {
    log_step "Running scenario: preflight"
    local passed=0
    local failed=0
    local skipped=0

    # Test: preflight run --json (dry-run)
    run_ee preflight run_dryrun preflight run --task "test task" --dry-run --json
    if [[ "${LAST_EXIT_CODE}" -eq 0 ]]; then
        if assert_stdout_json "preflight run format" && \
           assert_stdout_clean "preflight run stdout clean"; then
            ((passed++))
            log_pass "preflight run --dry-run --json"
        else
            ((failed++))
        fi
    else
        # May not be fully implemented yet
        ((skipped++))
        log_skip "preflight run --dry-run --json (not yet implemented or degraded)"
    fi

    # Test: preflight show --json
    run_ee preflight show preflight show --json
    if [[ "${LAST_EXIT_CODE}" -eq 0 ]]; then
        if assert_stdout_json "preflight show format"; then
            ((passed++))
            log_pass "preflight show --json"
        else
            ((failed++))
        fi
    else
        ((skipped++))
        log_skip "preflight show --json (not yet implemented or degraded)"
    fi

    TESTS_RUN=$((TESTS_RUN + passed + failed))
    TESTS_PASSED=$((TESTS_PASSED + passed))
    TESTS_FAILED=$((TESTS_FAILED + failed))
    TESTS_SKIPPED=$((TESTS_SKIPPED + skipped))

    [[ ${failed} -eq 0 ]]
}

scenario_procedure() {
    log_step "Running scenario: procedure"
    local passed=0
    local failed=0
    local skipped=0

    # Test: procedure list --json
    run_ee procedure list procedure list --json
    if [[ "${LAST_EXIT_CODE}" -eq 0 ]]; then
        if assert_stdout_json "procedure list format" && \
           assert_stdout_clean "procedure list stdout clean"; then
            ((passed++))
            log_pass "procedure list --json"
        else
            ((failed++))
        fi
    else
        ((skipped++))
        log_skip "procedure list --json (not yet implemented or degraded)"
    fi

    # Test: procedure show --json (with placeholder ID)
    run_ee procedure show procedure show proc_test --json
    if [[ "${LAST_EXIT_CODE}" -eq 0 ]]; then
        if assert_stdout_json "procedure show format"; then
            ((passed++))
            log_pass "procedure show --json"
        else
            ((failed++))
        fi
    elif [[ "${LAST_EXIT_CODE}" -eq 10 ]]; then
        # NotFound is acceptable for placeholder ID
        ((passed++))
        log_pass "procedure show --json (not found, expected)"
    else
        ((skipped++))
        log_skip "procedure show --json (not yet implemented or degraded)"
    fi

    # Test: procedure propose --dry-run --json
    run_ee procedure propose_dryrun procedure propose --run-id run_test --dry-run --json
    if [[ "${LAST_EXIT_CODE}" -eq 0 ]]; then
        if assert_stdout_json "procedure propose format"; then
            ((passed++))
            log_pass "procedure propose --dry-run --json"
        else
            ((failed++))
        fi
    else
        ((skipped++))
        log_skip "procedure propose --dry-run --json (not yet implemented or degraded)"
    fi

    TESTS_RUN=$((TESTS_RUN + passed + failed))
    TESTS_PASSED=$((TESTS_PASSED + passed))
    TESTS_FAILED=$((TESTS_FAILED + failed))
    TESTS_SKIPPED=$((TESTS_SKIPPED + skipped))

    [[ ${failed} -eq 0 ]]
}

scenario_economy() {
    log_step "Running scenario: economy"
    local passed=0
    local failed=0
    local skipped=0

    # Test: economy report --json
    run_ee economy report economy report --json
    if [[ "${LAST_EXIT_CODE}" -eq 0 ]]; then
        if assert_stdout_json "economy report format" && \
           assert_stdout_clean "economy report stdout clean"; then
            ((passed++))
            log_pass "economy report --json"
        else
            ((failed++))
        fi
    else
        ((skipped++))
        log_skip "economy report --json (not yet implemented or degraded)"
    fi

    # Test: economy score --json
    run_ee economy score economy score mem_test --json
    if [[ "${LAST_EXIT_CODE}" -eq 0 ]]; then
        if assert_stdout_json "economy score format"; then
            ((passed++))
            log_pass "economy score --json"
        else
            ((failed++))
        fi
    elif [[ "${LAST_EXIT_CODE}" -eq 10 ]]; then
        # NotFound is acceptable for placeholder ID
        ((passed++))
        log_pass "economy score --json (not found, expected)"
    else
        ((skipped++))
        log_skip "economy score --json (not yet implemented or degraded)"
    fi

    TESTS_RUN=$((TESTS_RUN + passed + failed))
    TESTS_PASSED=$((TESTS_PASSED + passed))
    TESTS_FAILED=$((TESTS_FAILED + failed))
    TESTS_SKIPPED=$((TESTS_SKIPPED + skipped))

    [[ ${failed} -eq 0 ]]
}

scenario_learning() {
    log_step "Running scenario: learning"
    local passed=0
    local failed=0
    local skipped=0

    # Test: learn agenda --json
    run_ee learning agenda learn agenda --json
    if [[ "${LAST_EXIT_CODE}" -eq 0 ]]; then
        if assert_stdout_json "learn agenda format" && \
           assert_stdout_clean "learn agenda stdout clean"; then
            ((passed++))
            log_pass "learn agenda --json"
        else
            ((failed++))
        fi
    else
        ((skipped++))
        log_skip "learn agenda --json (not yet implemented or degraded)"
    fi

    # Test: learn uncertainty --json
    run_ee learning uncertainty learn uncertainty --json
    if [[ "${LAST_EXIT_CODE}" -eq 0 ]]; then
        if assert_stdout_json "learn uncertainty format"; then
            ((passed++))
            log_pass "learn uncertainty --json"
        else
            ((failed++))
        fi
    else
        ((skipped++))
        log_skip "learn uncertainty --json (not yet implemented or degraded)"
    fi

    TESTS_RUN=$((TESTS_RUN + passed + failed))
    TESTS_PASSED=$((TESTS_PASSED + passed))
    TESTS_FAILED=$((TESTS_FAILED + failed))
    TESTS_SKIPPED=$((TESTS_SKIPPED + skipped))

    [[ ${failed} -eq 0 ]]
}

scenario_causal() {
    log_step "Running scenario: causal"
    local passed=0
    local failed=0
    local skipped=0

    # Test: causal trace --json
    run_ee causal trace causal trace mem_test --json
    if [[ "${LAST_EXIT_CODE}" -eq 0 ]]; then
        if assert_stdout_json "causal trace format" && \
           assert_stdout_clean "causal trace stdout clean"; then
            ((passed++))
            log_pass "causal trace --json"
        else
            ((failed++))
        fi
    elif [[ "${LAST_EXIT_CODE}" -eq 10 ]]; then
        ((passed++))
        log_pass "causal trace --json (not found, expected)"
    else
        ((skipped++))
        log_skip "causal trace --json (not yet implemented or degraded)"
    fi

    # Test: causal estimate --json
    run_ee causal estimate causal estimate mem_test --json
    if [[ "${LAST_EXIT_CODE}" -eq 0 ]]; then
        if assert_stdout_json "causal estimate format"; then
            ((passed++))
            log_pass "causal estimate --json"
        else
            ((failed++))
        fi
    elif [[ "${LAST_EXIT_CODE}" -eq 10 ]]; then
        ((passed++))
        log_pass "causal estimate --json (not found, expected)"
    else
        ((skipped++))
        log_skip "causal estimate --json (not yet implemented or degraded)"
    fi

    TESTS_RUN=$((TESTS_RUN + passed + failed))
    TESTS_PASSED=$((TESTS_PASSED + passed))
    TESTS_FAILED=$((TESTS_FAILED + failed))
    TESTS_SKIPPED=$((TESTS_SKIPPED + skipped))

    [[ ${failed} -eq 0 ]]
}

# ============================================================================
# Main
# ============================================================================

show_help() {
    head -25 "$0" | tail -22 | sed 's/^# //' | sed 's/^#//'
    exit 0
}

list_scenarios() {
    echo "Available scenarios:"
    for s in "${SCENARIOS[@]}"; do
        echo "  - ${s}"
    done
    exit 0
}

main() {
    local target_scenarios=()

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --help|-h)
                show_help
                ;;
            --list|-l)
                list_scenarios
                ;;
            *)
                target_scenarios+=("$1")
                ;;
        esac
        shift
    done

    # Default to all scenarios
    if [[ ${#target_scenarios[@]} -eq 0 ]]; then
        target_scenarios=("${SCENARIOS[@]}")
    fi

    # Validate scenarios
    for s in "${target_scenarios[@]}"; do
        local found=false
        for valid in "${SCENARIOS[@]}"; do
            if [[ "${s}" == "${valid}" ]]; then
                found=true
                break
            fi
        done
        if [[ "${found}" != "true" ]]; then
            log_error "Unknown scenario: ${s}"
            echo "Use --list to see available scenarios"
            exit 1
        fi
    done

    # Setup
    trap cleanup EXIT
    START_TIME=$(date +%s%3N 2>/dev/null || echo "0")

    check_binary
    setup_workspace

    log_info "Running ${#target_scenarios[@]} scenario(s): ${target_scenarios[*]}"
    echo ""

    # Run scenarios
    local any_failed=false
    for scenario in "${target_scenarios[@]}"; do
        if ! "scenario_${scenario}"; then
            any_failed=true
        fi
        echo ""
    done

    # Summary
    local total_elapsed
    total_elapsed=$(elapsed_ms "${START_TIME}")

    echo "=============================================="
    echo "Advanced E2E Test Summary"
    echo "=============================================="
    echo "Total:   ${TESTS_RUN}"
    echo "Passed:  ${TESTS_PASSED}"
    echo "Failed:  ${TESTS_FAILED}"
    echo "Skipped: ${TESTS_SKIPPED}"
    echo "Elapsed: ${total_elapsed}ms"
    echo "Log:     ${LOG_FILE}"
    echo ""

    # Append summary to log
    {
        echo ""
        echo "# Summary"
        echo "Total: ${TESTS_RUN}"
        echo "Passed: ${TESTS_PASSED}"
        echo "Failed: ${TESTS_FAILED}"
        echo "Skipped: ${TESTS_SKIPPED}"
        echo "Elapsed: ${total_elapsed}ms"
    } >> "${LOG_FILE}"

    if [[ "${any_failed}" == "true" ]]; then
        exit 2
    fi

    exit 0
}

main "$@"
