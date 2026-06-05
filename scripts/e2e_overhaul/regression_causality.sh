#!/usr/bin/env bash
# bd-391ze.6 — regression causality no-mock e2e driver.
#
# Exercises the real `ee regress explain` CLI against structured JSON
# artifacts. The script does not build Rust, mutate Beads, send mail, create
# worktrees, or clean up retained evidence.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq

if [ -z "${EE_TEST_LOG_PATH:-}" ]; then
    export EE_TEST_LOG_PATH="${TMPDIR:-/tmp}/ee-regression-causality-${BASHPID:-$$}.jsonl"
fi
: >"$EE_TEST_LOG_PATH"

epic_setup "regression_causality"

ARTIFACT_DIR="$EPIC_WORKSPACE/regression-causality"
mkdir -p "$ARTIFACT_DIR"

COMMAND_CHECK_STDOUT="$ARTIFACT_DIR/regress_explain_help.stdout.txt"
COMMAND_CHECK_STDERR="$ARTIFACT_DIR/regress_explain_help.stderr.txt"
VERIFICATION_EVIDENCE="$ARTIFACT_DIR/verification_evidence.json"
BV_HISTORY="$ARTIFACT_DIR/bv_history.json"
PERF_REPORT="$ARTIFACT_DIR/perf_report.json"
GIT_METADATA="$ARTIFACT_DIR/git_metadata.json"

cat >"$VERIFICATION_EVIDENCE" <<'JSON'
{
  "schema": "ee.verification_evidence.v1",
  "status": "rch_environment_failure",
  "commandHash": "blake3:bd391ze6command",
  "observedAt": "2026-06-05T08:00:00Z",
  "sourceState": {
    "sourceHash": "blake3:bd391ze6source",
    "materialization": "remote_checkout_unverified",
    "remoteSourceMaterialized": false,
    "degradedCodes": [
      "rch_verify_remote_marker_missing",
      "rch_verify_local_fallback_refused"
    ]
  },
  "selectorAdmission": {
    "localFallbackRefused": true,
    "degradedCodes": [
      "rch_verify_remote_command_failed"
    ]
  },
  "stderr": "raw stderr with secret token should be suppressed; see /Users/jemanuel/private/proof.log",
  "redactionStatus": "safe"
}
JSON

cat >"$BV_HISTORY" <<'JSON'
{
  "schema": "ee.bv.history.v1",
  "status": "stale",
  "observedAt": "2026-06-05T08:01:00Z",
  "degraded": [
    {"code": "tracker_state_mismatch"},
    {"code": "bv_blocked_issue_claim_command"}
  ],
  "summary": {
    "issueId": "bd-37ugy",
    "brStatus": "blocked",
    "bvRecommendation": "claim_command_emitted"
  },
  "redactionStatus": "safe"
}
JSON

cat >"$PERF_REPORT" <<'JSON'
{
  "schema": "ee.perf.v1",
  "status": "budget_exceeded",
  "observedAt": "2026-06-05T08:02:00Z",
  "artifactHash": "blake3:bd391ze6perf",
  "metrics": {
    "p95Ms": 1250,
    "budgetMs": 800
  },
  "degradedCodes": [
    "perf_latency_budget_exceeded"
  ],
  "redactionStatus": "safe"
}
JSON

cat >"$GIT_METADATA" <<'JSON'
{
  "schema": "ee.git_metadata.v1",
  "status": "available",
  "gitTree": "blake3:bd391ze6tree",
  "observedAt": "2026-06-05T08:03:00Z",
  "redactionStatus": "safe"
}
JSON

now_ms() {
    python3 - <<'PY'
import time
print(int(time.time() * 1000))
PY
}

emit_case_event() {
    local case_name="${1:?case name required}"
    local status="${2:?status required}"
    local exit_code="${3:?exit code required}"
    local elapsed_ms="${4:?elapsed ms required}"
    local stdout_path="${5:?stdout path required}"
    local stderr_path="${6:?stderr path required}"
    local first_failure_diagnosis="${7:-}"

    _e2e_emit_event "regression_causality_case" \
        "case" "$case_name" \
        "status" "$status" \
        "exit_code" "$exit_code" \
        "elapsed_ms" "$elapsed_ms" \
        "stdout_path" "$stdout_path" \
        "stderr_path" "$stderr_path" \
        "first_failure_diagnosis" "$first_failure_diagnosis"
}

if ! "$EE_BINARY" regress explain --help >"$COMMAND_CHECK_STDOUT" 2>"$COMMAND_CHECK_STDERR"; then
    emit_case_event "command_availability" "blocked" "3" "0" \
        "$COMMAND_CHECK_STDOUT" "$COMMAND_CHECK_STDERR" \
        "regression_causality_command_unavailable_or_stale_binary"
    e2e_log_note "regression_causality_command_unavailable_or_stale_binary binary=$EE_BINARY stdout=$COMMAND_CHECK_STDOUT stderr=$COMMAND_CHECK_STDERR"
    exit 3
fi

run_regress_case() {
    local case_name="${1:?case name required}"
    local surface="${2:?surface required}"
    shift 2

    local stdout_path="$ARTIFACT_DIR/${case_name}.stdout.json"
    local stderr_path="$ARTIFACT_DIR/${case_name}.stderr.txt"
    local started ended elapsed_ms exit_code status diagnosis
    started="$(now_ms)"
    "$EE_BINARY" \
        --workspace "$EPIC_WORKSPACE" \
        --json \
        regress explain \
        "$@" \
        --surface "$surface" \
        --workspace-hash "blake3:bd391ze6workspace" \
        >"$stdout_path" 2>"$stderr_path"
    exit_code=$?
    ended="$(now_ms)"
    elapsed_ms=$((ended - started))
    if [ "$exit_code" -eq 0 ] && jq -e '.schema == "ee.response.v2" and .success == true' "$stdout_path" >/dev/null 2>&1; then
        status="ok"
        diagnosis=""
    else
        status="failed"
        diagnosis="$(head -c 240 "$stderr_path" | tr '\n' ' ')"
        if [ -z "$diagnosis" ]; then
            diagnosis="regress explain did not emit a successful ee.response.v2 payload"
        fi
    fi
    emit_case_event "$case_name" "$status" "$exit_code" "$elapsed_ms" \
        "$stdout_path" "$stderr_path" "$diagnosis"
    e2e_log_assert_eq "$exit_code" "0" "${case_name}_exit_zero"
    printf '%s\n' "$stdout_path"
}

assert_file_jq() {
    local path="${1:?path required}"
    local filter="${2:?jq filter required}"
    local want="${3:?expected value required}"
    local label="${4:?label required}"
    local got
    got="$(jq -r "$filter" "$path" 2>/dev/null || true)"
    e2e_log_assert_eq "$got" "$want" "$label"
}

assert_file_jq_nonempty() {
    local path="${1:?path required}"
    local filter="${2:?jq filter required}"
    local label="${3:?label required}"
    local got
    got="$(jq -r "$filter" "$path" 2>/dev/null || true)"
    if [ -z "$got" ] || [ "$got" = "null" ]; then
        e2e_log_assert_eq "<empty>" "non-empty" "$label"
    else
        e2e_log_assert_eq "non-empty" "non-empty" "$label"
    fi
}

assert_jsonl_jq() {
    local path="${1:?path required}"
    local filter="${2:?jq filter required}"
    local want="${3:?expected value required}"
    local label="${4:?label required}"
    local got
    got="$(jq -s -r "$filter" "$path" 2>/dev/null || true)"
    e2e_log_assert_eq "$got" "$want" "$label"
}

assert_file_excludes() {
    local path="${1:?path required}"
    local forbidden="${2:?forbidden text required}"
    local label="${3:?label required}"
    if grep -Fq "$forbidden" "$path"; then
        e2e_log_assert_eq "present" "absent" "$label"
    else
        e2e_log_assert_eq "absent" "absent" "$label"
    fi
}

MULTI_CASE_STDOUT="$(run_regress_case multi_cause verification_gate \
    --from "verification_evidence=$VERIFICATION_EVIDENCE" \
    --from "bv_history=$BV_HISTORY" \
    --from "perf_report=$PERF_REPORT")"

assert_file_jq "$MULTI_CASE_STDOUT" '.data.schema' \
    "ee.regression_causality.v1" "regression_multi_capsule_schema"
assert_file_jq "$MULTI_CASE_STDOUT" '.data.subject.surface' \
    "verification_gate" "regression_multi_surface"
assert_file_jq "$MULTI_CASE_STDOUT" '.data.hypotheses[0].code' \
    "source_not_materialized" "regression_multi_top_source_not_materialized"
assert_file_jq "$MULTI_CASE_STDOUT" \
    'any(.data.hypotheses[]?; .code == "known_environment_blocker")' \
    "true" "regression_multi_environment_hypothesis"
assert_file_jq "$MULTI_CASE_STDOUT" \
    'any(.data.hypotheses[]?; .code == "tracker_state_mismatch")' \
    "true" "regression_multi_tracker_hypothesis"
assert_file_jq "$MULTI_CASE_STDOUT" \
    'any(.data.hypotheses[]?; .code == "perf_budget_regression")' \
    "true" "regression_multi_perf_hypothesis"
assert_file_jq "$MULTI_CASE_STDOUT" \
    'all(.data.hypotheses[]?; .authoritative == false)' \
    "true" "regression_multi_hypotheses_non_authoritative"
assert_file_jq "$MULTI_CASE_STDOUT" \
    'all(.data.nextCommands[]?; .mutatesWorkspace == false)' \
    "true" "regression_multi_next_commands_read_only"
assert_file_jq "$MULTI_CASE_STDOUT" \
    'any(.data.nextCommands[]?; .requiresRch == true)' \
    "true" "regression_multi_preserves_rch_command"
assert_file_jq "$MULTI_CASE_STDOUT" '.data.redaction.rawLogsPresent' \
    "false" "regression_multi_raw_logs_absent"
assert_file_jq "$MULTI_CASE_STDOUT" '.data.redaction.rawMailBodiesPresent' \
    "false" "regression_multi_raw_mail_absent"
assert_file_jq "$MULTI_CASE_STDOUT" '.data.redaction.rawMemoryBodiesPresent' \
    "false" "regression_multi_raw_memory_absent"
assert_file_jq "$MULTI_CASE_STDOUT" '.data.redaction.privatePathsPresent' \
    "false" "regression_multi_private_paths_absent"
assert_file_jq "$MULTI_CASE_STDOUT" '.data.redaction.secretScanApplied' \
    "true" "regression_multi_secret_scan_applied"
assert_file_jq "$MULTI_CASE_STDOUT" \
    'any(.data.degraded[]?; .code == "regression_evidence_raw_output_suppressed")' \
    "true" "regression_multi_raw_output_suppressed_degraded"
assert_file_jq "$MULTI_CASE_STDOUT" \
    'any(.data.degraded[]?; .code == "regression_evidence_private_path_redacted")' \
    "true" "regression_multi_private_path_degraded"
assert_file_jq_nonempty "$MULTI_CASE_STDOUT" \
    '.data.evidenceSources[0].artifactHash | select(startswith("blake3:"))' \
    "regression_multi_artifact_hash_present"
assert_file_excludes "$MULTI_CASE_STDOUT" "/Users/jemanuel/private/proof.log" \
    "regression_multi_no_private_path_leak"
assert_file_excludes "$MULTI_CASE_STDOUT" "secret token should be suppressed" \
    "regression_multi_no_raw_stderr_leak"

UNKNOWN_CASE_STDOUT="$(run_regress_case insufficient_evidence unknown \
    --from "git_metadata=$GIT_METADATA")"

assert_file_jq "$UNKNOWN_CASE_STDOUT" '.data.schema' \
    "ee.regression_causality.v1" "regression_unknown_capsule_schema"
assert_file_jq "$UNKNOWN_CASE_STDOUT" '.data.hypotheses[0].code' \
    "unknown_insufficient_evidence" "regression_unknown_abstains"
assert_file_jq "$UNKNOWN_CASE_STDOUT" \
    'any(.data.degraded[]?; .code == "regression_hypothesis_missing_required_source")' \
    "true" "regression_unknown_missing_required_source_degraded"
assert_file_jq "$UNKNOWN_CASE_STDOUT" \
    'all(.data.nextCommands[]?; .mutatesWorkspace == false)' \
    "true" "regression_unknown_next_commands_read_only"

assert_jsonl_jq "$EE_TEST_LOG_PATH" \
    '[select(.kind == "regression_causality_case")] | length' \
    "2" "regression_case_event_count"
assert_jsonl_jq "$EE_TEST_LOG_PATH" \
    'all(select(.kind == "regression_causality_case"); (.fields.stdout_path | length) > 0 and (.fields.stderr_path | length) > 0 and (.exit_code == 0))' \
    "true" "regression_case_events_have_artifacts"

if [ "$EE_TEST_LOG_ASSERTS_FAIL" -ne 0 ]; then
    exit 1
fi
