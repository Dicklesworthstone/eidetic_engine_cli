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
  local bead="${4:-${E2E_CURRENT_BEAD:-bd-2vq2z.12}}"
  local workspace="${E2E_CURRENT_WORKSPACE:-$WORKSPACE}"
  jq -cn \
    --arg schema "ee.test_event.v1" \
    --arg event "$event" \
    --arg status "$status" \
    --arg detail "$detail" \
    --arg bead "$bead" \
    --arg workspace "$workspace" \
    '{schema:$schema,event:$event,status:$status,detail:$detail,bead:$bead,workspace:$workspace,ts:(now|todateiso8601)}' \
    >> "$LOG"
}

run_ee() {
  local event="$1"
  shift
  echo "e2e_delivery step=$event status=start cmd=$*" >&2
  log_event "$event" "start" "$*"
  "$EE_BIN" "$@"
  echo "e2e_delivery step=$event status=ok cmd=$*" >&2
  log_event "$event" "ok" "$*"
}

assert_jq() {
  local event="$1"
  local file="$2"
  local expr="$3"
  local bead="${4:-${E2E_CURRENT_BEAD:-bd-2vq2z.12}}"
  echo "e2e_delivery assert=$event status=start file=$file expr=$expr" >&2
  if jq -e "$expr" "$file" >/dev/null; then
    echo "e2e_delivery assert=$event status=ok" >&2
    log_event "$event" "ok" "$expr" "$bead"
  else
    echo "e2e_delivery assert=$event status=fail" >&2
    log_event "$event" "fail" "$expr" "$bead"
    echo "assertion failed: $event" >&2
    echo "log: $LOG" >&2
    exit 1
  fi
}

assert_equal() {
  local event="$1"
  local expected="$2"
  local actual="$3"
  local bead="${4:-${E2E_CURRENT_BEAD:-bd-2vq2z.12}}"
  echo "e2e_delivery assert=$event status=start expected=$expected actual=$actual" >&2
  if [[ "$expected" == "$actual" && -n "$actual" ]]; then
    echo "e2e_delivery assert=$event status=ok" >&2
    log_event "$event" "ok" "expected=$expected actual=$actual" "$bead"
  else
    echo "e2e_delivery assert=$event status=fail expected=$expected actual=$actual" >&2
    log_event "$event" "fail" "expected=$expected actual=$actual" "$bead"
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

ANTI_BEAD="bd-2vq2z.11"
ANTI_TASK="code-first swarm verification local cargo builds"

log_event "anti_pattern_first_e2e" "start" "reserved what NOT to do slice" "$ANTI_BEAD"

E2E_CURRENT_BEAD="$ANTI_BEAD" run_ee "remember_anti_pattern" remember \
  "Never run local Cargo builds during code-first swarm batches; use central batch verify." \
  --workspace "$WORKSPACE" \
  --level procedural \
  --kind anti-pattern \
  --tags delivery,anti-pattern-first,batch-verify \
  --source "test://bd-2vq2z.11/anti-pattern-first" \
  --json > "$ROOT/remember_antipattern.json"

assert_jq "anti_pattern_remember_schema" "$ROOT/remember_antipattern.json" \
  '.schema == "ee.response.v2" and .success == true and .data.kind == "anti-pattern" and ((.data.memory_id // .data.memoryId // "") | length > 0)' \
  "$ANTI_BEAD"

ANTI_MEMORY_ID="$(jq -r '.data.memory_id // .data.memoryId // empty' "$ROOT/remember_antipattern.json")"
assert_equal "anti_pattern_memory_id_present" "$ANTI_MEMORY_ID" "$ANTI_MEMORY_ID" "$ANTI_BEAD"

E2E_CURRENT_BEAD="$ANTI_BEAD" run_ee "remember_supporting_rule" remember \
  "For code-first swarm verification, commit scoped changes and wait for the central batch verifier." \
  --workspace "$WORKSPACE" \
  --level procedural \
  --kind rule \
  --tags delivery,anti-pattern-first,batch-verify \
  --source "test://bd-2vq2z.11/supporting-rule" \
  --json > "$ROOT/remember_supporting_rule.json"

assert_jq "supporting_rule_remember_schema" "$ROOT/remember_supporting_rule.json" \
  '.schema == "ee.response.v2" and .success == true and .data.kind == "rule"' \
  "$ANTI_BEAD"

E2E_CURRENT_BEAD="$ANTI_BEAD" run_ee "index_rebuild_anti_pattern" \
  index rebuild --workspace "$WORKSPACE" --json > "$ROOT/index_antipattern.json"

assert_jq "anti_pattern_index_rebuild_schema" "$ROOT/index_antipattern.json" \
  '.schema == "ee.response.v2" and .success == true' \
  "$ANTI_BEAD"

E2E_CURRENT_BEAD="$ANTI_BEAD" run_ee "pack_anti_pattern_small_budget_1" pack \
  "$ANTI_TASK" \
  --workspace "$WORKSPACE" \
  --profile balanced \
  --max-tokens 120 \
  --read-only \
  --json > "$ROOT/pack_antipattern_1.json"

E2E_CURRENT_BEAD="$ANTI_BEAD" run_ee "pack_anti_pattern_small_budget_2" pack \
  "$ANTI_TASK" \
  --workspace "$WORKSPACE" \
  --profile balanced \
  --max-tokens 120 \
  --read-only \
  --json > "$ROOT/pack_antipattern_2.json"

assert_jq "anti_pattern_pack_schema" "$ROOT/pack_antipattern_1.json" \
  '.schema == "ee.response.v2" and .success == true and ((.data.pack.hash // "") | startswith("blake3:")) and .data.pack.budget.maxTokens == 120' \
  "$ANTI_BEAD"

assert_jq "anti_pattern_reserved_item" "$ROOT/pack_antipattern_1.json" \
  "any(.data.pack.items[]?; .memoryId == \"$ANTI_MEMORY_ID\" and .section == \"failures\" and .selectedIn == \"anti_pattern_first\" and ((.why // \"\") | contains(\"What NOT to do\")) and ((.provenance // []) | length >= 1))" \
  "$ANTI_BEAD"

assert_jq "anti_pattern_provenance_uri" "$ROOT/pack_antipattern_1.json" \
  "any(.data.pack.items[]?; .memoryId == \"$ANTI_MEMORY_ID\" and any(.provenance[]?; .uri == \"test://bd-2vq2z.11/anti-pattern-first\"))" \
  "$ANTI_BEAD"

assert_jq "anti_pattern_markdown_section" "$ROOT/pack_antipattern_1.json" \
  '(.data.pack.text // "") | contains("## What NOT to do")' \
  "$ANTI_BEAD"

assert_jq "anti_pattern_budget_respected" "$ROOT/pack_antipattern_1.json" \
  '.data.pack.budget.usedTokens <= .data.pack.budget.maxTokens' \
  "$ANTI_BEAD"

PACK_HASH_1="$(jq -r '.data.pack.hash // empty' "$ROOT/pack_antipattern_1.json")"
PACK_HASH_2="$(jq -r '.data.pack.hash // empty' "$ROOT/pack_antipattern_2.json")"
assert_equal "anti_pattern_pack_hash_deterministic" "$PACK_HASH_1" "$PACK_HASH_2" "$ANTI_BEAD"

log_event "anti_pattern_first_e2e" "ok" "reserved what NOT to do slice" "$ANTI_BEAD"

log_event "delivery_e2e" "ok" "semantic query assistant"
echo "$LOG"
