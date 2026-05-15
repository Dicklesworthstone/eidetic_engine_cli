#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURE_DIR="$REPO_ROOT/tests/fixtures/swarm_incidents"
SCHEMA="$REPO_ROOT/docs/schemas/swarm/ee.swarm_incident.v1.json"
# This verifier writes only a tiny diagnostic JSONL. Keep it off the external
# build scratch path by default; callers can override when they need a custom
# artifact location.
EVENT_DIR="${EE_TEST_EVENT_DIR:-/tmp/ee-swarm-incident-events}"
EVENT_LOG="$EVENT_DIR/swarm_incident_check.jsonl"

mkdir -p "$EVENT_DIR"
: > "$EVENT_LOG"

if ! command -v jq >/dev/null 2>&1; then
  printf 'error: jq is required for swarm incident fixture checks\n' >&2
  exit 1
fi

emit_event() {
  local scenario_id="$1"
  local valid="$2"
  local errors_count="$3"
  local detail="$4"
  jq -cn \
    --arg schema "ee.test_event.v1" \
    --arg kind "swarm_incident_check" \
    --arg scenario_id "$scenario_id" \
    --arg detail "$detail" \
    --argjson valid "$valid" \
    --argjson errors_count "$errors_count" \
    '{schema:$schema,kind:$kind,scenarioId:$scenario_id,valid:$valid,errorsCount:$errors_count,detail:$detail}' \
    | tee -a "$EVENT_LOG" >&2
}

fail_fixture() {
  local scenario_id="$1"
  local detail="$2"
  emit_event "$scenario_id" false 1 "$detail"
  exit 1
}

if [[ ! -s "$SCHEMA" ]]; then
  fail_fixture "catalog" "missing ee.swarm_incident.v1 schema"
fi

mapfile -t fixture_files < <(find "$FIXTURE_DIR" -maxdepth 1 -type f -name '*.json' | sort)
expected_count=5
actual_count="${#fixture_files[@]}"
if [[ "$actual_count" != "$expected_count" ]]; then
  fail_fixture "catalog" "expected $expected_count incident fixtures, found $actual_count"
fi

declare -A required_scenarios=(
  [agent_mail_unavailable]=0
  [beads_jsonl_ahead_of_db]=0
  [disk_pressure_external_target_ok]=0
  [hot_path_burst_admission]=0
  [rch_topology_blocked]=0
)

for fixture in "${fixture_files[@]}"; do
  if ! jq -e . "$fixture" >/dev/null; then
    fail_fixture "$(basename "$fixture")" "invalid json"
  fi

  scenario_id="$(jq -r '.scenarioId // empty' "$fixture")"
  [[ -n "$scenario_id" ]] || fail_fixture "$(basename "$fixture")" "missing scenarioId"

  if [[ "$(jq -r '.schema' "$fixture")" != "ee.swarm_incident.v1" ]]; then
    fail_fixture "$scenario_id" "schema field must be ee.swarm_incident.v1"
  fi
  if [[ -z "${required_scenarios[$scenario_id]+present}" ]]; then
    fail_fixture "$scenario_id" "unexpected incident scenario"
  fi
  required_scenarios[$scenario_id]=1

  expected_file="$FIXTURE_DIR/$scenario_id.json"
  if [[ "$fixture" != "$expected_file" ]]; then
    fail_fixture "$scenario_id" "fixture filename must match scenarioId"
  fi

  if ! jq -e '
    .assertions.noLiveServices == true and
    .assertions.noLocalCargo == true and
    .assertions.noDeletion == true and
    .assertions.noMutation == true
  ' "$fixture" >/dev/null; then
    fail_fixture "$scenario_id" "safety assertions must all be true"
  fi

  for substrate in agentMail beads rch disk hotPath; do
    if ! jq -e --arg substrate "$substrate" '.substrates[$substrate] | type == "object"' "$fixture" >/dev/null; then
      fail_fixture "$scenario_id" "missing substrate $substrate"
    fi
  done

  degraded_count="$(jq -r '.expectedDegraded | length' "$fixture")"
  action_count="$(jq -r '.expectedRecoveryActions | length' "$fixture")"
  if [[ "$degraded_count" -lt 1 || "$action_count" -lt 1 ]]; then
    fail_fixture "$scenario_id" "expected degraded codes and recovery actions are required"
  fi

  if jq -e '
    [.expectedRecoveryActions[]
      | select(.destructive != false)
    ] | length > 0
  ' "$fixture" >/dev/null; then
    fail_fixture "$scenario_id" "recovery actions must be non-destructive"
  fi

  if jq -e '
    [.expectedRecoveryActions[]
      | select((.command // "" | test("(^|[[:space:]])(rm|git reset|git clean|git stash|git checkout|git rebase|cargo)([[:space:]]|$)")))
    ] | length > 0
  ' "$fixture" >/dev/null; then
    fail_fixture "$scenario_id" "recovery actions include a forbidden automatic command"
  fi

  if jq -e '
    tostring | test("/Users/[^/[:space:]\"]+")
  ' "$fixture" >/dev/null; then
    fail_fixture "$scenario_id" "fixture must not contain raw home paths"
  fi

  if jq -e '
    tostring | test("(?i)(api[_-]?key[[:space:]]*[:=]|bearer[[:space:]]+[a-z0-9._-]+|secret[[:space:]]*[:=]|token[[:space:]]*[:=])")
  ' "$fixture" >/dev/null; then
    fail_fixture "$scenario_id" "fixture must not contain secret-like strings"
  fi

  emit_event "$scenario_id" true 0 "incident fixture invariants passed"
done

for scenario_id in "${!required_scenarios[@]}"; do
  if [[ "${required_scenarios[$scenario_id]}" != "1" ]]; then
    fail_fixture "catalog" "missing required scenario $scenario_id"
  fi
done

printf 'swarm incident fixture checks passed; events=%s\n' "$EVENT_LOG" >&2
