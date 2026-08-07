#!/usr/bin/env bash
set -euo pipefail

EE_BIN="${EE_BIN:-ee}"
ROOT="${EE_E2E_TMPDIR:-${TMPDIR:-/tmp}}/ee-e2e-delivery.$$"
WORKSPACE="$ROOT/workspace"
LOG_DIR="$ROOT/logs"
LOG="$LOG_DIR/e2e_delivery.jsonl"
HOOK_STATE_DIR="$ROOT/hook-state"

mkdir -p "$WORKSPACE" "$LOG_DIR" "$HOOK_STATE_DIR"

resolve_ee_bin_path() {
  if [[ "$EE_BIN" == */* ]]; then
    printf '%s\n' "$EE_BIN"
  elif command -v "$EE_BIN" >/dev/null 2>&1; then
    command -v "$EE_BIN"
  else
    printf '%s\n' "$EE_BIN"
  fi
}

EE_BIN_PATH="$(resolve_ee_bin_path)"
export EE_AMBIENT_CONTEXT=true
export EE_AMBIENT_CONTEXT_VERBOSITY=standard
export EE_AMBIENT_CONTEXT_STATE_DIR="$HOOK_STATE_DIR"
export EE_NO_COLOR=1

log_event() {
  local event="$1"
  local status="$2"
  local detail="${3:-}"
  local bead="${4:-${E2E_CURRENT_BEAD:-bd-2vq2z.21}}"
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

run_hook() {
  local event="$1"
  local command="$2"
  local payload="$3"
  local stdout_file="$4"
  local stderr_file="$5"
  local bead="${6:-${E2E_CURRENT_BEAD:-bd-2vq2z.21}}"
  echo "e2e_delivery hook=$event status=start" >&2
  log_event "$event" "start" "generated hook command" "$bead"
  set +e
  printf '%s' "$payload" | \
    EE_AMBIENT_CONTEXT="${EE_AMBIENT_CONTEXT:-true}" \
    EE_AMBIENT_CONTEXT_VERBOSITY="${EE_AMBIENT_CONTEXT_VERBOSITY:-standard}" \
    EE_AMBIENT_CONTEXT_STATE_DIR="${EE_AMBIENT_CONTEXT_STATE_DIR:-$HOOK_STATE_DIR}" \
    EE_NO_COLOR=1 \
    bash -lc "$command" > "$stdout_file" 2> "$stderr_file"
  local rc=$?
  set -e
  if [[ "$rc" == "0" ]]; then
    echo "e2e_delivery hook=$event status=ok stdout=$stdout_file stderr=$stderr_file" >&2
    log_event "$event" "ok" "stdout=$stdout_file stderr=$stderr_file" "$bead"
  else
    echo "e2e_delivery hook=$event status=fail rc=$rc stdout=$stdout_file stderr=$stderr_file" >&2
    log_event "$event" "fail" "rc=$rc stdout=$stdout_file stderr=$stderr_file" "$bead"
    echo "hook failed: $event" >&2
    echo "stdout:" >&2
    cat "$stdout_file" >&2 || true
    echo "stderr:" >&2
    cat "$stderr_file" >&2 || true
    echo "log: $LOG" >&2
    exit 1
  fi
}

assert_jq() {
  local event="$1"
  local file="$2"
  local expr="$3"
  local bead="${4:-${E2E_CURRENT_BEAD:-bd-2vq2z.21}}"
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
  local bead="${4:-${E2E_CURRENT_BEAD:-bd-2vq2z.21}}"
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

assert_empty_file() {
  local event="$1"
  local file="$2"
  local bead="${3:-${E2E_CURRENT_BEAD:-bd-2vq2z.21}}"
  echo "e2e_delivery assert=$event status=start file=$file empty=true" >&2
  if [[ ! -s "$file" ]]; then
    echo "e2e_delivery assert=$event status=ok" >&2
    log_event "$event" "ok" "empty output suppressed" "$bead"
  else
    echo "e2e_delivery assert=$event status=fail file=$file" >&2
    log_event "$event" "fail" "expected empty output" "$bead"
    echo "assertion failed: $event" >&2
    cat "$file" >&2 || true
    echo "log: $LOG" >&2
    exit 1
  fi
}

assert_le() {
  local event="$1"
  local actual="$2"
  local limit="$3"
  local bead="${4:-${E2E_CURRENT_BEAD:-bd-2vq2z.21}}"
  echo "e2e_delivery assert=$event status=start actual=$actual limit=$limit" >&2
  if [[ "$actual" =~ ^[0-9]+$ && "$limit" =~ ^[0-9]+$ && "$actual" -le "$limit" ]]; then
    echo "e2e_delivery assert=$event status=ok" >&2
    log_event "$event" "ok" "actual=$actual limit=$limit" "$bead"
  else
    echo "e2e_delivery assert=$event status=fail actual=$actual limit=$limit" >&2
    log_event "$event" "fail" "actual=$actual limit=$limit" "$bead"
    echo "assertion failed: $event" >&2
    echo "log: $LOG" >&2
    exit 1
  fi
}

hook_command_for() {
  local snippet_id="$1"
  jq -r --arg id "$snippet_id" \
    '.data.harnessInstall.snippets[]? | select(.id == $id) | .command' \
    "$ROOT/hook_plan.json" | head -n 1
}

hook_context_word_count() {
  local file="$1"
  jq -r '.hookSpecificOutput.additionalContext // ""' "$file" | \
    python3 -c 'import sys; print(len(sys.stdin.read().split()))'
}

hook_context_budget() {
  local file="$1"
  jq -r '.hookSpecificOutput.additionalContext // ""' "$file" | \
    python3 -c 'import re, sys; text=sys.stdin.read(); match=re.search(r"budgetTokens=([0-9]+)", text); print(match.group(1) if match else "0")'
}

log_event "delivery_e2e" "start" "ambient-context anti-pattern pack query assistant"

run_ee "init" init --workspace "$WORKSPACE" --json > "$ROOT/init.json"

assert_jq "init_response_schema" "$ROOT/init.json" \
  '.schema == "ee.response.v2" and .success == true'

AMBIENT_BEAD="bd-2vq2z.10"

log_event "ambient_context_e2e" "start" "generated hook profile and hook entrypoints" "$AMBIENT_BEAD"

run_ee "hook_codex_ambient_plan" hook codex \
  --ambient \
  --print \
  --workspace "$WORKSPACE" \
  --settings-path "$ROOT/codex-settings.json" \
  --ee-binary "$EE_BIN_PATH" \
  --json > "$ROOT/hook_plan.json"

assert_jq "ambient_hook_plan_schema" "$ROOT/hook_plan.json" \
  '.schema == "ee.response.v2" and .success == true and .data.harnessInstall.schema == "ee.hook.harness_install.v1"' \
  "$AMBIENT_BEAD"
assert_jq "ambient_profile_is_on_by_default_and_read_only" "$ROOT/hook_plan.json" \
  '.data.harnessInstall.ambientContext.schema == "ee.ambient_context.v1" and .data.harnessInstall.ambientContext.enabledByDefault == true and .data.harnessInstall.ambientContext.readOnly == true and .data.harnessInstall.ambientContext.provenanceTag == "ee:ee.ambient_context.v1"' \
  "$AMBIENT_BEAD"
assert_jq "ambient_profile_budgets_registered" "$ROOT/hook_plan.json" \
  '([.data.harnessInstall.ambientContext.budgets[]? | .surface] | sort) == ["pre_edit_recall","session_end_capture_suggest","session_start_orient"]' \
  "$AMBIENT_BEAD"
assert_jq "ambient_profile_suppression_rules_registered" "$ROOT/hook_plan.json" \
  '([.data.harnessInstall.ambientContext.suppressionRules[]? | .code] | index("duplicate_in_session") != null) and ([.data.harnessInstall.ambientContext.suppressionRules[]? | .code] | index("empty_context") != null) and ([.data.harnessInstall.ambientContext.suppressionRules[]? | .code] | index("preflight_allows_command") == null)' \
  "$AMBIENT_BEAD"
# shellcheck disable=SC2016
assert_jq "ambient_snippets_include_delivery_hooks" "$ROOT/hook_plan.json" \
  '(. as $root | ["ee-ambient-session-orient","ee-ambient-pre-edit-recall","ee-ambient-session-capture-suggest"] as $required | all($required[]; . as $id | any($root.data.harnessInstall.snippets[]?; .id == $id and .installable == true and (.command | contains("ee.ambient_context.v1"))))) and all(.data.harnessInstall.snippets[]?; .id != "ee-ambient-pre-risky-preflight" and ((.command | contains("permissionDecision")) | not))' \
  "$AMBIENT_BEAD"

SESSION_ORIENT_CMD="$(hook_command_for "ee-ambient-session-orient")"
PRE_EDIT_CMD="$(hook_command_for "ee-ambient-pre-edit-recall")"
CAPTURE_SUGGEST_CMD="$(hook_command_for "ee-ambient-session-capture-suggest")"
assert_equal "ambient_session_orient_command_present" "$SESSION_ORIENT_CMD" "$SESSION_ORIENT_CMD" "$AMBIENT_BEAD"
assert_equal "ambient_pre_edit_command_present" "$PRE_EDIT_CMD" "$PRE_EDIT_CMD" "$AMBIENT_BEAD"
assert_equal "ambient_capture_suggest_command_present" "$CAPTURE_SUGGEST_CMD" "$CAPTURE_SUGGEST_CMD" "$AMBIENT_BEAD"

SESSION_PAYLOAD="$(jq -cn \
  --arg cwd "$WORKSPACE" \
  --arg session "bd-2vq2z-21-session" \
  '{hook_event_name:"SessionStart",cwd:$cwd,session_id:$session,task:"delivery e2e ambient orientation"}')"
run_hook "ambient_session_orient_first" "$SESSION_ORIENT_CMD" "$SESSION_PAYLOAD" "$ROOT/session_orient_1.json" "$ROOT/session_orient_1.stderr" "$AMBIENT_BEAD"
assert_jq "ambient_session_orient_injects_context" "$ROOT/session_orient_1.json" \
  '.hookSpecificOutput.hookEventName == "SessionStart" and (.hookSpecificOutput.additionalContext | contains("ee ambient_context") and contains("surface=session_start_orient") and contains("schema=ee.ambient_context.v1") and contains("provenance=ee:ee.ambient_context.v1") and contains("ee.response.v2"))' \
  "$AMBIENT_BEAD"
run_hook "ambient_session_orient_duplicate" "$SESSION_ORIENT_CMD" "$SESSION_PAYLOAD" "$ROOT/session_orient_duplicate.json" "$ROOT/session_orient_duplicate.stderr" "$AMBIENT_BEAD"
assert_empty_file "ambient_session_orient_duplicate_suppressed" "$ROOT/session_orient_duplicate.json" "$AMBIENT_BEAD"
EE_AMBIENT_CONTEXT_VERBOSITY=quiet \
EE_AMBIENT_CONTEXT_STATE_DIR="$ROOT/hook-state-quiet" \
  run_hook "ambient_session_orient_quiet" "$SESSION_ORIENT_CMD" "$SESSION_PAYLOAD" "$ROOT/session_orient_quiet.json" "$ROOT/session_orient_quiet.stderr" "$AMBIENT_BEAD"
assert_empty_file "ambient_session_orient_quiet_suppressed" "$ROOT/session_orient_quiet.json" "$AMBIENT_BEAD"

mkdir -p "$WORKSPACE/src"
printf '%s\n' 'pub fn delivery_anchor() {}' > "$WORKSPACE/src/delivery.rs"
E2E_CURRENT_BEAD="$AMBIENT_BEAD" run_ee "remember_ambient_anchor" remember \
  "Before editing delivery fixture code, preserve provenance-tagged recall context. anchor:path:src/delivery.rs" \
  --workspace "$WORKSPACE" \
  --level procedural \
  --kind rule \
  --source "test://bd-2vq2z.10/pre-edit-recall" \
  --json > "$ROOT/remember_ambient_anchor.json"
assert_jq "ambient_anchor_remember_schema" "$ROOT/remember_ambient_anchor.json" \
  '.schema == "ee.response.v2" and .success == true and .data.kind == "rule"' \
  "$AMBIENT_BEAD"

run_ee "memory_list_before_ambient_hooks" memory list --workspace "$WORKSPACE" --json > "$ROOT/memory_before_ambient_hooks.json"
run_ee "curate_candidates_before_ambient_hooks" curate candidates --workspace "$WORKSPACE" --json > "$ROOT/candidates_before_ambient_hooks.json"
MEMORY_COUNT_BEFORE="$(jq -r '(.data.memories // []) | length' "$ROOT/memory_before_ambient_hooks.json")"
CANDIDATE_COUNT_BEFORE="$(jq -r '(.data.candidates // []) | length' "$ROOT/candidates_before_ambient_hooks.json")"

PRE_EDIT_PAYLOAD="$(jq -cn \
  --arg cwd "$WORKSPACE" \
  '{hook_event_name:"PreToolUse",tool_name:"Edit",cwd:$cwd,tool_input:{file_path:"src/delivery.rs"}}')"
run_hook "ambient_pre_edit_recall_first" "$PRE_EDIT_CMD" "$PRE_EDIT_PAYLOAD" "$ROOT/pre_edit_1.json" "$ROOT/pre_edit_1.stderr" "$AMBIENT_BEAD"
assert_jq "ambient_pre_edit_recall_injects_context" "$ROOT/pre_edit_1.json" \
  '.hookSpecificOutput.hookEventName == "PreToolUse" and (.hookSpecificOutput.additionalContext | contains("surface=pre_edit_recall") and contains("budgetTokens=400") and contains("maxPaths=8") and contains("provenance=ee:ee.ambient_context.v1") and contains("delivery fixture") and contains("provenance-tagged"))' \
  "$AMBIENT_BEAD"
run_hook "ambient_pre_edit_recall_duplicate" "$PRE_EDIT_CMD" "$PRE_EDIT_PAYLOAD" "$ROOT/pre_edit_duplicate.json" "$ROOT/pre_edit_duplicate.stderr" "$AMBIENT_BEAD"
assert_empty_file "ambient_pre_edit_duplicate_suppressed" "$ROOT/pre_edit_duplicate.json" "$AMBIENT_BEAD"
NO_PATH_PAYLOAD="$(jq -cn --arg cwd "$WORKSPACE" '{hook_event_name:"PreToolUse",tool_name:"Edit",cwd:$cwd,tool_input:{}}')"
run_hook "ambient_pre_edit_no_path" "$PRE_EDIT_CMD" "$NO_PATH_PAYLOAD" "$ROOT/pre_edit_no_path.json" "$ROOT/pre_edit_no_path.stderr" "$AMBIENT_BEAD"
assert_empty_file "ambient_pre_edit_no_path_suppressed" "$ROOT/pre_edit_no_path.json" "$AMBIENT_BEAD"

CAPTURE_PAYLOAD="$(jq -cn --arg cwd "$WORKSPACE" '{hook_event_name:"SessionEnd",cwd:$cwd,session_id:"bd-2vq2z-21-session"}')"
run_hook "ambient_session_capture_empty" "$CAPTURE_SUGGEST_CMD" "$CAPTURE_PAYLOAD" "$ROOT/session_capture_empty.json" "$ROOT/session_capture_empty.stderr" "$AMBIENT_BEAD"
assert_empty_file "ambient_session_capture_empty_suppressed" "$ROOT/session_capture_empty.json" "$AMBIENT_BEAD"

run_ee "memory_list_after_ambient_hooks" memory list --workspace "$WORKSPACE" --json > "$ROOT/memory_after_ambient_hooks.json"
run_ee "curate_candidates_after_ambient_hooks" curate candidates --workspace "$WORKSPACE" --json > "$ROOT/candidates_after_ambient_hooks.json"
MEMORY_COUNT_AFTER="$(jq -r '(.data.memories // []) | length' "$ROOT/memory_after_ambient_hooks.json")"
CANDIDATE_COUNT_AFTER="$(jq -r '(.data.candidates // []) | length' "$ROOT/candidates_after_ambient_hooks.json")"
assert_equal "ambient_hooks_do_not_mutate_memory_count" "$MEMORY_COUNT_BEFORE" "$MEMORY_COUNT_AFTER" "$AMBIENT_BEAD"
assert_equal "ambient_hooks_do_not_mutate_candidate_count" "$CANDIDATE_COUNT_BEFORE" "$CANDIDATE_COUNT_AFTER" "$AMBIENT_BEAD"

SESSION_ORIENT_WORDS="$(hook_context_word_count "$ROOT/session_orient_1.json")"
PRE_EDIT_WORDS="$(hook_context_word_count "$ROOT/pre_edit_1.json")"
SESSION_ORIENT_BUDGET="$(hook_context_budget "$ROOT/session_orient_1.json")"
PRE_EDIT_BUDGET="$(hook_context_budget "$ROOT/pre_edit_1.json")"
TOTAL_AMBIENT_WORDS=$((SESSION_ORIENT_WORDS + PRE_EDIT_WORDS))
TOTAL_AMBIENT_BUDGET=$((SESSION_ORIENT_BUDGET + PRE_EDIT_BUDGET))
assert_le "ambient_noise_governor_total_words_within_declared_budget" "$TOTAL_AMBIENT_WORDS" "$TOTAL_AMBIENT_BUDGET" "$AMBIENT_BEAD"
log_event "ambient_noise_governor_summary" "ok" "words=$TOTAL_AMBIENT_WORDS budget=$TOTAL_AMBIENT_BUDGET" "$AMBIENT_BEAD"
log_event "ambient_context_e2e" "ok" "generated hook entrypoints exercised" "$AMBIENT_BEAD"

QUERY_BEAD="bd-2vq2z.12"

log_event "query_assistant_e2e" "start" "semantic query assistant" "$QUERY_BEAD"
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
  '.data.queryAssist.schema == "ee.query_assist.v1"' \
  "$QUERY_BEAD"
assert_jq "paraphrase_did_you_mean_surfaces_memory" "$ROOT/search_paraphrase.json" \
  '([.data.queryAssist.didYouMean[]? | (.content // "")] | join(" ") | contains("installer") and contains("smoke"))' \
  "$QUERY_BEAD"

for attempt in 1 2 3; do
  run_ee "search_absent_${attempt}" search \
    "orbital stapler quorum" \
    --workspace "$WORKSPACE" \
    --relevance-floor 0.99 \
    --explain \
    --json > "$ROOT/search_absent_${attempt}.json"
done

assert_jq "absent_capture_template" "$ROOT/search_absent_3.json" \
  '(.data.queryAssist.captureTemplate.command // "") | startswith("ee remember ")' \
  "$QUERY_BEAD"

run_ee "learn_gaps" learn gaps --workspace "$WORKSPACE" --json > "$ROOT/learn_gaps.json"
assert_jq "absent_query_recorded_in_learn_gaps" "$ROOT/learn_gaps.json" \
  '.clusterCount >= 1 and (.gaps | length) >= 1' \
  "$QUERY_BEAD"
log_event "query_assistant_e2e" "ok" "did-you-mean and capture-template routes exercised" "$QUERY_BEAD"

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

log_event "delivery_e2e" "ok" "ambient-context anti-pattern pack query assistant"
echo "$LOG"
