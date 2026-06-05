#!/usr/bin/env bash
# No-Cargo E2E harness for the environment attestation public workflow.
#
# This script expects an already-built ee binary. It never builds, deletes,
# claims Beads, sends Agent Mail, or mutates git. All stdout/stderr artifacts
# and the structured event log are retained under a per-run artifact directory.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# shellcheck disable=SC1091
source "$REPO_ROOT/scripts/lib/ee_binary_resolution.sh"

EE_BIN="$(ee_resolve_binary "${EE_ENV_ATTESTATION_PROFILE:-debug}")"
RUN_BASE="${EE_ENV_ATTESTATION_RUN_BASE:-${TMPDIR:-/tmp}}"
RUN_ROOT="$RUN_BASE/ee-environment-attestation-e2e.${BASHPID:-$$}"
ARTIFACT_DIR="$RUN_ROOT/artifacts"
EVENT_LOG="$ARTIFACT_DIR/events.jsonl"
SETUP_STDOUT="$ARTIFACT_DIR/setup.stdout.txt"
SETUP_STDERR="$ARTIFACT_DIR/setup.stderr.txt"
VALIDATION_STDERR="$ARTIFACT_DIR/validation.stderr.txt"
WORKSPACE="${EE_ENV_ATTESTATION_WORKSPACE:-$REPO_ROOT}"
SNAPSHOT_PATH="${EE_ENV_ATTESTATION_AGENT_MAIL_SNAPSHOT:-}"
COMMAND_TIMEOUT_MS="${EE_ENV_ATTESTATION_COMMAND_TIMEOUT_MS:-30000}"
TEST_ID="environment_attestation_e2e"

mkdir -p "$ARTIFACT_DIR"
: >"$EVENT_LOG"
: >"$SETUP_STDOUT"
: >"$SETUP_STDERR"
: >"$VALIDATION_STDERR"

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
        "$SNAPSHOT_PATH")
            printf '[SNAPSHOT]'
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
        "$SNAPSHOT_PATH")
            printf '[SNAPSHOT]'
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
        --arg binary_hash "$(hash_file "$EE_BIN")" \
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
            binary_hash: $binary_hash,
            sanitized_env: {
              HOME: "[unchanged]",
              NO_COLOR: "1",
              EE_WORKSPACE: "[unset]",
              CARGO_TARGET_DIR: "[external-or-unset]",
              TMPDIR: "[redacted]"
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
        --arg stdout_artifact_path "$(path_tail "$stdout_file")" \
        --arg stderr_artifact_path "$(path_tail "$stderr_file")" \
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
            stdout_artifact_path: $stdout_artifact_path,
            stderr_artifact_path: $stderr_artifact_path,
            stdout_bytes: $stdout_bytes,
            stderr_bytes: $stderr_bytes
          }
        }' >>"$EVENT_LOG"
}

emit_assert_result() {
    local kind="$1"
    local label="$2"
    local command_text="$3"
    local first_failure_diagnosis="$4"
    local schema_validation_status="$5"
    local redaction_status="$6"
    local command_exit_code="$7"
    local command_elapsed_ms="$8"
    local stdout_file="$9"
    local stderr_file="${10}"
    local environment_verdict="${11:-unknown}"
    local source_test_verdict="${12:-unknown}"
    local degraded_codes="${13:-[]}"
    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg ts "$(now_iso)" \
        --arg test_id "$TEST_ID" \
        --arg kind "$kind" \
        --arg label "$label" \
        --arg command_text "$command_text" \
        --arg first_failure_diagnosis "$first_failure_diagnosis" \
        --arg schema_validation_status "$schema_validation_status" \
        --arg redaction_status "$redaction_status" \
        --arg stdout_artifact_path "$(path_tail "$stdout_file")" \
        --arg stderr_artifact_path "$(path_tail "$stderr_file")" \
        --arg environment_verdict "$environment_verdict" \
        --arg source_test_verdict "$source_test_verdict" \
        --argjson exit_code "$command_exit_code" \
        --argjson elapsed_ms "$command_elapsed_ms" \
        --argjson degraded_codes "$degraded_codes" \
        '{
          schema: $schema,
          ts: $ts,
          test_id: $test_id,
          kind: $kind,
          fields: {
            label: $label,
            command: $command_text,
            workspace: "[WORKSPACE]",
            sanitized_env: {
              HOME: "[unchanged]",
              NO_COLOR: "1",
              EE_WORKSPACE: "[unset]"
            },
            exit_code: $exit_code,
            elapsed_ms: $elapsed_ms,
            stdout_artifact_path: $stdout_artifact_path,
            stderr_artifact_path: $stderr_artifact_path,
            schema_validation_status: $schema_validation_status,
            redaction_status: $redaction_status,
            first_failure_diagnosis: $first_failure_diagnosis,
            environment_verdict: $environment_verdict,
            source_test_verdict: $source_test_verdict,
            degraded_codes: $degraded_codes,
            rch_status: "not_run_by_harness"
          }
        }' | tee -a "$EVENT_LOG"
}

fail_with_artifacts() {
    local label="$1"
    local command_text="$2"
    local first_failure_diagnosis="$3"
    local exit_code="$4"
    local command_exit_code="$5"
    local command_elapsed_ms="$6"
    local stdout_file="$7"
    local stderr_file="$8"
    local schema_validation_status="${9:-failed}"
    local redaction_status="${10:-passed}"
    local environment_verdict="${11:-unknown}"
    local source_test_verdict="${12:-unknown}"
    local degraded_codes="${13:-[]}"
    emit_assert_result \
        "assert_result" \
        "$label" \
        "$command_text" \
        "$first_failure_diagnosis" \
        "$schema_validation_status" \
        "$redaction_status" \
        "$command_exit_code" \
        "$command_elapsed_ms" \
        "$stdout_file" \
        "$stderr_file" \
        "$environment_verdict" \
        "$source_test_verdict" \
        "$degraded_codes" >/dev/null
    printf 'error: %s\n' "$first_failure_diagnosis" >&2
    printf 'Artifacts: %s\n' "$ARTIFACT_DIR" >&2
    exit "$exit_code"
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
    env NO_COLOR=1 "$EE_BIN" "$@" >"$stdout_file" 2>"$stderr_file"
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
    local event_lines=()
    mapfile -t event_lines <"$EVENT_LOG"
    for line in "${event_lines[@]}"; do
        lines=$((lines + 1))
        if ! printf '%s\n' "$line" | jq -e '
          .schema == "ee.test_event.v1"
          and (.ts | type == "string")
          and .test_id == "environment_attestation_e2e"
          and (.kind | IN("command_start", "command_end", "assert_ok", "assert_result"))
        ' >/dev/null; then
            printf 'error: event log line %s failed schema validation\n' "$lines" >"$VALIDATION_STDERR"
            cat "$VALIDATION_STDERR" >&2
            fail_with_artifacts \
                "event_log_schema_validation_failed" \
                "validate event log" \
                "event_log_schema_validation_failed" \
                1 \
                1 \
                0 \
                "$EVENT_LOG" \
                "$VALIDATION_STDERR"
        fi
    done
    if [ "$lines" -lt 3 ]; then
        printf 'error: expected at least 3 test events, got %s\n' "$lines" >"$VALIDATION_STDERR"
        cat "$VALIDATION_STDERR" >&2
        fail_with_artifacts \
            "event_log_too_short" \
            "validate event log" \
            "event_log_too_short" \
            1 \
            1 \
            0 \
            "$EVENT_LOG" \
            "$VALIDATION_STDERR"
    fi
    if grep -Fq "$WORKSPACE" "$EVENT_LOG" ||
        grep -Fq "$RUN_ROOT" "$EVENT_LOG" ||
        { [ -n "$SNAPSHOT_PATH" ] && grep -Fq "$SNAPSHOT_PATH" "$EVENT_LOG"; }; then
        printf 'error: event log leaked raw workspace, run root, or snapshot path\n' >"$VALIDATION_STDERR"
        cat "$VALIDATION_STDERR" >&2
        fail_with_artifacts \
            "event_log_redaction_failed" \
            "validate event log" \
            "event_log_redaction_failed" \
            1 \
            1 \
            0 \
            "$EVENT_LOG" \
            "$VALIDATION_STDERR"
    fi
}

if ! command -v jq >/dev/null 2>&1; then
    printf 'error: jq is required for environment attestation E2E\n' >"$SETUP_STDERR"
    cat "$SETUP_STDERR" >&2
    fail_with_artifacts \
        "setup_jq_unavailable" \
        "setup" \
        "jq_unavailable" \
        2 \
        2 \
        0 \
        "$SETUP_STDOUT" \
        "$SETUP_STDERR" \
        "not_run"
fi

if [ ! -x "$EE_BIN" ]; then
    printf 'error: ee binary not found or not executable: %s\n' "$EE_BIN" >"$SETUP_STDERR"
    printf 'run Cargo build/test gates through RCH before this E2E harness.\n' >>"$SETUP_STDERR"
    cat "$SETUP_STDERR" >&2
    fail_with_artifacts \
        "setup_ee_binary_unavailable" \
        "setup" \
        "ee_binary_unavailable" \
        3 \
        3 \
        0 \
        "$SETUP_STDOUT" \
        "$SETUP_STDERR" \
        "not_run"
fi

FULL_AGENT_MAIL_SNAPSHOT="$ARTIFACT_DIR/agent-mail-full-snapshot.json"
HEALTH_AGENT_MAIL_SNAPSHOT="$ARTIFACT_DIR/agent-mail-health-degraded.json"
CI_STALE_PROOF_SNAPSHOT="$REPO_ROOT/tests/fixtures/ci_proof_lane/artifact_stale.json"
CI_CANCELLED_PROOF_SNAPSHOT="$REPO_ROOT/tests/fixtures/ci_proof_lane/cancelled_before_artifact.json"
LOCAL_CARGO_PROCESS_SCAN="$ARTIFACT_DIR/local-cargo-bypass-scan.json"

cat >"$FULL_AGENT_MAIL_SNAPSHOT" <<'JSON'
{
  "schema": "ee.agent_mail.snapshot.v1",
  "captured_at": "2026-06-05T08:00:00Z",
  "agents": [
    {"name": "RubyElk", "last_active_ts": "2026-06-05T08:00:00Z"},
    {"name": "TurquoiseTern", "last_active_ts": "2026-06-05T08:00:00Z"}
  ],
  "file_reservations": [
    {
      "path_pattern": "src/core/*.rs",
      "holder": "TurquoiseTern",
      "exclusive": true,
      "expires_ts": "2026-06-05T09:00:00Z"
    }
  ],
  "inbox": [
    {"mailbox": "RubyElk", "unread_count": 1, "ack_required_count": 0}
  ],
  "threads": [
    {"thread_id": "bd-20453.6", "subject": "Environment attestation proof", "message_count": 2}
  ]
}
JSON

cat >"$HEALTH_AGENT_MAIL_SNAPSHOT" <<'JSON'
{
  "schema": "ee.swarm.coordination_health.v1",
  "healthLevel": "green",
  "mcp_http_reachable": false,
  "am_agents_list_ok": true,
  "am_send_single_recipient_ok": true,
  "am_send_multi_recipient_ok": false,
  "observed_panic": "RefCell already borrowed",
  "fallback_active": true
}
JSON

cat >"$LOCAL_CARGO_PROCESS_SCAN" <<'JSON'
{
  "schema": "ee.rch_local_cargo_tripwire.v1",
  "mode": "probe_processes",
  "status": "bypass_detected",
  "count": 1,
  "detectedLocalBuilds": [
    {"kind": "cargo", "pid": 4242, "command": "cargo test"}
  ],
  "evidence": [
    {"kind": "active_process_scan", "result": "bypass_detected"}
  ]
}
JSON

validate_attestation_case() {
    local label="$1"
    local case_filter="$2"
    local command_text="$3"

    if [ "$LAST_EXIT_CODE" -ne 0 ]; then
        if jq -e '.schema == "ee.error.v2" and (.error.message // "" | contains("unrecognized subcommand"))' "$LAST_STDOUT" >/dev/null 2>&1 ||
            grep -Fq "unrecognized subcommand 'environment-attestation'" "$LAST_STDOUT" "$LAST_STDERR"; then
            fail_with_artifacts \
                "environment_attestation_command_unavailable" \
                "$command_text" \
                "environment_attestation_command_unavailable_or_stale_binary" \
                3 \
                "$LAST_EXIT_CODE" \
                "$LAST_ELAPSED_MS" \
                "$LAST_STDOUT" \
                "$LAST_STDERR" \
                "not_run"
        fi
        fail_with_artifacts \
            "${label}_command_failed" \
            "$command_text" \
            "environment_attestation_command_failed" \
            1 \
            "$LAST_EXIT_CODE" \
            "$LAST_ELAPSED_MS" \
            "$LAST_STDOUT" \
            "$LAST_STDERR" \
            "failed"
    fi

    if ! jq -e '
      .schema == "ee.response.v2"
      and .success == true
      and .data.schema == "ee.environment_attestation.v1"
      and .data.redactionStatus == "counts_ids_statuses_path_patterns_command_templates_no_mail_body_no_file_content"
      and (.data.sourceAuthority | type == "array")
      and (.data.sourceAuthority | length) > 0
      and (.data.summary.safeToClaim | type == "boolean")
      and (.data.summary.environmentVerdict | type == "string")
      and (.data.summary.sourceTestVerdict | type == "string")
      and (.data.recoveryActions | type == "array")
      and (.data.evidenceRefs | type == "array")
    ' "$LAST_STDOUT" >/dev/null; then
        fail_with_artifacts \
            "${label}_schema_validation_failed" \
            "$command_text" \
            "environment_attestation_schema_validation_failed" \
            1 \
            1 \
            "$LAST_ELAPSED_MS" \
            "$LAST_STDOUT" \
            "$LAST_STDERR" \
            "failed"
    fi

    if ! jq -e "$case_filter" "$LAST_STDOUT" >/dev/null; then
        fail_with_artifacts \
            "${label}_case_validation_failed" \
            "$command_text" \
            "environment_attestation_case_validation_failed" \
            1 \
            1 \
            "$LAST_ELAPSED_MS" \
            "$LAST_STDOUT" \
            "$LAST_STDERR" \
            "failed"
    fi

    if [ -s "$LAST_STDERR" ]; then
        fail_with_artifacts \
            "${label}_stdout_stderr_contract_failed" \
            "$command_text" \
            "environment_attestation_stderr_not_empty_in_json_mode" \
            1 \
            1 \
            "$LAST_ELAPSED_MS" \
            "$LAST_STDOUT" \
            "$LAST_STDERR" \
            "passed"
    fi

    if grep -Eq '/Users/|/Volumes/|/data/projects/|/private/tmp/|SECRET_TOKEN|body_md|mail body|raw source' "$LAST_STDOUT"; then
        fail_with_artifacts \
            "${label}_redaction_failed" \
            "$command_text" \
            "environment_attestation_redaction_failed" \
            1 \
            1 \
            "$LAST_ELAPSED_MS" \
            "$LAST_STDOUT" \
            "$LAST_STDERR" \
            "passed" \
            "failed"
    fi

    local environment_verdict
    local source_test_verdict
    local degraded_codes
    environment_verdict="$(jq -r '.data.summary.environmentVerdict' "$LAST_STDOUT")"
    source_test_verdict="$(jq -r '.data.summary.sourceTestVerdict' "$LAST_STDOUT")"
    degraded_codes="$(jq -c '[.data.degraded[]?.code] | sort' "$LAST_STDOUT")"

    emit_assert_result \
        "assert_ok" \
        "$label" \
        "$command_text" \
        "none" \
        "passed" \
        "passed" \
        "$LAST_EXIT_CODE" \
        "$LAST_ELAPSED_MS" \
        "$LAST_STDOUT" \
        "$LAST_STDERR" \
        "$environment_verdict" \
        "$source_test_verdict" \
        "$degraded_codes" >/dev/null
}

run_attestation_case() {
    local label="$1"
    local case_filter="$2"
    local command_text="$3"
    shift 3
    run_ee "$label" "$@"
    validate_attestation_case "$label" "$case_filter" "$command_text"
}

DEFAULT_ARGS=(
    diag
    environment-attestation
    --workspace
    "$WORKSPACE"
    --include-rch
    --json
    --command-timeout-ms
    "$COMMAND_TIMEOUT_MS"
)
if [ -n "$SNAPSHOT_PATH" ]; then
    DEFAULT_ARGS+=(--agent-mail-snapshot "$SNAPSHOT_PATH")
fi

run_attestation_case \
    "environment-attestation-default" \
    '.data.sourceAuthority
      | any(.source == "local_cargo_tripwire" and .status == "ok")
      and any(.source == "source_tree")
      and any(.source == "rch" or .source == "build_admission")' \
    "ee diag environment-attestation --workspace [WORKSPACE] --include-rch --json" \
    "${DEFAULT_ARGS[@]}"

run_attestation_case \
    "environment-attestation-no-sources" \
    '.data.sourceAuthority
      | any(.source == "source_tree" and .status == "not_collected")
      and any(.source == "rch" and .status == "not_collected")
      and any(.source == "local_cargo_tripwire" and .status == "ok")
      and any(.source == "file_reservations" and .status == "ok")' \
    "ee diag environment-attestation --workspace [WORKSPACE] --sources none --json" \
    diag environment-attestation \
    --workspace "$WORKSPACE" \
    --sources none \
    --json \
    --command-timeout-ms "$COMMAND_TIMEOUT_MS"

run_attestation_case \
    "environment-attestation-agent-mail-conflict" \
    '.data.sourceAuthority
      | any(.source == "agent_mail_probe" and (.metrics | any(.name == "reservation_count" and .value == "1")))
      and any(.source == "file_reservations" and .status == "blocked")' \
    "ee diag environment-attestation --workspace [WORKSPACE] --sources agent-mail --agent-mail-snapshot [RUN]/artifacts/agent-mail-full-snapshot.json --json" \
    diag environment-attestation \
    --workspace "$WORKSPACE" \
    --sources agent-mail \
    --agent-mail-snapshot "$FULL_AGENT_MAIL_SNAPSHOT" \
    --json \
    --command-timeout-ms "$COMMAND_TIMEOUT_MS"

run_attestation_case \
    "environment-attestation-agent-mail-health" \
    '.data.sourceAuthority
      | any(.source == "agent_mail_probe" and (.degradedCodes | index("agent_mail_unavailable")))' \
    "ee diag environment-attestation --workspace [WORKSPACE] --sources agent-mail --agent-mail-snapshot [RUN]/artifacts/agent-mail-health-degraded.json --json" \
    diag environment-attestation \
    --workspace "$WORKSPACE" \
    --sources agent-mail \
    --agent-mail-snapshot "$HEALTH_AGENT_MAIL_SNAPSHOT" \
    --json \
    --command-timeout-ms "$COMMAND_TIMEOUT_MS"

run_attestation_case \
    "environment-attestation-ci-proof-stale" \
    '(.data.summary.environmentVerdict == "source_authority_ambiguous"
      or (.data.summary.environmentVerdict == "coordinate_before_claim"
          and (.data.degraded | any(.code == "dirty_checkout_observed"))))
      and .data.summary.sourceTestVerdict == "stale_source"
      and (.data.degraded | any(.code == "ci_proof_lane_artifact_stale"))
      and (.data.sourceAuthority | any(.source == "ci_proof_lane" and .status == "stale"))' \
    "ee diag environment-attestation --workspace [WORKSPACE] --sources git --ci-proof-lane-snapshot [REPO]/tests/fixtures/ci_proof_lane/artifact_stale.json --json" \
    diag environment-attestation \
    --workspace "$WORKSPACE" \
    --sources git \
    --ci-proof-lane-snapshot "$CI_STALE_PROOF_SNAPSHOT" \
    --json \
    --command-timeout-ms "$COMMAND_TIMEOUT_MS"

run_attestation_case \
    "environment-attestation-ci-proof-cancelled" \
    '(.data.summary.environmentVerdict == "source_authority_ambiguous"
      or (.data.summary.environmentVerdict == "coordinate_before_claim"
          and (.data.degraded | any(.code == "dirty_checkout_observed"))))
      and (.data.degraded | any(.code == "ci_proof_lane_cancelled_before_artifact"))
      and (.data.sourceAuthority | any(.source == "ci_proof_lane" and .status == "blocked"))' \
    "ee diag environment-attestation --workspace [WORKSPACE] --sources git --ci-proof-lane-snapshot [REPO]/tests/fixtures/ci_proof_lane/cancelled_before_artifact.json --json" \
    diag environment-attestation \
    --workspace "$WORKSPACE" \
    --sources git \
    --ci-proof-lane-snapshot "$CI_CANCELLED_PROOF_SNAPSHOT" \
    --json \
    --command-timeout-ms "$COMMAND_TIMEOUT_MS"

run_attestation_case \
    "environment-attestation-local-cargo-bypass-fixture" \
    '.data.summary.environmentVerdict == "local_cargo_bypass_detected"
      and .data.summary.localCargoFallbackObserved == true
      and (.data.degraded | any(.code == "local_cargo_bypass_detected"))
      and (.data.sourceAuthority | any(.source == "local_cargo_tripwire" and .status == "blocked"))' \
    "ee diag environment-attestation --workspace [WORKSPACE] --sources git --local-cargo-process-scan [RUN]/artifacts/local-cargo-bypass-scan.json --json" \
    diag environment-attestation \
    --workspace "$WORKSPACE" \
    --sources git \
    --local-cargo-process-scan "$LOCAL_CARGO_PROCESS_SCAN" \
    --json \
    --command-timeout-ms "$COMMAND_TIMEOUT_MS"

validate_event_log

printf 'Artifacts: %s\n' "$ARTIFACT_DIR" >&2
printf 'environment attestation E2E passed; events=%s\n' "$EVENT_LOG" >&2
