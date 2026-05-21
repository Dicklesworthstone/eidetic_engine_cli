#!/usr/bin/env bash
# Shared helper library for F1-F5 agent-ergonomics e2e scripts.
#
# Source this from scripts/e2e_lib/e2e_*.sh. The caller must provide
# WORKSPACE so tests are explicit about the state they mutate.

set -euo pipefail

if [ -z "${WORKSPACE:-}" ]; then
    echo "agent_ergonomics_lib: WORKSPACE is required" >&2
    exit 2
fi

AGENT_ERGONOMICS_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$AGENT_ERGONOMICS_LIB_DIR/../.." && pwd)"
export REPO_ROOT

# shellcheck source=/dev/null
source "$REPO_ROOT/scripts/lib/e2e_logger.sh"

EE_BIN="${EE_BIN:-ee}"
TEST_NAME="${TEST_NAME:-$(basename "${BASH_SOURCE[1]:-${0}}" .sh)}"
STEP=0
PASS=0
FAIL=0
FAILURES=()
LOG_DIR="${LOG_DIR:-${WORKSPACE%/}/agent_ergonomics_logs/${TEST_NAME}.${BASHPID:-$$}}"
export EE_BIN TEST_NAME STEP PASS FAIL LOG_DIR

mkdir -p "$LOG_DIR"
EE_TEST_LOG_PATH="${EE_TEST_LOG_PATH:-$LOG_DIR/events.jsonl}"
export EE_TEST_LOG_PATH

e2e_log_start "$TEST_NAME" "$EE_TEST_LOG_PATH"
e2e_log_note "agent_ergonomics_setup workspace=$WORKSPACE log_dir=$LOG_DIR ee_bin=$EE_BIN"

_agent_ergonomics_finalized=0

record_failure() {
    local label="${1:?label required}"
    local detail="${2:-failed}"
    FAIL=$((FAIL + 1))
    FAILURES+=("$label: $detail")
    e2e_log_note "agent_ergonomics_failure label=$label detail=$detail"
}

record_pass() {
    local label="${1:?label required}"
    PASS=$((PASS + 1))
    e2e_log_note "agent_ergonomics_pass label=$label"
}

log_step() {
    local label="${1:?step label required}"
    STEP=$((STEP + 1))
    printf '[%02d] %s\n' "$STEP" "$label" >&2
    e2e_log_note "step=$STEP label=$label"
}

log_run() {
    local label="${1:?command label required}"
    shift
    if [ "$#" -eq 0 ]; then
        record_failure "$label" "missing command"
        return 2
    fi

    log_step "$label"

    local stdout_file
    local stderr_file
    stdout_file="$LOG_DIR/step_$(printf '%02d' "$STEP")_stdout.txt"
    stderr_file="$LOG_DIR/step_$(printf '%02d' "$STEP")_stderr.txt"
    local args_str=""
    local arg
    for arg in "$@"; do
        if [ -z "$args_str" ]; then
            args_str="$arg"
        else
            args_str="$args_str"$'\x01'"$arg"
        fi
    done
    _e2e_emit_event "command_start" "command" "$1" "args" "$args_str"
    local started
    started=$(date +%s)

    set +e
    "$@" >"$stdout_file" 2>"$stderr_file"
    local rc=$?
    set -e

    local ended elapsed
    ended=$(date +%s)
    elapsed=$((ended - started))

    _e2e_emit_event "command_end" \
        "command" "$1" \
        "args" "$args_str" \
        "stdout_hash" "$(_e2e_hash_file "$stdout_file")" \
        "stderr_excerpt" "$(head -c "${EE_TEST_LOG_STDERR_CAP:-4096}" "$stderr_file")" \
        "exit_code" "$rc" \
        "elapsed_ms" "$((elapsed * 1000))"
    e2e_log_note "command_artifacts label=$label stdout=$stdout_file stderr=$stderr_file exit_code=$rc elapsed_seconds=$elapsed"

    if [ "$rc" -eq 0 ]; then
        record_pass "$label"
    else
        record_failure "$label" "exit_code=$rc stdout=$stdout_file stderr=$stderr_file"
    fi
    return "$rc"
}

assert_jq() {
    local json="${1:-}"
    local filter="${2:?jq filter required}"
    local want="${3:-}"
    local label="${4:?assertion label required}"

    log_step "$label"

    local got
    got="$(printf '%s' "$json" | jq -r "$filter" 2>/dev/null || true)"
    if [ "$got" = "$want" ]; then
        record_pass "$label"
        e2e_log_assert_eq "$got" "$want" "$label"
        return 0
    fi

    record_failure "$label" "expected=$want actual=${got:-<empty>}"
    e2e_log_assert_eq "$got" "$want" "$label" || true
    return 1
}

assert_contains() {
    local haystack="${1:-}"
    local needle="${2:?needle required}"
    local label="${3:?assertion label required}"

    log_step "$label"

    if [[ "$haystack" == *"$needle"* ]]; then
        record_pass "$label"
        e2e_log_assert_eq "contains" "contains" "$label"
        return 0
    fi

    record_failure "$label" "missing substring"
    e2e_log_assert_eq "missing" "contains:$needle" "$label" || true
    return 1
}

finalize() {
    local rc=$?
    if [ "$_agent_ergonomics_finalized" -eq 1 ]; then
        return "$rc"
    fi
    _agent_ergonomics_finalized=1

    if [ "$FAIL" -gt 0 ] && [ "$rc" -eq 0 ]; then
        rc=1
    fi

    e2e_log_note "agent_ergonomics_summary pass=$PASS fail=$FAIL log_dir=$LOG_DIR"
    e2e_log_end

    printf 'agent_ergonomics: pass=%d fail=%d log_dir=%s\n' "$PASS" "$FAIL" "$LOG_DIR" >&2
    if [ "$FAIL" -gt 0 ]; then
        printf 'agent_ergonomics failures:\n' >&2
        printf '  - %s\n' "${FAILURES[@]}" >&2
    fi

    return "$rc"
}

trap finalize EXIT
