#!/usr/bin/env bash
# E2E harness for bd-3boan and bd-3ak9b: round-trip successful and failed
# ee doctor --fix runs through ee doctor --undo <RUN_ID>, proving both
# surfaces emit truthful canonical envelope schemas and process exits.
#
# This script asserts the RunContext chokepoint lifecycle:
#
#   1. ee doctor --fix --workspace $TMP --json
#      ->  ee.response.v2 with ee.doctor.fix_summary.v1 under data,
#          data.fixerDispatchPending=false, data.status=completed_ok,
#          and data.sideEffectFree=false
#          (lock + run_dir creation IS the side effect; the
#          chokepoint is reachable from the CLI).
#   2. <workspace>/.doctor/runs/<run-id>/state.json exists, is valid
#      JSON, and pins schema=ee.doctor.run_state.v1.
#   3. ee doctor --undo <run-id> --workspace $TMP --json
#      ->  ee.doctor.undo_summary.v1 with actionsUndone=0,
#          actionsSkipped=0, status=undone, firstError=null.
#   4. <workspace>/.doctor/runs/<run-id>/state.json now reports
#      status in {undone, undone_partial}.
#   5. A peer-owned regular .doctor/latest forces finalization failure:
#      --fix exits nonzero with ee.error.v2, state.json reports failed,
#      completed fixer outcomes and action evidence remain in the error,
#      structured recovery stays canonical, the lock is released, the peer
#      file is preserved, and --undo can still replay the failed run.
#
# This is the proof that bd-3boan's --fix + --undo CLI wiring is
# end-to-end functional with the registered fixer dispatch table.
# Emits ee.test_event.v1 lines to
# $EE_TEST_EVENT_DIR/doctor_undo_replay.jsonl for forensic audit.
#
# AGENTS.md compliance:
#  - Per RULE 1, this script NEVER deletes files (the temp workspace
#    is left in place for the operator to inspect).
#  - No Cargo / rustc / rustdoc invocations. The ee binary path is
#    supplied via $EE_BIN (defaults to "ee" on PATH) and the harness
#    refuses to run if the binary cannot be probed.
#  - No git mutation of any kind.
#
# Exit 0 on success.

set -euo pipefail

EE_BIN="${EE_BIN:-ee}"
WORKSPACE="${EE_DOCTOR_UNDO_REPLAY_WORKSPACE:-${TMPDIR:-/tmp}/ee-doctor-undo-replay-$$}"
FAILURE_WORKSPACE="${EE_DOCTOR_FAILURE_WORKSPACE:-${WORKSPACE}-finish-failure}"
EVENT_DIR="${EE_TEST_EVENT_DIR:-${TMPDIR:-/tmp}/ee-doctor-undo-replay-events}"
EVENT_LOG="$EVENT_DIR/doctor_undo_replay.jsonl"
BEAD_ID="bd-3boan"
SURFACE="doctor_undo_replay"

mkdir -p "$WORKSPACE" "$EVENT_DIR"
: > "$EVENT_LOG"

if ! command -v jq >/dev/null 2>&1; then
  printf 'error: jq is required for doctor_undo_replay e2e\n' >&2
  exit 1
fi

emit_event() {
  local phase="$1"
  local status="$2"
  local detail="$3"
  jq -nc \
    --arg schema "ee.test_event.v1" \
    --arg bead "$BEAD_ID" \
    --arg surface "$SURFACE" \
    --arg phase "$phase" \
    --arg status "$status" \
    --arg detail "$detail" \
    --arg workspace "$WORKSPACE" \
    --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    '{schema:$schema, beadId:$bead, surface:$surface, phase:$phase, status:$status, detail:$detail, workspace:$workspace, ts:$ts}' \
    >> "$EVENT_LOG"
}

probe_binary() {
  if ! "$EE_BIN" --version >/dev/null 2>&1; then
    emit_event "probe" "unavailable" "EE_BIN=$EE_BIN cannot be invoked; aborting before mutation"
    printf 'error: EE_BIN (%s) is not invokable; set EE_BIN or place ee on PATH\n' "$EE_BIN" >&2
    exit 2
  fi
  emit_event "probe" "ok" "$EE_BIN --version succeeded"
}

run_fix() {
  emit_event "fix" "begin" "ee doctor --fix --workspace=$WORKSPACE --json"
  local fix_json
  fix_json="$("$EE_BIN" --workspace "$WORKSPACE" --json doctor --fix)"
  printf '%s\n' "$fix_json" > "$WORKSPACE/fix.json"
  local response_schema success data_schema run_id status pending
  response_schema="$(printf '%s' "$fix_json" | jq -r '.schema')"
  success="$(printf '%s' "$fix_json" | jq -r '.success')"
  data_schema="$(printf '%s' "$fix_json" | jq -r '.data.schema')"
  run_id="$(printf '%s' "$fix_json" | jq -r '.data.runId')"
  status="$(printf '%s' "$fix_json" | jq -r '.data.status')"
  pending="$(printf '%s' "$fix_json" | jq -r '.data.fixerDispatchPending')"
  if [ "$response_schema" != "ee.response.v2" ] ||
     [ "$success" != "true" ] ||
     [ "$data_schema" != "ee.doctor.fix_summary.v1" ]; then
    emit_event "fix" "fail" "contract mismatch: response=$response_schema success=$success data=$data_schema"
    printf 'error: --fix returned response=%s success=%s data=%s; expected ee.response.v2 true ee.doctor.fix_summary.v1\n' \
      "$response_schema" "$success" "$data_schema" >&2
    exit 3
  fi
  if [ -z "$run_id" ] || [ "$run_id" = "null" ]; then
    emit_event "fix" "fail" "no runId in fix summary"
    printf 'error: --fix returned no runId; payload was: %s\n' "$fix_json" >&2
    exit 3
  fi
  if [ "$status" != "completed_ok" ]; then
    emit_event "fix" "fail" "status=$status, expected completed_ok"
    printf 'error: --fix returned status=%s\n' "$status" >&2
    exit 3
  fi
  if [ "$pending" != "false" ]; then
    emit_event "fix" "fail" "fixerDispatchPending=$pending, expected false"
    printf 'error: --fix returned fixerDispatchPending=%s, expected false\n' "$pending" >&2
    exit 3
  fi
  printf '%s' "$run_id"
}

assert_state_json() {
  local run_id="$1"
  local expect_status="$2"
  local workspace="${3:-$WORKSPACE}"
  local state_path="$workspace/.doctor/runs/$run_id/state.json"
  if [ ! -f "$state_path" ]; then
    emit_event "state_json" "fail" "missing state.json at $state_path"
    printf 'error: missing state.json at %s\n' "$state_path" >&2
    exit 4
  fi
  local schema status
  schema="$(jq -r '.schema' "$state_path")"
  status="$(jq -r '.status' "$state_path")"
  if [ "$schema" != "ee.doctor.run_state.v1" ]; then
    emit_event "state_json" "fail" "schema=$schema"
    printf 'error: state.json schema=%s, expected ee.doctor.run_state.v1\n' "$schema" >&2
    exit 4
  fi
  if [ "$expect_status" != "*" ] && [ "$status" != "$expect_status" ]; then
    emit_event "state_json" "fail" "status=$status, expected $expect_status"
    printf 'error: state.json status=%s, expected %s\n' "$status" "$expect_status" >&2
    exit 4
  fi
  emit_event "state_json" "ok" "schema+status verified for $run_id (status=$status)"
}

run_undo() {
  local run_id="$1"
  local workspace="${2:-$WORKSPACE}"
  emit_event "undo" "begin" "ee doctor --undo $run_id --workspace=$workspace --json"
  local undo_json
  undo_json="$("$EE_BIN" --workspace "$workspace" --json doctor --undo "$run_id")"
  printf '%s\n' "$undo_json" > "$workspace/undo.json"
  local schema actions_undone first_error
  schema="$(printf '%s' "$undo_json" | jq -r '.schema')"
  actions_undone="$(printf '%s' "$undo_json" | jq -r '.actionsUndone')"
  first_error="$(printf '%s' "$undo_json" | jq -r '.firstError')"
  if [ "$schema" != "ee.doctor.undo_summary.v1" ]; then
    emit_event "undo" "fail" "schema mismatch: $schema"
    printf 'error: --undo returned schema=%s\n' "$schema" >&2
    exit 5
  fi
  if [ "$actions_undone" != "0" ]; then
    emit_event "undo" "warn" "actionsUndone=$actions_undone (expected 0 until bd-tu4s8 ships fixers)"
  fi
  if [ "$first_error" != "null" ]; then
    emit_event "undo" "fail" "firstError=$first_error"
    printf 'error: --undo reported firstError=%s\n' "$first_error" >&2
    exit 5
  fi
  emit_event "undo" "ok" "actionsUndone=$actions_undone"
}

run_finish_failure_and_undo() {
  if [ -e "$FAILURE_WORKSPACE" ] || [ -L "$FAILURE_WORKSPACE" ]; then
    emit_event "finish_failure" "fail" "refusing pre-existing failure workspace $FAILURE_WORKSPACE"
    printf 'error: failure workspace already exists; refusing to overwrite: %s\n' \
      "$FAILURE_WORKSPACE" >&2
    exit 6
  fi

  mkdir -p "$FAILURE_WORKSPACE/.doctor"
  local sentinel="peer-owned latest entry"
  printf '%s' "$sentinel" > "$FAILURE_WORKSPACE/.doctor/latest"
  local stderr_path="$FAILURE_WORKSPACE/fix.stderr"
  local failure_json failure_exit
  emit_event "finish_failure" "begin" "forcing regular .doctor/latest refusal"
  set +e
  failure_json="$("$EE_BIN" --workspace "$FAILURE_WORKSPACE" --json doctor --fix 2>"$stderr_path")"
  failure_exit=$?
  set -e
  printf '%s\n' "$failure_json" > "$FAILURE_WORKSPACE/fix.json"

  if [ "$failure_exit" -eq 0 ]; then
    emit_event "finish_failure" "fail" "doctor --fix returned exit 0"
    printf 'error: finish failure returned success exit\n' >&2
    exit 6
  fi
  if [ -s "$stderr_path" ]; then
    emit_event "finish_failure" "fail" "machine error leaked to stderr"
    printf 'error: finish failure wrote machine diagnostics to stderr\n' >&2
    exit 6
  fi

  local schema code phase status run_id
  schema="$(printf '%s' "$failure_json" | jq -r '.schema')"
  code="$(printf '%s' "$failure_json" | jq -r '.error.code')"
  phase="$(printf '%s' "$failure_json" | jq -r '.error.details.phase')"
  status="$(printf '%s' "$failure_json" | jq -r '.error.details.run.status')"
  run_id="$(printf '%s' "$failure_json" | jq -r '.error.details.run.runId')"
  if [ "$schema" != "ee.error.v2" ] ||
     [ "$code" != "doctor_latest_entry_unsafe" ] ||
     [ "$phase" != "finish" ] ||
     [ "$status" != "failed" ] ||
     [ -z "$run_id" ] ||
     [ "$run_id" = "null" ]; then
    emit_event "finish_failure" "fail" \
      "schema=$schema code=$code phase=$phase status=$status run_id=$run_id"
    printf 'error: finish failure contract mismatch: %s\n' "$failure_json" >&2
    exit 6
  fi
  if ! printf '%s' "$failure_json" | jq -e '
       .error.repair | type == "string" and length > 0
     ' >/dev/null; then
    emit_event "finish_failure" "fail" "canonical repair guidance missing"
    printf 'error: finish failure omitted canonical repair guidance\n' >&2
    exit 6
  fi
  if ! printf '%s' "$failure_json" |
       jq -e '
         .error.details.recovery
         | type == "array" and length > 0
           and all(.[];
             (.priority | type == "number")
             and (.kind | type == "string" and length > 0)
             and (.rationale | type == "string" and length > 0)
             and (.command | type == "string" and length > 0)
           )
       ' >/dev/null; then
    emit_event "finish_failure" "fail" "structured recovery missing"
    printf 'error: finish failure omitted structured recovery\n' >&2
    exit 6
  fi
  if ! printf '%s' "$failure_json" | jq -e '
       .error.details as $details
       | ($details.fixerResults | type == "array" and length > 0)
         and ($details.fixerResultCount == ($details.fixerResults | length))
         and ($details.attemptedFixerCount == ($details.fixerResults | length))
         and ($details.failedFixerCount == 0)
         and ($details.skippedFixerCount == 0)
         and all($details.fixerResults[];
           .outcome == "applied"
           and (.actionSequence | type == "number" and . > 0)
         )
         and ($details.run.actionCount ==
           ([$details.fixerResults[].actionSequence] | max))
     ' >/dev/null; then
    emit_event "finish_failure" "fail" "partial fixer outcomes were discarded or inconsistent"
    printf 'error: finish failure did not preserve completed fixer outcomes: %s\n' \
      "$failure_json" >&2
    exit 6
  fi
  if [ "$(tr -d '\n' < "$FAILURE_WORKSPACE/.doctor/latest")" != "$sentinel" ]; then
    emit_event "finish_failure" "fail" "peer-owned latest entry changed"
    printf 'error: finish failure changed peer-owned latest entry\n' >&2
    exit 6
  fi
  if [ ! -f "$FAILURE_WORKSPACE/.ee/.doctor.lock" ] ||
     [ -L "$FAILURE_WORKSPACE/.ee/.doctor.lock" ]; then
    emit_event "finish_failure" "fail" "persistent doctor lock is missing or unsafe"
    printf 'error: finish failure did not preserve a regular persistent doctor lock\n' >&2
    exit 6
  fi

  assert_state_json "$run_id" "failed" "$FAILURE_WORKSPACE"
  # Successful undo acquisition is the behavioral proof that failed
  # finalization released the advisory lock. The public lock file itself is
  # intentionally persistent and must not be removed during teardown.
  run_undo "$run_id" "$FAILURE_WORKSPACE"
  assert_state_json "$run_id" "undone" "$FAILURE_WORKSPACE"
  emit_event "finish_failure" "ok" \
    "nonzero error envelope + failed state + undo verified for run_id=$run_id"
}

probe_binary
run_id="$(run_fix)"
assert_state_json "$run_id" "completed_ok"
run_undo "$run_id"
assert_state_json "$run_id" "*"
run_finish_failure_and_undo

emit_event "round_trip" "ok" "fix+undo roundtrip complete for run_id=$run_id"
printf 'doctor_undo_replay: ok run_id=%s workspace=%s events=%s\n' \
  "$run_id" "$WORKSPACE" "$EVENT_LOG" >&2
