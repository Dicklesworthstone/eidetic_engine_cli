#!/usr/bin/env bash
set -euo pipefail

EE_BIN="${EE_BIN:-ee}"
ROOT="${EE_E2E_TMPDIR:-${TMPDIR:-/tmp}}/ee-e2e-delivery.$$"
WORKSPACE="$ROOT/workspace"
LOG_DIR="$ROOT/logs"
LOG="$LOG_DIR/e2e_delivery.jsonl"

mkdir -p "$WORKSPACE" "$LOG_DIR"

log_event() {
  local event="$1"
  local status="$2"
  local detail="${3:-}"
  jq -cn \
    --arg schema "ee.test_event.v1" \
    --arg event "$event" \
    --arg status "$status" \
    --arg detail "$detail" \
    --arg bead "bd-2vq2z.12" \
    --arg workspace "$WORKSPACE" \
    '{schema:$schema,event:$event,status:$status,detail:$detail,bead:$bead,workspace:$workspace,ts:(now|todateiso8601)}' \
    >> "$LOG"
}

run_ee() {
  local event="$1"
  shift
  log_event "$event" "start" "$*"
  "$EE_BIN" "$@"
  log_event "$event" "ok" "$*"
}

assert_jq() {
  local event="$1"
  local file="$2"
  local expr="$3"
  if jq -e "$expr" "$file" >/dev/null; then
    log_event "$event" "ok" "$expr"
  else
    log_event "$event" "fail" "$expr"
    echo "assertion failed: $event" >&2
    echo "log: $LOG" >&2
    exit 1
  fi
}

log_event "delivery_e2e" "start" "semantic query assistant"

run_ee "init" init --workspace "$WORKSPACE" --json > "$ROOT/init.json"
run_ee "remember_paraphrase" remember \
  "Release installers must pass live smoke validation before publishing artifacts." \
  --workspace "$WORKSPACE" \
  --level procedural \
  --kind rule \
  --json > "$ROOT/remember.json"
run_ee "index_rebuild" index rebuild --workspace "$WORKSPACE" --json > "$ROOT/index.json"

run_ee "search_paraphrase_high_floor" search \
  "shipper launch rehearsal" \
  --workspace "$WORKSPACE" \
  --relevance-floor 0.99 \
  --explain \
  --json > "$ROOT/search_paraphrase.json"

assert_jq "paraphrase_query_assist_schema" "$ROOT/search_paraphrase.json" \
  '.data.queryAssist.schema == "ee.query_assist.v1"'
assert_jq "paraphrase_did_you_mean_surfaces_memory" "$ROOT/search_paraphrase.json" \
  '([.data.queryAssist.didYouMean[]? | (.content // "")] | join(" ") | contains("installer") and contains("smoke"))'

for attempt in 1 2 3; do
  run_ee "search_absent_${attempt}" search \
    "orbital stapler quorum" \
    --workspace "$WORKSPACE" \
    --relevance-floor 0.99 \
    --explain \
    --json > "$ROOT/search_absent_${attempt}.json"
done

assert_jq "absent_capture_template" "$ROOT/search_absent_3.json" \
  '(.data.queryAssist.captureTemplate.command // "") | startswith("ee remember ")'

run_ee "learn_gaps" learn gaps --workspace "$WORKSPACE" --json > "$ROOT/learn_gaps.json"
assert_jq "absent_query_recorded_in_learn_gaps" "$ROOT/learn_gaps.json" \
  '.clusterCount >= 1 and (.gaps | length) >= 1'

log_event "delivery_e2e" "ok" "semantic query assistant"
echo "$LOG"
