#!/usr/bin/env bash
# E2E smoke for br_retry.sh transient Beads JSONL parse recovery.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

ts="$(date -u +%Y%m%dT%H%M%SZ)"
artifact_root="${EE_BR_RACE_ARTIFACT_ROOT:-/tmp/ee_e2e_br_race_${ts}_$$}"
fake_bin="$artifact_root/bin"
state_file="$artifact_root/fake_br_state"
stdout_path="$artifact_root/stdout.json"
stderr_path="$artifact_root/stderr.jsonl"
summary_path="$artifact_root/summary.jsonl"
request_id="bd-3usjw.73-e2e"
workspace_id="br-race-fixture"

mkdir -p "$fake_bin"

cat >"$fake_bin/br" <<'BR'
#!/usr/bin/env bash
set -euo pipefail
state_file="${EE_FAKE_BR_STATE:?EE_FAKE_BR_STATE required}"
mode="${EE_FAKE_BR_MODE:-transient}"
count=0
if [ -f "$state_file" ]; then
    count="$(cat "$state_file")"
fi
count=$((count + 1))
printf '%s\n' "$count" >"$state_file"

case "$mode" in
    transient)
        if [ "$count" -eq 1 ]; then
            printf 'Configuration error: Invalid JSON at line 2318: invalid type: integer `7`, expected struct Issue\n' >&2
            exit 2
        fi
        printf '{"schema":"br.ready.v1","workspace_id":"%s","request_id":"%s","issues":[],"attempt":%s}\n' "${EE_FAKE_BR_WORKSPACE_ID:-br-race-fixture}" "${EE_FAKE_BR_REQUEST_ID:-bd-3usjw.73-e2e}" "$count"
        ;;
    actionable)
        if [ "${1:-}" != "ready" ] || [ "${2:-}" != "--json" ]; then
            printf 'expected br ready --json, got: %s\n' "$*" >&2
            exit 67
        fi
        cat <<'JSON'
[
  {"id":"bd-safe","status":"open","assignee":null,"issue_type":"task","title":"Safe implementation leaf"},
  {"id":"bd-epic","status":"open","assignee":null,"issue_type":"epic","title":"Open parent epic"},
  {"id":"bd-progress","status":"in_progress","assignee":"OtherAgent","issue_type":"task","title":"Owned in-progress leaf"},
  {"id":"bd-blocked","status":"blocked","assignee":null,"issue_type":"bug","title":"Blocked high-priority bug"},
  {"id":"bd-assigned","status":"open","assignee":"OtherAgent","issue_type":"task","title":"Assigned open task"}
]
JSON
        ;;
    hang)
        printf '{"schema":"br.ready.v1","partial":true'
        printf 'fake br emitted a partial diagnostic before hanging\n' >&2
        sleep 30
        ;;
    malformed)
        printf 'worker filter output was malformed before JSON could be produced\n' >&2
        exit 65
        ;;
    *)
        printf 'unknown fake br mode: %s\n' "$mode" >&2
        exit 66
        ;;
esac
BR
chmod +x "$fake_bin/br"

start_ms="$(python3 -c 'import time; print(int(time.monotonic() * 1000))')"
PATH="$fake_bin:$PATH" \
EE_FAKE_BR_STATE="$state_file" \
EE_FAKE_BR_WORKSPACE_ID="$workspace_id" \
EE_FAKE_BR_REQUEST_ID="$request_id" \
BR_RETRY_TMPDIR="$artifact_root" \
    "$REPO_ROOT/scripts/br_retry.sh" ready --json >"$stdout_path" 2>"$stderr_path"
end_ms="$(python3 -c 'import time; print(int(time.monotonic() * 1000))')"

attempt="$(jq -r '.attempt' "$stdout_path")"
recovered_attempts="$(jq -r 'select(.schema=="ee.beads_retry.v1" and .succeeded==true) | .attempts' "$stderr_path" | tail -n 1)"

if [ "$attempt" != "2" ]; then
    echo "expected fake br success on second attempt, got attempt=$attempt" >&2
    exit 1
fi

if [ -z "$recovered_attempts" ] || [ "$recovered_attempts" -lt 2 ]; then
    echo "expected br_retry recovery diagnostic in $stderr_path" >&2
    exit 1
fi

hang_stdout_path="$artifact_root/hang_stdout.json"
hang_stderr_path="$artifact_root/hang_stderr.jsonl"
hang_start_ms="$(python3 -c 'import time; print(int(time.monotonic() * 1000))')"
set +e
PATH="$fake_bin:$PATH" \
EE_FAKE_BR_STATE="$state_file.hang" \
EE_FAKE_BR_MODE="hang" \
BR_RETRY_ATTEMPT_TIMEOUT_MS=150 \
BR_RETRY_TAIL_BYTES=256 \
BR_RETRY_TMPDIR="$artifact_root" \
    "$REPO_ROOT/scripts/br_retry.sh" ready --json >"$hang_stdout_path" 2>"$hang_stderr_path"
hang_status="$?"
set -e
hang_end_ms="$(python3 -c 'import time; print(int(time.monotonic() * 1000))')"
hang_elapsed_ms="$((hang_end_ms - hang_start_ms))"

if [ "$hang_status" -ne 124 ]; then
    echo "expected br_retry hang guard exit 124, got $hang_status" >&2
    cat "$hang_stderr_path" >&2
    exit 1
fi

if [ "$hang_elapsed_ms" -gt 5000 ]; then
    echo "expected hang guard to return quickly, elapsed_ms=$hang_elapsed_ms" >&2
    exit 1
fi

timeout_code="$(jq -r 'select(.schema=="ee.beads_retry.v1") | .degraded_codes[0]' "$hang_stderr_path" | tail -n 1)"
timeout_flag="$(jq -r 'select(.schema=="ee.beads_retry.v1") | .timed_out' "$hang_stderr_path" | tail -n 1)"
stdout_tail_has_partial="$(jq -r 'select(.schema=="ee.beads_retry.v1") | (.stdout_tail | contains("partial"))' "$hang_stderr_path" | tail -n 1)"
stderr_tail_has_hanging="$(jq -r 'select(.schema=="ee.beads_retry.v1") | (.stderr_tail | contains("hanging"))' "$hang_stderr_path" | tail -n 1)"
retained_stdout="$(jq -r 'select(.schema=="ee.beads_retry.v1") | .artifacts[] | select(.kind=="stdout") | .path' "$hang_stderr_path" | tail -n 1)"
retained_stderr="$(jq -r 'select(.schema=="ee.beads_retry.v1") | .artifacts[] | select(.kind=="stderr") | .path' "$hang_stderr_path" | tail -n 1)"

if [ "$timeout_code" != "beads_command_timeout" ] || [ "$timeout_flag" != "true" ]; then
    echo "expected beads_command_timeout timed_out=true diagnostic" >&2
    cat "$hang_stderr_path" >&2
    exit 1
fi

if [ "$stdout_tail_has_partial" != "true" ]; then
    echo "expected retained stdout tail to mention partial output" >&2
    cat "$hang_stderr_path" >&2
    exit 1
fi

if [ "$stderr_tail_has_hanging" != "true" ]; then
    echo "expected retained stderr tail to mention hanging diagnostic" >&2
    cat "$hang_stderr_path" >&2
    exit 1
fi

if [ ! -f "$retained_stdout" ] || [ ! -f "$retained_stderr" ]; then
    echo "expected retained stdout/stderr artifact paths to exist" >&2
    cat "$hang_stderr_path" >&2
    exit 1
fi

actionable_stdout_path="$artifact_root/actionable_stdout.json"
actionable_stderr_path="$artifact_root/actionable_stderr.jsonl"
PATH="$fake_bin:$PATH" \
EE_FAKE_BR_STATE="$state_file.actionable" \
EE_FAKE_BR_MODE="actionable" \
BR_RETRY_TMPDIR="$artifact_root" \
    "$REPO_ROOT/scripts/br_retry.sh" actionable --json >"$actionable_stdout_path" 2>"$actionable_stderr_path"

actionable_count="$(jq 'length' "$actionable_stdout_path")"
actionable_id="$(jq -r '.[0].id // ""' "$actionable_stdout_path")"
filtered_unsafe_count="$(jq '[.[] | select(.id == "bd-epic" or .id == "bd-progress" or .id == "bd-blocked" or .id == "bd-assigned")] | length' "$actionable_stdout_path")"

if [ "$actionable_count" -ne 1 ] || [ "$actionable_id" != "bd-safe" ]; then
    echo "expected actionable filter to keep only bd-safe" >&2
    cat "$actionable_stdout_path" >&2
    cat "$actionable_stderr_path" >&2
    exit 1
fi

if [ "$filtered_unsafe_count" -ne 0 ]; then
    echo "expected actionable filter to remove epic/in_progress/blocked/assigned rows" >&2
    cat "$actionable_stdout_path" >&2
    exit 1
fi

jq -c -n \
    --arg artifactRoot "$artifact_root" \
    --arg stdout "$stdout_path" \
    --arg stderr "$stderr_path" \
    --arg hangStdout "$hang_stdout_path" \
    --arg hangStderr "$hang_stderr_path" \
    --arg actionableStdout "$actionable_stdout_path" \
    --arg actionableStderr "$actionable_stderr_path" \
    --arg workspaceId "$workspace_id" \
    --arg requestId "$request_id" \
    --arg beadId "bd-3usjw.73" \
    --arg surface "scripts/br_retry.sh" \
    --arg phase "br_ready_json_read" \
    --argjson elapsedMs "$((end_ms - start_ms))" \
    --argjson hangElapsedMs "$hang_elapsed_ms" \
    --argjson recoveredAttempts "$recovered_attempts" \
    '{
      schema: "ee.test_event.v1",
      test: "br_concurrent_race",
      workspace_id: $workspaceId,
      request_id: $requestId,
      bead_id: $beadId,
      surface: $surface,
      phase: $phase,
      elapsed_ms: $elapsedMs,
      artifactRoot: $artifactRoot,
      stdoutPath: $stdout,
      stderrPath: $stderr,
      hangStdoutPath: $hangStdout,
      hangStderrPath: $hangStderr,
      actionableStdoutPath: $actionableStdout,
      actionableStderrPath: $actionableStderr,
      hang_elapsed_ms: $hangElapsedMs,
      race_observed: true,
      hang_guard_observed: true,
      actionable_filter_observed: true,
      retry_attempts: $recoveredAttempts,
      recovered_attempts: $recoveredAttempts,
      degraded_codes: ["beads_jsonl_partial_write_transient", "beads_command_timeout"],
      status: "pass"
    }' | tee "$summary_path"

echo "br_concurrent_race artifacts retained at $artifact_root" >&2
