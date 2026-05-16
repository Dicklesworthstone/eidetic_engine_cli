#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURE_DIR="$REPO_ROOT/tests/fixtures/swarm_incidents"
UNSAFE_FIXTURE="$FIXTURE_DIR/unsafe_recovery_actions.json"
EVENT_DIR="${EE_TEST_EVENT_DIR:-/tmp/ee-swarm-incident-events}"
EVENT_LOG="$EVENT_DIR/swarm_incident_recovery_actions.jsonl"

mkdir -p "$EVENT_DIR"
: > "$EVENT_LOG"

if ! command -v jq >/dev/null 2>&1; then
  printf 'error: jq is required for swarm incident recovery-action checks\n' >&2
  exit 1
fi

emit_event() {
  local scenario_id="$1"
  local expected_valid="$2"
  local valid="$3"
  local errors_count="$4"
  local detail="$5"
  local fixture_path="$6"
  jq -cn \
    --arg schema "ee.test_event.v1" \
    --arg kind "swarm_incident_recovery_actions" \
    --arg scenario_id "$scenario_id" \
    --arg detail "$detail" \
    --arg fixture_path "$fixture_path" \
    --argjson expected_valid "$expected_valid" \
    --argjson valid "$valid" \
    --argjson errors_count "$errors_count" \
    '{schema:$schema,kind:$kind,scenarioId:$scenario_id,expectedValid:$expected_valid,valid:$valid,errorsCount:$errors_count,detail:$detail,fixturePath:$fixture_path}' \
    | tee -a "$EVENT_LOG" >&2
}

fixture_errors() {
  local fixture="$1"
  jq -r '
    def pointer($index; $field): "\($fixture)#expectedRecoveryActions[\($index)]\($field)";
    def nonempty_string_array:
      type == "array" and length > 0 and all(.[]; type == "string" and length > 0);
    def manual_human_approval:
      .kind == "manual"
      and .command == null
      and ((.manualStep // "") | test("(?i)human|approval|approve|explicit"))
      and ((.preconditions // []) | any(test("(?i)human|approval|approve|explicit")));
    def forbidden_command:
      test("(^|[[:space:];|&])(rm|rmdir|git[[:space:]]+(reset|clean|stash|checkout|rebase)|cargo[[:space:]]+(build|test|check|run|clippy|bench))([[:space:];|&]|$)");
    . as $root
    | ($root.scenarioId // input_filename) as $scenario
    | ($root.expectedRecoveryActions // []) as $actions
    | if ($actions | type) != "array" or ($actions | length) == 0 then
        "\($fixture)#expectedRecoveryActions: expected a non-empty array"
      else empty end,
      ($actions | to_entries[] as $entry
        | ($entry.key) as $i
        | ($entry.value) as $action
        | (["priority","kind","summary","command","manualStep","evidence","destructive","preconditions"] - ($action | keys_unsorted))[]? as $missing
        | pointer($i; ".\($missing)") + ": missing required recovery-action field"),
      ($actions | to_entries[] as $entry
        | ($entry.key) as $i
        | ($entry.value) as $action
        | if (($action.command == null) and (($action.manualStep // null) == null)) then
            pointer($i; "") + ": action must include command or manualStep"
          else empty end),
      ($actions | to_entries[] as $entry
        | ($entry.key) as $i
        | ($entry.value) as $action
        | if (($action.evidence // []) | nonempty_string_array | not) then
            pointer($i; ".evidence") + ": evidence must be a non-empty string array"
          else empty end),
      ($actions | to_entries[] as $entry
        | ($entry.key) as $i
        | ($entry.value) as $action
        | if (($action.preconditions // []) | type) != "array" then
            pointer($i; ".preconditions") + ": preconditions must be an array"
          else empty end),
      ($actions | to_entries[] as $entry
        | ($entry.key) as $i
        | ($entry.value) as $action
        | if (($action.destructive // false) == true and ($action | manual_human_approval | not)) then
            pointer($i; ".destructive") + ": destructive actions must be manual and explicitly human-approved"
          else empty end),
      ($actions | to_entries[] as $entry
        | ($entry.key) as $i
        | ($entry.value) as $action
        | if (($action.command // "") | forbidden_command) then
            pointer($i; ".command") + ": forbidden automatic command: \($action.command)"
          else empty end),
      (if (($actions[0].kind // "") != "observe") then
          "\($fixture)#expectedRecoveryActions[0].kind: first action must observe before repair or retry"
        else empty end),
      ($actions | to_entries[] as $entry
        | ($entry.key) as $i
        | ($entry.value.priority // 0) as $priority
        | if ($i > 0 and $priority < (($actions[$i - 1].priority // 0))) then
            pointer($i; ".priority") + ": recovery actions must be ordered by nondecreasing priority"
          else empty end),
      ($actions | to_entries[] as $entry
        | ($entry.key) as $i
        | ($entry.value.kind // "") as $kind
        | if ($kind == "retry" and $i != (($actions | length) - 1)) then
            pointer($i; ".kind") + ": retry must be the final action"
          else empty end)
  ' --arg fixture "$fixture" "$fixture"
}

check_fixture() {
  local fixture="$1"
  local expected_valid="$2"
  local scenario_id
  local errors_file
  local errors_count
  local valid
  scenario_id="$(jq -r '.scenarioId // empty' "$fixture")"
  if [[ -z "$scenario_id" ]]; then
    scenario_id="$(basename "$fixture")"
  fi
  errors_file="$(mktemp "${TMPDIR:-/tmp}/ee-recovery-actions.XXXXXX")"
  fixture_errors "$fixture" > "$errors_file"
  errors_count="$(wc -l < "$errors_file" | tr -d '[:space:]')"
  if [[ "$errors_count" -eq 0 ]]; then
    valid=true
  else
    valid=false
  fi

  if [[ "$valid" == "$expected_valid" ]]; then
    emit_event "$scenario_id" "$expected_valid" "$valid" "$errors_count" "recovery-action verifier expectation matched" "$fixture"
  else
    emit_event "$scenario_id" "$expected_valid" "$valid" "$errors_count" "$(head -n 1 "$errors_file")" "$fixture"
    cat "$errors_file" >&2
    return 1
  fi

  if [[ "$expected_valid" == "false" && "$errors_count" -lt 1 ]]; then
    emit_event "$scenario_id" "$expected_valid" "$valid" "$errors_count" "negative fixture did not produce verifier errors" "$fixture"
    return 1
  fi
}

mapfile -t valid_fixtures < <(find "$FIXTURE_DIR" -maxdepth 1 -type f -name '*.json' ! -name 'unsafe_recovery_actions.json' | sort)
for fixture in "${valid_fixtures[@]}"; do
  check_fixture "$fixture" true
done

if [[ ! -s "$UNSAFE_FIXTURE" ]]; then
  emit_event "unsafe_recovery_actions" false false 1 "missing negative fixture" "$UNSAFE_FIXTURE"
  exit 1
fi
check_fixture "$UNSAFE_FIXTURE" false

printf 'swarm incident recovery-action checks passed; events=%s\n' "$EVENT_LOG" >&2
