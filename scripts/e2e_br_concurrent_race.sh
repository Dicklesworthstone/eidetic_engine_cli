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
count=0
if [ -f "$state_file" ]; then
    count="$(cat "$state_file")"
fi
count=$((count + 1))
printf '%s\n' "$count" >"$state_file"

if [ "$count" -eq 1 ]; then
    printf 'Configuration error: Invalid JSON at line 2318: invalid type: integer `7`, expected struct Issue\n' >&2
    exit 2
fi

printf '{"schema":"br.ready.v1","workspace_id":"%s","request_id":"%s","issues":[],"attempt":%s}\n' "${EE_FAKE_BR_WORKSPACE_ID:-br-race-fixture}" "${EE_FAKE_BR_REQUEST_ID:-bd-3usjw.73-e2e}" "$count"
BR
chmod +x "$fake_bin/br"

start_ms="$(python3 -c 'import time; print(int(time.monotonic() * 1000))')"
PATH="$fake_bin:$PATH" \
EE_FAKE_BR_STATE="$state_file" \
EE_FAKE_BR_WORKSPACE_ID="$workspace_id" \
EE_FAKE_BR_REQUEST_ID="$request_id" \
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

jq -c -n \
    --arg artifactRoot "$artifact_root" \
    --arg stdout "$stdout_path" \
    --arg stderr "$stderr_path" \
    --arg workspaceId "$workspace_id" \
    --arg requestId "$request_id" \
    --arg beadId "bd-3usjw.73" \
    --arg surface "scripts/br_retry.sh" \
    --arg phase "br_ready_json_read" \
    --argjson elapsedMs "$((end_ms - start_ms))" \
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
      race_observed: true,
      retry_attempts: $recoveredAttempts,
      recovered_attempts: $recoveredAttempts,
      degraded_codes: ["beads_jsonl_partial_write_transient"],
      status: "pass"
    }' | tee "$summary_path"

echo "br_concurrent_race artifacts retained at $artifact_root" >&2
