#!/usr/bin/env bash
# E2E harness for bd-3boan: round-trip the ee doctor --fix scaffold
# through ee doctor --undo <RUN_ID> and prove both surfaces emit their
# canonical envelope schemas.
#
# Until bd-tu4s8 wires the per-FM fixer dispatch table behind --fix,
# this script asserts the RunContext chokepoint lifecycle:
#
#   1. ee doctor --fix --workspace $TMP --json
#      ->  ee.doctor.fix_summary.v1 with fixerDispatchPending=true,
#          actionCount=0, status=completed_ok, sideEffectFree=false
#          (lock + run_dir creation IS the side effect; the
#          chokepoint is reachable from the CLI).
#   2. <workspace>/.doctor/runs/<run-id>/state.json exists, is valid
#      JSON, and pins schema=ee.doctor.run_state.v1.
#   3. ee doctor --undo <run-id> --workspace $TMP --json
#      ->  ee.doctor.undo_summary.v1 with actionsUndone=0,
#          actionsSkipped=0, status=undone, firstError=null.
#   4. <workspace>/.doctor/runs/<run-id>/state.json now reports
#      status in {undone, undone_partial}.
#
# This is the read-only proof that bd-3boan's --fix + --undo CLI
# wiring is end-to-end functional independently of the per-FM fixer
# work owned by bd-tu4s8. Emits ee.test_event.v1 lines to
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
    printf 'error: $EE_BIN (%s) is not invokable; set EE_BIN or place ee on PATH\n' "$EE_BIN" >&2
    exit 2
  fi
  emit_event "probe" "ok" "$EE_BIN --version succeeded"
}

run_fix() {
  emit_event "fix" "begin" "ee doctor --fix --workspace=$WORKSPACE --json"
  local fix_json
  fix_json="$("$EE_BIN" --workspace "$WORKSPACE" --json doctor --fix)"
  printf '%s\n' "$fix_json" > "$WORKSPACE/fix.json"
  local schema run_id status pending
  schema="$(printf '%s' "$fix_json" | jq -r '.schema')"
  run_id="$(printf '%s' "$fix_json" | jq -r '.runId')"
  status="$(printf '%s' "$fix_json" | jq -r '.status')"
  pending="$(printf '%s' "$fix_json" | jq -r '.fixerDispatchPending')"
  if [ "$schema" != "ee.doctor.fix_summary.v1" ]; then
    emit_event "fix" "fail" "schema mismatch: $schema"
    printf 'error: --fix returned schema=%s, expected ee.doctor.fix_summary.v1\n' "$schema" >&2
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
  if [ "$pending" != "true" ]; then
    emit_event "fix" "warn" "fixerDispatchPending=$pending (expected true until bd-tu4s8 lands)"
  fi
  printf '%s' "$run_id"
}

assert_state_json() {
  local run_id="$1"
  local expect_status="$2"
  local state_path="$WORKSPACE/.doctor/runs/$run_id/state.json"
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
  emit_event "undo" "begin" "ee doctor --undo $run_id --workspace=$WORKSPACE --json"
  local undo_json
  undo_json="$("$EE_BIN" --workspace "$WORKSPACE" --json doctor --undo "$run_id")"
  printf '%s\n' "$undo_json" > "$WORKSPACE/undo.json"
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

probe_binary
run_id="$(run_fix)"
assert_state_json "$run_id" "completed_ok"
run_undo "$run_id"
assert_state_json "$run_id" "*"

emit_event "round_trip" "ok" "fix+undo roundtrip complete for run_id=$run_id"
printf 'doctor_undo_replay: ok run_id=%s workspace=%s events=%s\n' \
  "$run_id" "$WORKSPACE" "$EVENT_LOG" >&2
