#!/usr/bin/env bash
# bd-1n0np.1.6 - real-binary E2E coverage for `ee why-not`.

# NOTE: no `set -e` — the harness assert_* helpers accumulate pass/fail and
# `harness_summary` decides the exit code (same pattern as e2e_primer_agentsmd.sh).
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
REQUESTED_EE_BIN="${EE_BIN:-}"

# Keep validation artifacts by default. The shared harness can delete temp
# command captures/workspaces when these are unset; this feature proof keeps the
# evidence trail intact for agent closeout.
export EE_E2E_KEEP="${EE_E2E_KEEP:-1}"
export EE_E2E_KEEP_ARTIFACTS="${EE_E2E_KEEP_ARTIFACTS:-1}"

# shellcheck source=scripts/lib/e2e_harness.sh
# shellcheck disable=SC1091
source "$REPO_ROOT/scripts/lib/e2e_harness.sh"

# log_event <kind> [key value]... — ee.test_event.v1 event plus a compact
# human-readable mirror on stderr.
log_event() {
    local kind="${1:?log_event: kind required}"
    shift
    if [ $(( $# % 2 )) -ne 0 ]; then
        _harness_fail "log_event $kind: expected key/value pairs"
        return 1
    fi
    _e2e_emit_event "$kind" "$@"
    printf '[harness] event %s' "$kind" >&2
    while [ $# -gt 0 ]; do
        printf ' %s=%s' "$1" "$2" >&2
        shift 2
    done
    printf '\n' >&2
}

# assert_json <json> <jq-filter> <expected> <label> — scalar extraction + eq.
assert_json() {
    local json="${1:?assert_json: json required}"
    local filter="${2:?assert_json: jq filter required}"
    local expected="${3:-}"
    local label="${4:-assert_json}"
    local actual
    if ! actual="$(printf '%s' "$json" | jq -r "$filter" 2>/dev/null)"; then
        e2e_log_assert_eq "jq_error" "$expected" "$label" || true
        _harness_fail "$label: jq filter failed [$filter]"
        return 0
    fi
    assert_eq "$actual" "$expected" "$label"
}

require_tool() {
    local tool="${1:?tool required}"
    if command -v "$tool" >/dev/null 2>&1; then
        _harness_pass "required tool available: $tool"
    else
        _harness_fail "required tool missing: $tool"
    fi
}

run_ee_json() {
    e2e_log_command "$EE_BIN" "$@"
}

binary_supports_why_not() {
    local candidate="${1:-}"
    [ -n "$candidate" ] || return 1
    if [ -x "$candidate" ]; then
        "$candidate" why-not --help >/dev/null 2>&1
        return $?
    fi
    if command -v "$candidate" >/dev/null 2>&1; then
        "$candidate" why-not --help >/dev/null 2>&1
        return $?
    fi
    return 1
}

select_why_not_binary() {
    if [ -n "$REQUESTED_EE_BIN" ]; then
        if binary_supports_why_not "$REQUESTED_EE_BIN"; then
            printf '%s' "$REQUESTED_EE_BIN"
            return 0
        fi
        return 1
    fi

    local target_dir=""
    target_dir="$(
        cd "$REPO_ROOT" &&
            cargo metadata --no-deps --format-version 1 2>/dev/null |
            python3 -c 'import json,sys; print(json.load(sys.stdin).get("target_directory",""))' 2>/dev/null
    )" || target_dir=""

    local path_ee=""
    path_ee="$(command -v ee 2>/dev/null || true)"

    local candidate
    for candidate in \
        "${target_dir:+$target_dir/debug/ee}" \
        "${target_dir:+$target_dir/release/ee}" \
        "$EE_BIN" \
        "$path_ee"
    do
        [ -n "$candidate" ] || continue
        if binary_supports_why_not "$candidate"; then
            printf '%s' "$candidate"
            return 0
        fi
    done
    return 1
}

run_ee_capture() {
    local __out_var="${1:?output variable required}"
    local label="${2:?label required}"
    shift 2
    # Locals deliberately avoid common caller variable names: printf -v writes
    # through bash dynamic scoping, so a `local output` here would shadow the
    # caller's variable and leave it unset.
    local __captured
    local __exit_code
    __captured="$(run_ee_json "$@")"
    __exit_code=$?
    printf -v "$__out_var" '%s' "$__captured"
    if [ "$__exit_code" -eq 0 ]; then
        _harness_pass "$label command exit 0"
    else
        _harness_fail "$label command exit $__exit_code"
    fi
    return 0
}

run_ee_capture_status() {
    local __out_var="${1:?output variable required}"
    local __status_var="${2:?status variable required}"
    shift 2
    local __captured
    local __exit_code
    __captured="$(run_ee_json "$@")"
    __exit_code=$?
    printf -v "$__out_var" '%s' "$__captured"
    printf -v "$__status_var" '%s' "$__exit_code"
    return 0
}

json_value() {
    local json="${1:?json required}"
    local filter="${2:?jq filter required}"
    local label="${3:-json_value}"
    local value
    if value="$(printf '%s' "$json" | jq -er "$filter" 2>/dev/null)"; then
        printf '%s' "$value"
        return 0
    fi
    _harness_fail "$label: jq filter failed [$filter]"
    printf ''
    return 1
}

assert_nonempty() {
    local value="${1:-}"
    local label="${2:-nonempty}"
    if [ -n "$value" ]; then
        _harness_pass "$label"
    else
        _harness_fail "$label: expected non-empty value"
    fi
}

assert_nonzero_exit() {
    local exit_code="${1:?exit code required}"
    local label="${2:-nonzero exit}"
    if [ "$exit_code" -ne 0 ]; then
        _harness_pass "$label (exit $exit_code)"
    else
        _harness_fail "$label: expected nonzero exit"
    fi
}

remember_fixture() {
    local workspace="${1:?workspace required}"
    local content="${2:?content required}"
    local output
    local memory_id

    run_ee_capture output \
        "remember fixture" \
        remember \
        --workspace "$workspace" \
        --level semantic \
        --kind note \
        --tags why-not,e2e \
        --no-auto-link \
        --no-propose-candidates \
        --json \
        "$content"
    assert_json "$output" '.schema' "ee.response.v2" "remember emits response envelope"
    assert_json "$output" '.success' "true" "remember succeeds"
    memory_id="$(json_value "$output" '.data.memory_id // .data.memoryId // empty' "remember memory id")" || memory_id=""
    assert_nonempty "$memory_id" "remember returned memory id"
    printf '%s' "$memory_id"
}

why_not_fixture() {
    local workspace="${1:?workspace required}"
    local memory_id="${2:?memory id required}"
    local task="${3:?task required}"

    run_ee_json why-not \
        "$memory_id" \
        --task "$task" \
        --workspace "$workspace" \
        --candidate-pool 20 \
        --max-tokens 1000 \
        --json
}

harness_init "why_not"
require_tool jq

selected_ee_bin="$(select_why_not_binary)" || selected_ee_bin=""
if [ -n "$selected_ee_bin" ]; then
    EE_BIN="$selected_ee_bin"
    export EE_BIN
    _harness_pass "ee binary supports why-not: $EE_BIN"
else
    _harness_fail "no resolved ee binary supports why-not; build through RCH first or set EE_BIN"
    harness_summary
    exit $?
fi

step "seed isolated workspace"
workspace=""
init_output=""
with_temp_workspace workspace
run_ee_capture init_output "init" init --workspace "$workspace" --json
assert_json "$init_output" '.schema' "ee.response.v2" "init emits response envelope"
assert_json "$init_output" '.success' "true" "init succeeds"

release_memory_id="$(
    remember_fixture "$workspace" \
        "Run cargo fmt --check before the release verification step."
)"
unrelated_memory_id="$(
    remember_fixture "$workspace" \
        "Banana mango smoothie recipe with crushed ice."
)"
log_event "note" \
    "phase" "seed" \
    "releaseMemoryId" "$release_memory_id" \
    "unrelatedMemoryId" "$unrelated_memory_id"

step "selected memory reports authoritative selected path"
selected_first=""
selected_second=""
run_ee_capture selected_first \
    "why-not selected first" \
    why-not "$release_memory_id" \
    --task "prepare release verification" \
    --workspace "$workspace" \
    --candidate-pool 20 \
    --max-tokens 1000 \
    --json
run_ee_capture selected_second \
    "why-not selected second" \
    why-not "$release_memory_id" \
    --task "prepare release verification" \
    --workspace "$workspace" \
    --candidate-pool 20 \
    --max-tokens 1000 \
    --json
assert_json "$selected_first" '.schema' "ee.response.v2" "why-not selected envelope"
assert_json "$selected_first" '.success' "true" "why-not selected succeeds"
assert_json "$selected_first" '.data.schema' "ee.why_not_selected.v1" "why-not selected data schema"
assert_json "$selected_first" '.data.memoryId' "$release_memory_id" "why-not selected memory id"
assert_json "$selected_first" '.data.selected' "true" "why-not selected flag"
assert_json "$selected_first" '.data.primaryReason' "selected" "why-not selected primary reason"
assert_json "$selected_first" '.data.reasonSource' "authoritative" "why-not selected reason source"
assert_jq "$selected_first" '.data.counterfactualHints | type == "array"' "why-not selected hints array"
assert_eq \
    "$(printf '%s' "$selected_first" | jq -c '.data')" \
    "$(printf '%s' "$selected_second" | jq -c '.data')" \
    "why-not selected output is deterministic"

# With the candidate pool (20) larger than the seeded memory count, every
# memory reaches the selection ledger, so the unrelated memory gets an
# authoritative omitted_by_score_floor verdict rather than the reconstructed
# not_retrieved path (which needs a memory absent from the candidate universe).
step "unrelated memory reports authoritative omitted_by_score_floor"
unretrieved_output=""
run_ee_capture unretrieved_output \
    "why-not low-score" \
    why-not "$unrelated_memory_id" \
    --task "prepare release verification gate" \
    --workspace "$workspace" \
    --candidate-pool 20 \
    --max-tokens 1000 \
    --json
assert_json "$unretrieved_output" '.schema' "ee.response.v2" "why-not low-score envelope"
assert_json "$unretrieved_output" '.success' "true" "why-not low-score succeeds"
assert_json "$unretrieved_output" '.data.schema' "ee.why_not_selected.v1" "why-not low-score data schema"
assert_json "$unretrieved_output" '.data.memoryId' "$unrelated_memory_id" "why-not low-score memory id"
assert_json "$unretrieved_output" '.data.selected' "false" "why-not low-score selected flag"
assert_json "$unretrieved_output" '.data.primaryReason' "omitted_by_score_floor" "why-not low-score primary reason"
assert_json "$unretrieved_output" '.data.reasonSource' "authoritative" "why-not low-score reason source"
assert_jq "$unretrieved_output" '.data.counterfactualHints | length >= 1' "why-not low-score offers hint"

step "missing memory id fails closed"
missing_memory_id="mem_00000000000000000000000000"
missing_output=""
missing_exit=0
run_ee_capture_status missing_output missing_exit \
    why-not "$missing_memory_id" \
    --task "prepare release verification" \
    --workspace "$workspace" \
    --candidate-pool 20 \
    --max-tokens 1000 \
    --json
assert_nonzero_exit "$missing_exit" "why-not missing memory fails closed"
assert_json "$missing_output" '.schema' "ee.error.v2" "why-not missing emits error envelope"
assert_jq "$missing_output" '.error.message | contains("not found")' "why-not missing names not found"

log_event "note" \
    "phase" "selector_contract_coverage" \
    "realBinaryReasons" "selected,omitted_by_score_floor,missing_memory_id" \
    "libraryPinnedReasons" "omitted_by_token_budget,not_retrieved,excluded_by_scope,excluded_by_redaction,excluded_by_validity_window,not_retrieved_due_to_degraded_index"

end_temp_workspace
harness_summary
