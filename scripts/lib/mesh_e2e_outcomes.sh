#!/usr/bin/env bash
# Shared outcome logging for mesh shell e2e drivers.

set -o pipefail

mesh_e2e_event_ts() {
  if [ -n "${MESH_E2E_EVENT_TS:-}" ]; then
    printf '%s\n' "$MESH_E2E_EVENT_TS"
  else
    python3 - <<'PY'
from datetime import datetime, timezone

print(datetime.now(timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z"))
PY
  fi
}

mesh_e2e_emit_event() {
  local kind="${1:?kind required}"
  local surface="${2:?surface required}"
  local scenario="${3:?scenario required}"
  local phase="${4:?phase required}"
  local status="${5:?status required}"
  local duration_ms="${6:-}"
  local stderr_tail="${7:-}"
  local command="${8:-}"
  local message="${9:-}"
  python3 - "$kind" "$surface" "$scenario" "$phase" "$status" "$duration_ms" "$stderr_tail" "$command" "$message" "$(mesh_e2e_event_ts)" <<'PY'
import json
import math
import sys

kind, surface, scenario, phase, status, duration_ms, stderr_tail, command, message, ts = sys.argv[1:]
test_id = f"{surface}.{scenario}"
label = f"{surface}:{scenario}:{phase}"
fields = {
    "label": label,
    "surface": surface,
    "phase": phase,
    "scenario": scenario,
    "status": status,
    "ok": status == "pass",
}
if duration_ms != "":
    try:
        duration = float(duration_ms)
    except ValueError:
        duration = 0.0
    # bd-18bue: Python's float() happily parses "inf", "Infinity",
    # "-inf", and "nan" — and json.dumps then emits them as the
    # non-strict JSON literals Infinity/-Infinity/NaN, which jq and
    # every strict JSON consumer (including the
    # mesh_outcome_helper_emits_schema_valid_event_shape contract
    # test) reject. The MESH_E2E_DURATION_MS_OVERRIDE escape hatch
    # makes those values reachable from agent harnesses. Coerce any
    # non-finite or negative duration to 0.0 so emitted events are
    # always strict JSON — matching the existing ValueError fallback
    # semantics rather than introducing a separate error path.
    if not math.isfinite(duration) or duration < 0.0:
        duration = 0.0
    fields["duration_ms"] = duration
if stderr_tail:
    fields["stderr_tail"] = stderr_tail
if command:
    fields["command"] = command
if message:
    fields["message"] = message
if kind == "assert_fail":
    fields["expected"] = "pass"
    fields["actual"] = status

event = {
    "schema": "ee.test_event.v1",
    "ts": ts,
    "test_id": test_id,
    "kind": kind,
    "fields": fields,
}
if duration_ms != "":
    event["elapsed_ms"] = fields.get("duration_ms", 0.0)
if stderr_tail:
    event["stderr_excerpt"] = stderr_tail
print(json.dumps(event, ensure_ascii=False, separators=(",", ":"), sort_keys=True))
PY
}

mesh_e2e_emit_note() {
  local surface="${1:?surface required}"
  local scenario="${2:?scenario required}"
  local message="${3:?message required}"
  local phase="${4:-setup}"
  mesh_e2e_emit_event "note" "$surface" "$scenario" "$phase" "note" "" "" "" "$message"
}

mesh_e2e_emit_scheduled() {
  local surface="${1:?surface required}"
  local scenario="${2:?scenario required}"
  local command="${3:-}"
  mesh_e2e_emit_event "note" "$surface" "$scenario" "setup" "scheduled" "" "" "$command" "scheduled"
}

mesh_e2e_emit_outcome() {
  local surface="${1:?surface required}"
  local scenario="${2:?scenario required}"
  local status="${3:?status required}"
  local duration_ms="${4:?duration_ms required}"
  local stderr_tail="${5:-}"
  local kind="note"
  case "$status" in
    pass) kind="assert_ok" ;;
    fail) kind="assert_fail" ;;
    skipped) kind="note" ;;
  esac
  mesh_e2e_emit_event "$kind" "$surface" "$scenario" "outcome" "$status" "$duration_ms" "$stderr_tail"
}

mesh_e2e_emit_outcomes() {
  local surface="${1:?surface required}"
  local status="${2:?status required}"
  local duration_ms="${3:?duration_ms required}"
  local stderr_tail="${4:-}"
  shift 4
  local scenario
  for scenario in "$@"; do
    mesh_e2e_emit_outcome "$surface" "$scenario" "$status" "$duration_ms" "$stderr_tail"
  done
}

mesh_e2e_emit_skipped() {
  local surface="${1:?surface required}"
  local message="${2:?message required}"
  shift 2
  printf '%s\n' "$message" >&2
  mesh_e2e_emit_outcomes "$surface" "skipped" "0.0" "$message" "$@"
}

mesh_e2e_emit_failed() {
  local surface="${1:?surface required}"
  local message="${2:?message required}"
  shift 2
  printf '%s\n' "$message" >&2
  mesh_e2e_emit_outcomes "$surface" "fail" "0.0" "$message" "$@"
}

mesh_e2e_stderr_tail_file() {
  local path="${1:?stderr path required}"
  local cap="${MESH_E2E_STDERR_TAIL_BYTES:-4096}"
  if [ -f "$path" ]; then
    tail -c "$cap" "$path"
  fi
}

mesh_e2e_monotonic_ns() {
  python3 -c 'import time; print(time.monotonic_ns())'
}

mesh_e2e_duration_ms() {
  local started="${1:?started ns required}"
  local ended="${2:?ended ns required}"
  python3 - "$started" "$ended" <<'PY'
import sys

started = int(sys.argv[1])
ended = int(sys.argv[2])
print((ended - started) / 1_000_000.0)
PY
}

mesh_e2e_run_with_outcomes() {
  local surface="${1:?surface required}"
  shift
  local scenarios=()
  while [ "$#" -gt 0 ] && [ "$1" != "--" ]; do
    scenarios+=("$1")
    shift
  done
  if [ "$#" -eq 0 ]; then
    mesh_e2e_emit_failed "$surface" "mesh e2e outcome helper missing command separator" "${scenarios[@]}"
    return 2
  fi
  shift
  if [ "$#" -eq 0 ]; then
    mesh_e2e_emit_failed "$surface" "mesh e2e outcome helper missing command" "${scenarios[@]}"
    return 2
  fi

  local stderr_file started ended duration_ms rc status stderr_tail
  stderr_file="$(mktemp "${TMPDIR:-/tmp}/ee-mesh-e2e-stderr.XXXXXX")"
  started="$(mesh_e2e_monotonic_ns)"
  if "$@" 2> >(tee "$stderr_file" >&2); then
    rc=0
    status="pass"
  else
    rc=$?
    status="fail"
  fi
  ended="$(mesh_e2e_monotonic_ns)"
  duration_ms="$(mesh_e2e_duration_ms "$started" "$ended")"
  if [ -n "${MESH_E2E_DURATION_MS_OVERRIDE:-}" ]; then
    duration_ms="$MESH_E2E_DURATION_MS_OVERRIDE"
  fi
  stderr_tail="$(mesh_e2e_stderr_tail_file "$stderr_file")"
  mesh_e2e_emit_outcomes "$surface" "$status" "$duration_ms" "$stderr_tail" "${scenarios[@]}"
  return "$rc"
}
