#!/usr/bin/env bash
# No-Cargo smoke for the swarm replay lab public workflow.
#
# This script expects an already-built ee binary. It generates a small
# redaction-safe workload, runs the admission-only replay path, and records
# ee.test_event.v1 evidence with sanitized paths and environment posture.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# shellcheck disable=SC1091
source "$REPO_ROOT/scripts/lib/ee_binary_resolution.sh"

EE_BIN="$(ee_resolve_binary debug)"
RUN_ROOT="${TMPDIR:-/tmp}/ee-swarm-replay-lab-smoke.${BASHPID:-$$}"
WORKSPACE="$RUN_ROOT/workspace"
RUN_HOME="$RUN_ROOT/home"
ARTIFACT_DIR="$RUN_ROOT/artifacts"
EVENT_LOG="$ARTIFACT_DIR/events.jsonl"
TRACE_PATH="$ARTIFACT_DIR/smoke-swarm-workload.json"
TEST_ID="swarm_replay_lab_smoke"

mkdir -p "$WORKSPACE" "$RUN_HOME" "$ARTIFACT_DIR"
: >"$EVENT_LOG"

if [ ! -x "$EE_BIN" ]; then
    printf 'error: ee binary not found or not executable: %s\n' "$EE_BIN" >&2
    printf '       run the Cargo test/build gate through RCH before this smoke.\n' >&2
    exit 3
fi

if ! command -v jq >/dev/null 2>&1; then
    printf 'error: jq is required for swarm replay lab smoke\n' >&2
    exit 2
fi

now_iso() {
    date -u +"%Y-%m-%dT%H:%M:%SZ"
}

now_ms() {
    local value
    value="$(date +%s%3N 2>/dev/null || true)"
    if [[ "$value" =~ ^[0-9]+$ ]]; then
        printf '%s\n' "$value"
    else
        printf '%s000\n' "$(date +%s)"
    fi
}

hash_file() {
    local file="$1"
    if command -v b3sum >/dev/null 2>&1; then
        printf 'blake3:%s' "$(b3sum "$file" | awk '{print $1}')"
    else
        printf 'sha256:%s' "$(shasum -a 256 "$file" | awk '{print $1}')"
    fi
}

hash_string() {
    local value="$1"
    if command -v b3sum >/dev/null 2>&1; then
        printf '%s' "$value" | b3sum | awk '{printf "blake3:%s", $1}'
    else
        printf '%s' "$value" | shasum -a 256 | awk '{printf "sha256:%s", $1}'
    fi
}

path_tail() {
    local path="$1"
    case "$path" in
        "$WORKSPACE")
            printf '[WORKSPACE]'
            ;;
        "$WORKSPACE"/*)
            printf '[WORKSPACE]/%s' "${path#"$WORKSPACE"/}"
            ;;
        "$RUN_ROOT")
            printf '[RUN]'
            ;;
        "$RUN_ROOT"/*)
            printf '[RUN]/%s' "${path#"$RUN_ROOT"/}"
            ;;
        "$REPO_ROOT")
            printf '[REPO]'
            ;;
        "$REPO_ROOT"/*)
            printf '[REPO]/%s' "${path#"$REPO_ROOT"/}"
            ;;
        *)
            basename "$path"
            ;;
    esac
}

sanitize_arg() {
    local arg="$1"
    case "$arg" in
        "$EE_BIN")
            printf 'ee'
            ;;
        "$WORKSPACE"|"$WORKSPACE"/*|"$RUN_ROOT"|"$RUN_ROOT"/*|"$REPO_ROOT"|"$REPO_ROOT"/*)
            path_tail "$arg"
            ;;
        *)
            printf '%s' "$arg"
            ;;
    esac
}

args_json() {
    local sanitized=()
    local arg
    for arg in "$@"; do
        sanitized+=("$(sanitize_arg "$arg")")
    done
    printf '%s\n' "${sanitized[@]}" | jq -R -s 'split("\n")[:-1]'
}

emit_command_start() {
    local label="$1"
    shift
    local args
    args="$(args_json "$@")"
    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg ts "$(now_iso)" \
        --arg test_id "$TEST_ID" \
        --arg command "ee" \
        --argjson args "$args" \
        --arg label "$label" \
        --arg workspace_hash "$(hash_string "$WORKSPACE")" \
        '{
          schema: $schema,
          ts: $ts,
          test_id: $test_id,
          kind: "command_start",
          command: $command,
          args: $args,
          fields: {
            label: $label,
            workspace: "[WORKSPACE]",
            workspace_hash: $workspace_hash,
            sanitized_env: {
              HOME: "[RUN_HOME]",
              NO_COLOR: "1",
              EE_WORKSPACE: "[unset]"
            }
          }
        }' >>"$EVENT_LOG"
}

emit_command_end() {
    local label="$1"
    local exit_code="$2"
    local elapsed_ms="$3"
    local stdout_file="$4"
    local stderr_file="$5"
    shift 5
    local args
    args="$(args_json "$@")"
    local stdout_bytes
    local stderr_bytes
    stdout_bytes="$(wc -c <"$stdout_file" | tr -d ' ')"
    stderr_bytes="$(wc -c <"$stderr_file" | tr -d ' ')"
    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg ts "$(now_iso)" \
        --arg test_id "$TEST_ID" \
        --arg command "ee" \
        --argjson args "$args" \
        --argjson exit_code "$exit_code" \
        --argjson elapsed_ms "$elapsed_ms" \
        --arg stdout_hash "$(hash_file "$stdout_file")" \
        --arg stderr_hash "$(hash_file "$stderr_file")" \
        --arg label "$label" \
        --arg workspace_hash "$(hash_string "$WORKSPACE")" \
        --arg stdout_artifact_path "$(path_tail "$stdout_file")" \
        --arg stderr_artifact_path "$(path_tail "$stderr_file")" \
        --arg stdout_artifact_path_hash "$(hash_string "$stdout_file")" \
        --arg stderr_artifact_path_hash "$(hash_string "$stderr_file")" \
        --argjson stdout_bytes "$stdout_bytes" \
        --argjson stderr_bytes "$stderr_bytes" \
        '{
          schema: $schema,
          ts: $ts,
          test_id: $test_id,
          kind: "command_end",
          command: $command,
          args: $args,
          exit_code: $exit_code,
          elapsed_ms: $elapsed_ms,
          stdout_hash: $stdout_hash,
          stderr_hash: $stderr_hash,
          fields: {
            label: $label,
            workspace: "[WORKSPACE]",
            workspace_hash: $workspace_hash,
            sanitized_env: {
              HOME: "[RUN_HOME]",
              NO_COLOR: "1",
              EE_WORKSPACE: "[unset]"
            },
            stdout_artifact_path: $stdout_artifact_path,
            stderr_artifact_path: $stderr_artifact_path,
            stdout_artifact_path_hash: $stdout_artifact_path_hash,
            stderr_artifact_path_hash: $stderr_artifact_path_hash,
            stdout_bytes: $stdout_bytes,
            stderr_bytes: $stderr_bytes
          }
        }' >>"$EVENT_LOG"
}

emit_assert_ok() {
    local label="$1"
    local replay_status="$2"
    local rch_status="$3"
    local first_failure_diagnosis="$4"
    local command_exit_code="$5"
    local command_elapsed_ms="$6"
    local stdout_file="$7"
    local stderr_file="$8"
    local event
    event="$(
        jq -cn \
            --arg schema "ee.test_event.v1" \
            --arg ts "$(now_iso)" \
            --arg test_id "$TEST_ID" \
            --arg label "$label" \
            --arg replay_status "$replay_status" \
            --arg rch_status "$rch_status" \
            --arg first_failure_diagnosis "$first_failure_diagnosis" \
            --arg stdout_artifact_path "$(path_tail "$stdout_file")" \
            --arg stderr_artifact_path "$(path_tail "$stderr_file")" \
            --argjson exit_code "$command_exit_code" \
            --argjson elapsed_ms "$command_elapsed_ms" \
            '{
              schema: $schema,
              ts: $ts,
              test_id: $test_id,
              kind: "assert_ok",
              fields: {
                label: $label,
                command: "ee lab swarm replay --trace [RUN]/artifacts/smoke-swarm-workload.json --dry-run --json",
                workspace: "[WORKSPACE]",
                sanitized_env: {
                  HOME: "[RUN_HOME]",
                  NO_COLOR: "1",
                  EE_WORKSPACE: "[unset]"
                },
                exit_code: $exit_code,
                elapsed_ms: $elapsed_ms,
                stdout_artifact_path: $stdout_artifact_path,
                stderr_artifact_path: $stderr_artifact_path,
                schema_validation_status: "passed",
                redaction_status: "passed",
                first_failure_diagnosis: $first_failure_diagnosis,
                replay_status: $replay_status,
                rch_status: $rch_status
              }
            }'
    )"
    printf '%s\n' "$event" >>"$EVENT_LOG"
    printf '%s\n' "$event"
}

run_ee() {
    local label="$1"
    shift
    local stdout_file="$ARTIFACT_DIR/${label}.stdout.json"
    local stderr_file="$ARTIFACT_DIR/${label}.stderr.txt"
    emit_command_start "$label" "$@"
    local start
    start="$(now_ms)"
    local exit_code=0
    set +e
    env HOME="$RUN_HOME" NO_COLOR=1 "$EE_BIN" "$@" >"$stdout_file" 2>"$stderr_file"
    exit_code=$?
    set -e
    local end
    end="$(now_ms)"
    local elapsed_ms=$((end - start))
    emit_command_end "$label" "$exit_code" "$elapsed_ms" "$stdout_file" "$stderr_file" "$@"
    LAST_EXIT_CODE="$exit_code"
    LAST_ELAPSED_MS="$elapsed_ms"
    LAST_STDOUT="$stdout_file"
    LAST_STDERR="$stderr_file"
}

validate_event_log() {
    local lines=0
    local line
    while IFS= read -r line; do
        lines=$((lines + 1))
        printf '%s\n' "$line" | jq -e '
          .schema == "ee.test_event.v1"
          and (.ts | type == "string")
          and .test_id == "swarm_replay_lab_smoke"
          and (.kind | IN("command_start", "command_end", "assert_ok"))
        ' >/dev/null
    done <"$EVENT_LOG"
    if [ "$lines" -lt 5 ]; then
        printf 'error: expected at least 5 test events, got %s\n' "$lines" >&2
        exit 1
    fi
    if grep -Fq "$WORKSPACE" "$EVENT_LOG" || grep -Fq "$RUN_ROOT" "$EVENT_LOG"; then
        printf 'error: event log leaked raw workspace or run root path: %s\n' "$EVENT_LOG" >&2
        exit 1
    fi
}

run_ee generate-workload \
    --workspace "$WORKSPACE" \
    --json \
    lab generate-workload \
    --fixture-seed smoke_replay_lab_smoke_001 \
    --profile small

if [ "$LAST_EXIT_CODE" -ne 0 ]; then
    printf 'error: generate-workload exited %s\n' "$LAST_EXIT_CODE" >&2
    exit 1
fi

jq -e '
  .schema == "ee.swarm_workload.v1"
  and .sideEffectFree == true
  and .agentCount == 4
  and (.commandSequence | length) == 6
  and .resourceProfileHints.profile == "ci_smoke"
' "$LAST_STDOUT" >/dev/null

cp "$LAST_STDOUT" "$TRACE_PATH"

run_ee swarm-replay-dry-run \
    --workspace "$WORKSPACE" \
    --json \
    lab swarm replay \
    --trace "$TRACE_PATH" \
    --dry-run

if [ "$LAST_EXIT_CODE" -ne 6 ]; then
    printf 'error: dry-run replay expected exit 6 for missing RCH proof, got %s\n' "$LAST_EXIT_CODE" >&2
    exit 1
fi

jq -e '
  .schema == "ee.swarm_replay_result.v1"
  and .sideEffectFree == true
  and .status == "degraded"
  and .verification.rchRequired == true
  and .verification.rchStatus == "blocked_before_cargo"
  and .verification.proofCapsule.proofLevel == "static_replay_only"
  and .firstFailure == null
  and .redactionStatus.redactionProbesPassed == true
  and .redactionStatus.secretsPresent == false
  and .redactionStatus.absoluteHostPathPresent == false
  and (.warnings | any(contains("swarm_replay_dry_run_admission_only")))
  and (.warnings | any(contains("swarm_replay_rch_proof_missing")))
' "$LAST_STDOUT" >/dev/null

if grep -Fq "$WORKSPACE" "$LAST_STDOUT" ||
    grep -Fq "$RUN_ROOT" "$LAST_STDOUT" ||
    grep -Eq '/Users/|/data/projects/|SECRET_TOKEN|raw task content|raw query text|memory body payload|mail body payload|HOME=/' "$LAST_STDOUT"; then
    printf 'error: replay output leaked private or raw fixture content\n' >&2
    exit 1
fi

FIRST_FAILURE_DIAGNOSIS="$(
    jq -r '
      if .firstFailure == null then
        "none"
      elif (.firstFailure.code // "") != "" then
        "firstFailure:" + .firstFailure.code
      else
        "unknown"
      end
    ' "$LAST_STDOUT"
)"
REPLAY_STATUS="$(jq -r '.status' "$LAST_STDOUT")"
RCH_STATUS="$(jq -r '.verification.rchStatus' "$LAST_STDOUT")"
REPLAY_EXIT_CODE="$LAST_EXIT_CODE"
REPLAY_ELAPSED_MS="$LAST_ELAPSED_MS"
REPLAY_STDOUT="$LAST_STDOUT"
REPLAY_STDERR="$LAST_STDERR"

emit_assert_ok \
    "swarm_replay_lab_smoke_logged_evidence" \
    "$REPLAY_STATUS" \
    "$RCH_STATUS" \
    "$FIRST_FAILURE_DIAGNOSIS" \
    "$REPLAY_EXIT_CODE" \
    "$REPLAY_ELAPSED_MS" \
    "$REPLAY_STDOUT" \
    "$REPLAY_STDERR"

validate_event_log

printf 'Artifacts: %s\n' "$ARTIFACT_DIR" >&2
printf 'swarm replay lab smoke passed; events=%s\n' "$EVENT_LOG" >&2
