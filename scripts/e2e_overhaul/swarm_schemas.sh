#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
SCHEMA_DIR="$REPO_ROOT/docs/schemas/swarm"
FIXTURE="$REPO_ROOT/tests/fixtures/swarm_schemas/all_examples.json"
EVENT_DIR="${TMPDIR:-/Volumes/USBNVME16TB/temp_agent_space/tmp}/ee-swarm-schema-events"
EVENT_LOG="$EVENT_DIR/swarm_schema_check.jsonl"

mkdir -p "$EVENT_DIR"
: > "$EVENT_LOG"

if ! command -v jq >/dev/null 2>&1; then
  printf 'error: jq is required for swarm schema e2e\n' >&2
  exit 1
fi

emit_event() {
  local schema_id="$1"
  local valid="$2"
  local errors_count="$3"
  local detail="$4"
  jq -cn \
    --arg schema "ee.test_event.v1" \
    --arg kind "swarm_schema_check" \
    --arg schema_id "$schema_id" \
    --arg detail "$detail" \
    --argjson valid "$valid" \
    --argjson errors_count "$errors_count" \
    '{schema:$schema,kind:$kind,schemaId:$schema_id,valid:$valid,errorsCount:$errors_count,detail:$detail}' \
    | tee -a "$EVENT_LOG" >&2
}

expected_count="$(jq -r '.examples | length' "$FIXTURE")"
actual_count="$(find "$SCHEMA_DIR" -maxdepth 1 -type f -name '*.json' | wc -l | tr -d ' ')"
if [[ "$actual_count" != "$expected_count" ]]; then
  emit_event "catalog" false 1 "fixture manifest has $expected_count schema rows, found $actual_count schema files"
  exit 1
fi

for schema_file in "$SCHEMA_DIR"/*.json; do
  file_name="$(basename "$schema_file")"
  schema_id="${file_name%.json}"
  expected_id="https://eidetic-engine/schemas/swarm/$file_name"

  if ! jq -e . "$schema_file" >/dev/null; then
    emit_event "$schema_id" false 1 "invalid json"
    exit 1
  fi
  schema_dialect="$(jq -r '."$schema"' "$schema_file")"
  case "$schema_dialect" in
    "http://json-schema.org/draft-07/schema#"|"https://json-schema.org/draft/2020-12/schema") ;;
    *)
      emit_event "$schema_id" false 1 "unsupported schema dialect: $schema_dialect"
      exit 1
      ;;
  esac
  if [[ "$(jq -r '."$id"' "$schema_file")" != "$expected_id" ]]; then
    emit_event "$schema_id" false 1 "non-canonical id"
    exit 1
  fi
  if [[ "$(jq -r '.title' "$schema_file")" != "$schema_id" ]]; then
    emit_event "$schema_id" false 1 "title mismatch"
    exit 1
  fi
  if ! jq -e '.["x-ee-status"] | has("shipped") and has("tracking_bead") and has("available_in_build")' "$schema_file" >/dev/null; then
    emit_event "$schema_id" false 1 "missing x-ee-status fields"
    exit 1
  fi
  if ! jq -e --arg schema_id "$schema_id" '.examples[$schema_id] != null' "$FIXTURE" >/dev/null; then
    emit_event "$schema_id" false 1 "fixture manifest missing example"
    exit 1
  fi
  if ! jq -e '.examples | type == "array" and length > 0' "$schema_file" >/dev/null; then
    emit_event "$schema_id" false 1 "schema examples missing"
    exit 1
  fi
  emit_event "$schema_id" true 0 "schema, status, and fixture rows present"
done

printf 'swarm schema e2e passed; events=%s\n' "$EVENT_LOG" >&2
