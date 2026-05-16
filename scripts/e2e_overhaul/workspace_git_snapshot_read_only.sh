#!/usr/bin/env bash
# Logged e2e/RCH proof driver for bd-1eq3l.9.1.
#
# This script is intentionally a verifier wrapper around the focused Rust
# contract test because the workspace Git snapshot provider is not a standalone
# CLI surface yet. It never stages, cleans, deletes, or mutates repository files.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
EVENT_DIR="${EE_WORKSPACE_GIT_SNAPSHOT_EVENT_DIR:-${TMPDIR:-/tmp}/ee-workspace-git-snapshot-read-only}"
EVENT_LOG="$EVENT_DIR/events.jsonl"
STARTED_NS="$(date +%s%N)"

mkdir -p "$EVENT_DIR"
: > "$EVENT_LOG"

emit_event() {
    local phase="${1:?phase required}"
    local status="${2:?status required}"
    local exit_code="${3:?exit code required}"
    local command_text="${4:-}"
    local first_failure="${5:-}"
    local degraded_codes="${6:-[]}"
    local stdout_artifact="${7:-}"
    local stderr_artifact="${8:-}"
    local finished_ns elapsed_ms
    finished_ns="$(date +%s%N)"
    elapsed_ms="$(( (finished_ns - STARTED_NS) / 1000000 ))"

    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg kind "workspace_git_snapshot_read_only" \
        --arg phase "$phase" \
        --arg status "$status" \
        --arg command "$command_text" \
        --arg workspace "$REPO_ROOT" \
        --arg stdout_artifact "$stdout_artifact" \
        --arg stderr_artifact "$stderr_artifact" \
        --arg first_failure "$first_failure" \
        --argjson exit_code "$exit_code" \
        --argjson elapsed_ms "$elapsed_ms" \
        --argjson degraded_codes "$degraded_codes" \
        '{
          schema: $schema,
          kind: $kind,
          scenario: "workspace_git_snapshot_contract",
          phase: $phase,
          status: $status,
          command: $command,
          workspace: $workspace,
          elapsedMs: $elapsed_ms,
          exitCode: $exit_code,
          stdoutArtifact: (if $stdout_artifact == "" then null else $stdout_artifact end),
          stderrArtifact: (if $stderr_artifact == "" then null else $stderr_artifact end),
          degradedCodes: $degraded_codes,
          firstFailureDiagnosis: (if $first_failure == "" then null else $first_failure end)
        }' | tee -a "$EVENT_LOG" >&2
}

require_tool() {
    local tool="${1:?tool required}"
    if ! command -v "$tool" >/dev/null 2>&1; then
        emit_event "preflight" "blocked" 2 "command -v $tool" "missing required tool: $tool" '["tool_unavailable"]'
        exit 2
    fi
}

require_tool jq
require_tool git

cd "$REPO_ROOT"

DIRTY_TRACKED="$(
    git status --porcelain --untracked-files=no |
        awk '
            $0 ~ /scripts\/e2e_overhaul\/workspace_git_snapshot_read_only\.sh$/ { next }
            { print }
        '
)"

if [ -n "$DIRTY_TRACKED" ] && [ "${EE_WORKSPACE_GIT_SNAPSHOT_ALLOW_DIRTY:-0}" != "1" ]; then
    dirty_artifact="$EVENT_DIR/dirty_tracked.txt"
    printf '%s\n' "$DIRTY_TRACKED" > "$dirty_artifact"
    emit_event \
        "preflight" \
        "blocked" \
        6 \
        "git status --porcelain --untracked-files=no" \
        "dirty tracked checkout; refusing unattributable RCH proof" \
        '["dirty_tracked_checkout"]' \
        "$dirty_artifact" \
        ""
    printf 'workspace git snapshot proof blocked by dirty tracked checkout; events=%s\n' "$EVENT_LOG" >&2
    exit 6
fi

stdout_artifact="$EVENT_DIR/rch_stdout.log"
stderr_artifact="$EVENT_DIR/rch_stderr.log"
command_text="scripts/rch_verify.sh --bead-id bd-1eq3l.9.1 --summary --no-write -- cargo test --test contracts workspace_git_snapshot_provider_is_read_only -- --nocapture"

set +e
RCH_REQUIRE_REMOTE=1 \
    "$REPO_ROOT/scripts/rch_verify.sh" \
    --bead-id bd-1eq3l.9.1 \
    --summary \
    --no-write \
    -- \
    cargo test --test contracts workspace_git_snapshot_provider_is_read_only -- --nocapture \
    >"$stdout_artifact" 2>"$stderr_artifact"
exit_code=$?
set -e

if [ "$exit_code" -eq 0 ]; then
    emit_event "rch_contract" "pass" 0 "$command_text" "" "[]" "$stdout_artifact" "$stderr_artifact"
    printf 'workspace git snapshot read-only proof passed; events=%s\n' "$EVENT_LOG" >&2
    exit 0
fi

first_failure="$(tail -n 20 "$stderr_artifact" "$stdout_artifact" 2>/dev/null | tr '\n' ' ' | cut -c 1-500)"
emit_event \
    "rch_contract" \
    "failed" \
    "$exit_code" \
    "$command_text" \
    "$first_failure" \
    '["rch_verify_remote_command_failed"]' \
    "$stdout_artifact" \
    "$stderr_artifact"
printf 'workspace git snapshot read-only proof failed; events=%s\n' "$EVENT_LOG" >&2
exit "$exit_code"
