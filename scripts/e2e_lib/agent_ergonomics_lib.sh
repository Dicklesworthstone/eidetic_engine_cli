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

# bd-overhaul-false-binary-attestation-2rmdw: honour the pinned binary.
# EE_BIN wins if the caller set it, then EE_BINARY (exported by
# scripts/verify.sh:201), and only then PATH. verify.sh invokes this
# stage with no explicit pin, so defaulting straight to `ee` meant the
# gate could pass against whatever ee happened to be installed.
EE_BIN="${EE_BIN:-${EE_BINARY:-ee}}"
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

# Compare one jq-extracted value against an expected one.
#
# FOUR OUTCOMES, EACH NAMED DIFFERENTLY. The previous form had two:
#
#     got="$(printf '%s' "$json" | jq -r "$filter" 2>/dev/null || true)"
#     if [ "$got" = "$want" ]; then record_pass ...
#     record_failure "$label" "expected=$want actual=${got:-<empty>}"
#
# `2>/dev/null` discarded jq's error, `|| true` discarded its exit, so a
# filter that did not COMPILE produced got="" and was reported as a value
# mismatch -- "expected=absent actual=<empty>", which reads as the product
# returning nothing. Two live instances of exactly that are bd-kyeyw
# (e2e_curate_reject_with_reason.sh:96 and :107, backslash-escaped quotes
# inside single quotes). bd-82aq1 is the same defect class in the harness
# helper; this is its more dangerous sibling, because that one compares
# against the literal "true" and this one compares against whatever the
# caller passes.
#
# THE EMPTY-WANT REFUSAL IS WHAT CLOSES THE SILENT-PASS HOLE. An empty
# `want` compares equal to the empty `got` a failed jq produces, so a
# broken filter plus an empty expectation was a PASS that ran nothing.
# Enumerated before changing this: 82 call sites across the four files
# that source this library, 81 literal wants, one from a variable
# (e2e_harmful_burst_quarantine.sh:144), zero empty. The variable one is
# guarded at its point of production (:102-105 makes an empty jq result
# fatal), so nothing in the tree relies on the old tolerance. The hazard
# was entirely in the 83rd call site, written by someone who never read
# the bead.
#
# It also makes the two-failures-cancelling shape IMPOSSIBLE rather than
# merely unreached: a want produced by a jq that failed arrives here
# empty, and an empty want is now refused before any comparison. The
# helper cannot see how its want was computed, so refusing the empty
# value is the only place that shape can be stopped.
assert_jq() {
    local json="${1:-}"
    local filter="${2:?jq filter required}"
    local want="${3:-}"
    local label="${4:?assertion label required}"

    log_step "$label"

    # A HARNESS ERROR, not an assertion failure. Named distinctly on
    # purpose: bd-82aq1 exists because four outcomes were indistinguishable,
    # so this must not read like a product defect.
    if [ -z "$want" ]; then
        record_failure "$label" \
            "HARNESS ERROR: assert_jq called with an empty expected value; an empty want matches the empty output of a failed jq, so this assertion could never have failed. Pass the value you mean, or guard the variable that produced it."
        e2e_log_note "agent_ergonomics_harness_error label=$label reason=empty_want filter=$filter"
        return 2
    fi

    local got rc
    got="$(printf '%s' "$json" | jq -r "$filter" 2>&1)"
    rc=$?
    # jq's exit codes are distinct and were being collapsed: 1 is a false
    # property, 3 is a filter that did not compile, 4 is no output, 5 is
    # input that is not JSON. Only a zero exit means the filter ran and
    # produced the value being compared; anything else is a broken test or
    # a broken command, and $got holds jq's own error text because stderr
    # is no longer discarded.
    if [ "$rc" -ne 0 ]; then
        record_failure "$label" \
            "HARNESS ERROR: jq exited $rc for filter [$filter] -- $(printf '%s' "$got" | head -c 400)"
        e2e_log_note "agent_ergonomics_harness_error label=$label reason=jq_exit_$rc filter=$filter"
        return 2
    fi

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
