#!/usr/bin/env bash
# Shared outcome logging for mesh shell e2e drivers.

set -o pipefail

mesh_e2e_emit_scheduled() {
  local surface="${1:?surface required}"
  local scenario="${2:?scenario required}"
  local command="${3:-}"
  python3 - "$surface" "$scenario" "$command" <<'PY'
import json
import sys

surface, scenario, command = sys.argv[1:]
event = {
    "schema": "ee.test_event.v1",
    "surface": surface,
    "phase": "assert",
    "scenario": scenario,
    "stage": "scheduled",
}
if command:
    event["command"] = command
print(json.dumps(event, ensure_ascii=False, separators=(",", ":")))
PY
}

mesh_e2e_emit_outcome() {
  local surface="${1:?surface required}"
  local scenario="${2:?scenario required}"
  local status="${3:?status required}"
  local duration_ms="${4:?duration_ms required}"
  local stderr_tail="${5:-}"
  python3 - "$surface" "$scenario" "$status" "$duration_ms" "$stderr_tail" <<'PY'
import json
import sys

surface, scenario, status, duration_ms, stderr_tail = sys.argv[1:]
try:
    duration = float(duration_ms)
except ValueError:
    duration = 0.0
event = {
    "schema": "ee.test_event.v1",
    "surface": surface,
    "phase": "outcome",
    "scenario": scenario,
    "status": status,
    "ok": status == "pass",
    "duration_ms": duration,
    "stderr_tail": stderr_tail,
}
print(json.dumps(event, ensure_ascii=False, separators=(",", ":")))
PY
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
  stderr_tail="$(mesh_e2e_stderr_tail_file "$stderr_file")"
  mesh_e2e_emit_outcomes "$surface" "$status" "$duration_ms" "$stderr_tail" "${scenarios[@]}"
  return "$rc"
}
