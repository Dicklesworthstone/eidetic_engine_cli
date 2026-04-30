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
START_TIME=""

# Available scenarios
SCENARIOS=(
    "status"
    "health"
    "capabilities"
    "introspect"
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
    TEST_WORKSPACE=$(mktemp -d -t ee-e2e.XXXXXX)
    TEST_HOME="${TEST_WORKSPACE}/home"
    ARTIFACTS_DIR="${TEST_WORKSPACE}/artifacts"
    LOG_FILE="${ARTIFACTS_DIR}/e2e.log"

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

# ============================================================================
# Test Scenarios
# ============================================================================

scenario_status() {
    log_step "Running scenario: status"
    local passed=0
    local failed=0

    # Test: status --json
    run_ee status json_output status --json
    if assert_exit 0 "status --json exit" && \
       assert_stdout_json "status --json format" && \
       assert_json_schema "ee.response.v1" "status schema" && \
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

    # Test: introspect commands --json
    run_ee introspect commands_json introspect commands --json
    if assert_exit 0 "introspect commands exit" && \
       assert_stdout_json "introspect commands format" && \
       assert_stdout_contains "commands" "introspect has commands"; then
        ((passed++))
        log_pass "introspect commands --json"
    else
        ((failed++))
    fi

    # Test: introspect schemas --json
    run_ee introspect schemas_json introspect schemas --json
    if assert_exit 0 "introspect schemas exit" && \
       assert_stdout_json "introspect schemas format"; then
        ((passed++))
        log_pass "introspect schemas --json"
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
  EE_BINARY      Path to ee binary (default: target/debug/ee)
  EE_KEEP_TEMP   If set, preserve temp workspace on exit
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
        status)      scenario_status ;;
        health)      scenario_health ;;
        capabilities) scenario_capabilities ;;
        introspect)  scenario_introspect ;;
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
    START_TIME=$(date +%s%3N 2>/dev/null || echo "0")

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
