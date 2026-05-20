#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
EVENT_ROOT="${EE_REPO_HYGIENE_EVENT_DIR:-${TMPDIR:-/tmp}/ee-repo-hygiene}"
case "$EVENT_ROOT" in
    /Volumes/*) EVENT_ROOT="/tmp/ee-repo-hygiene" ;;
esac
EVENT_LOG="$EVENT_ROOT/events.jsonl"

mkdir -p "$EVENT_ROOT"
: >"$EVENT_LOG"

emit_event() {
    local phase="${1:?phase required}"
    local status="${2:?status required}"
    local exit_code="${3:?exit code required}"
    local first_failure="${4:-}"
    local patterns_covered="${5:-0}"
    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg bead_id "bd-3usjw.74" \
        --arg surface "repo_hygiene_root_clutter" \
        --arg phase "$phase" \
        --arg status "$status" \
        --arg command "git check-ignore --no-index -v --stdin" \
        --arg workspace "$REPO_ROOT" \
        --arg first_failure "$first_failure" \
        --argjson exit_code "$exit_code" \
        --argjson elapsed_ms 0 \
        --argjson patterns_covered "$patterns_covered" \
        '{
          schema: $schema,
          beadId: $bead_id,
          surface: $surface,
          phase: $phase,
          status: $status,
          command: $command,
          workspace: $workspace,
          elapsedMs: $elapsed_ms,
          exitCode: $exit_code,
          firstFailure: (if $first_failure == "" then null else $first_failure end),
          patternsCovered: $patterns_covered
        }' >>"$EVENT_LOG"
}

require_tool() {
    local tool="${1:?tool required}"
    if ! command -v "$tool" >/dev/null 2>&1; then
        emit_event "setup" "blocked" 2 "missing required tool: $tool" 0
        printf 'repo_hygiene: missing required tool: %s\n' "$tool" >&2
        exit 2
    fi
}

require_tool git
require_tool jq

patterns=(
    "/test_*.rs"
    "/test_capture*"
    "/test_clamp*"
    "/test_drop*"
    "/test_ln_1p*"
    "/test_min*"
    "/test_minmax*"
    "/test_multibyte*"
    "/test_output*.log"
    "/temp_*.rs"
    "/ubs_*.txt"
    "/ubs_*.json"
    "/ubs_*.jsonl"
    "/ubs.json"
    "/db_for_loops*"
    "/findings.jsonl"
    "/pass*.jsonl"
    "/fix_*.sh"
    "/find_*.sh"
)

paths=(
    "test_dummy.rs"
    "test_capture_dummy"
    "test_clamp_dummy"
    "test_drop_dummy"
    "test_ln_1p_dummy"
    "test_min_dummy"
    "test_minmax_dummy"
    "test_multibyte_dummy"
    "test_output_dummy.log"
    "temp_dummy.rs"
    "ubs_dummy.txt"
    "ubs_dummy.json"
    "ubs_dummy.jsonl"
    "ubs.json"
    "db_for_loops_dummy.txt"
    "findings.jsonl"
    "pass_dummy.jsonl"
    "fix_dummy.sh"
    "find_dummy.sh"
)

if [ "${#patterns[@]}" -ne "${#paths[@]}" ]; then
    emit_event "scenario_plan" "failed" 1 "pattern/path matrix length mismatch" 0
    printf 'repo_hygiene: internal matrix length mismatch\n' >&2
    exit 1
fi

failures=()
for pattern in "${patterns[@]}"; do
    if ! grep -Fxq "$pattern" "$REPO_ROOT/.gitignore"; then
        failures+=(".gitignore missing $pattern")
    fi
    if ! grep -Fxq "$pattern" "$REPO_ROOT/.rchignore"; then
        failures+=(".rchignore missing $pattern")
    fi
done

for index in "${!paths[@]}"; do
    path="${paths[$index]}"
    expected_pattern="${patterns[$index]}"
    if ! output="$(printf '%s\n' "$path" | git -C "$REPO_ROOT" check-ignore --no-index -v --stdin 2>&1)"; then
        failures+=("$path did not match any gitignore rule; stderr/stdout: $output")
        continue
    fi
    if ! grep -Fq -- "$expected_pattern" <<<"$output"; then
        failures+=("$path matched a different gitignore rule; expected $expected_pattern; output: $output")
    fi
done

if [ "${#failures[@]}" -ne 0 ]; then
    first_failure="${failures[0]}"
    emit_event "pattern_check" "failed" 1 "$first_failure" "${#patterns[@]}"
    printf 'repo_hygiene: %s failure(s)\n' "${#failures[@]}" >&2
    printf '%s\n' "${failures[@]}" >&2
    exit 1
fi

emit_event "pattern_check" "pass" 0 "" "${#patterns[@]}"
printf 'repo_hygiene: all %s root scratchpad pattern(s) matched; events=%s\n' "${#patterns[@]}" "$EVENT_LOG" >&2
