#!/usr/bin/env bash
# bd-3mgkw.4 - typed memory fields + decide public-CLI E2E.
#
# Scenario:
#   1. init a temp workspace.
#   2. store one fielded memory per typed kind through public CLI surfaces.
#   3. prove exact/contains/prefix --field operators.
#   4. record a decision, refuse a same-topic fork, supersede the head, and
#      surface overdue revisit state.
#   5. prove memory show renders the persisted typed-field sidecar.
#   6. remember --batch --stdin persists fielded JSONL rows and indexes them.
#
# No set -e: harness assertions accumulate failures and harness_summary owns
# the exit code.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_lib.sh
# shellcheck disable=SC1091
source "$E2E_DIR/e2e_lib.sh"

harness_init "typed_fields_decide"

ee_json() {
    e2e_log_command "$EE_BIN" "$@" || true
}

json_value() {
    local json="$1" filter="$2"
    printf '%s' "$json" | jq -r "$filter" 2>/dev/null || true
}

memory_id_from_remember() {
    json_value "$1" '.data.memoryId // .data.memory_id // empty'
}

decision_id_from_record() {
    json_value "$1" '.data.decision.memoryId // empty'
}

assert_search_returns_memory() {
    local json="$1" memory_id="$2" label="$3"
    local result
    result="$(printf '%s' "$json" \
        | jq -e --arg memory_id "$memory_id" \
            '[.data.results[]? | select((.memoryId // .docId) == $memory_id)] | length == 1' \
            >/dev/null 2>&1 && printf true || printf false)"
    e2e_log_assert_eq "$result" "true" "$label" || true
    if [ "$result" = "true" ]; then
        _harness_pass "$label"
    else
        _harness_fail "$label: search results did not contain $memory_id"
    fi
}

assert_search_omits_memory() {
    local json="$1" memory_id="$2" label="$3"
    local result
    result="$(printf '%s' "$json" \
        | jq -e --arg memory_id "$memory_id" \
            '[.data.results[]? | select((.memoryId // .docId) == $memory_id)] | length == 0' \
            >/dev/null 2>&1 && printf true || printf false)"
    e2e_log_assert_eq "$result" "true" "$label" || true
    if [ "$result" = "true" ]; then
        _harness_pass "$label"
    else
        _harness_fail "$label: search results unexpectedly contained $memory_id"
    fi
}

assert_jq_arg() {
    local json="$1" arg_name="$2" arg_value="$3" filter="$4" label="$5"
    local result
    result="$(printf '%s' "$json" \
        | jq -e --arg "$arg_name" "$arg_value" "$filter" >/dev/null 2>&1 \
        && printf true || printf false)"
    e2e_log_assert_eq "$result" "true" "$label" || true
    if [ "$result" = "true" ]; then
        _harness_pass "$label"
    else
        _harness_fail "$label: jq filter false [$filter]"
    fi
}

assert_jq_two_args() {
    local json="$1" first_name="$2" first_value="$3" second_name="$4" second_value="$5" filter="$6" label="$7"
    local result
    result="$(printf '%s' "$json" \
        | jq -e --arg "$first_name" "$first_value" --arg "$second_name" "$second_value" "$filter" >/dev/null 2>&1 \
        && printf true || printf false)"
    e2e_log_assert_eq "$result" "true" "$label" || true
    if [ "$result" = "true" ]; then
        _harness_pass "$label"
    else
        _harness_fail "$label: jq filter false [$filter]"
    fi
}

remember_fielded_memory() {
    local kind="$1" body="$2" source="$3"
    shift 3
    ee_json --workspace "$WS" remember "$body" \
        --level procedural --kind "$kind" --source "$source" "$@" --json
}

with_temp_workspace WS

step "init typed-fields decide workspace"
init_out="$(ee_json --workspace "$WS" init --json)"
assert_jq "$init_out" '.success == true' "ee init succeeds"

step "remember fielded failure memory"
failure_body="Tried page-cache WAL prefetch; small-N reads regressed and the change was reverted."
failure_out="$(remember_fielded_memory "failure" "$failure_body" "test://typed-fields-decide/failure" \
    --field "family=page-cache wal" --field "cause=cache pollution" \
    --field "regression-surface=shard read pool" --field "reverted-at-sha=9af3c21")"
assert_jq "$failure_out" '.success == true and .data.kind == "failure" and .data.persisted == true' \
    "failure memory persisted"
assert_jq "$failure_out" '.data.typedFields.family == "page-cache wal" and .data.typedFields.cause == "cache pollution"' \
    "remember response reports explicit failure fields"
failure_id="$(memory_id_from_remember "$failure_out")"
family_search="$(ee_json --workspace "$WS" search "page-cache wal" \
    --kind failure --field "family=page-cache wal" --json)"
assert_jq "$family_search" '.success == true' "failure family exact field search succeeds"
assert_search_returns_memory "$family_search" "$failure_id" "failure family exact search returns fielded memory"

step "remember fielded command memory"
command_body="Use the remote verification wrapper before closing typed-fields work."
command_out="$(remember_fielded_memory "command" "$command_body" "test://typed-fields-decide/command" \
    --field "command=scripts/rch_verify.sh -- cargo check --all-targets" \
    --field "when-to-use=before closing typed-fields decide work" \
    --field "exit-meaning=0 means remote proof passed")"
assert_jq "$command_out" '.success == true and .data.kind == "command"' "command memory persisted"
command_id="$(memory_id_from_remember "$command_out")"
command_search="$(ee_json --workspace "$WS" search "remote proof" \
    --kind command --field "command^scripts/rch_verify.sh" --json)"
assert_search_returns_memory "$command_search" "$command_id" "command prefix search returns fielded memory"

step "remember fielded rule memory"
rule_body="Protect the typed-field regression contract with the public E2E harness."
rule_out="$(remember_fielded_memory "rule" "$rule_body" "test://typed-fields-decide/rule" \
    --field "condition=typed field regression" \
    --field "action=run scripts/e2e_typed_fields_decide.sh" \
    --field "exceptions=docs only" --field "exceptions=read-only review")"
assert_jq "$rule_out" '.success == true and .data.kind == "rule"' "rule memory persisted"
rule_id="$(memory_id_from_remember "$rule_out")"
rule_search="$(ee_json --workspace "$WS" search "typed field regression" \
    --kind rule --field "condition~field regression" --json)"
assert_search_returns_memory "$rule_search" "$rule_id" "rule contains search returns fielded memory"

step "remember fielded convention memory"
convention_body="Public CLI JSON envelopes define the typed-fields test contract."
convention_out="$(remember_fielded_memory "convention" "$convention_body" "test://typed-fields-decide/convention" \
    --field "scope=decide typed-fields tests" \
    --field "pattern=use public CLI JSON envelopes for contract tests")"
assert_jq "$convention_out" '.success == true and .data.kind == "convention"' "convention memory persisted"
convention_id="$(memory_id_from_remember "$convention_out")"
convention_search="$(ee_json --workspace "$WS" search "public CLI" \
    --kind convention --field "scope=decide typed-fields tests" --json)"
assert_search_returns_memory "$convention_search" "$convention_id" "convention exact search returns fielded memory"

step "remember fielded risk and anti-pattern memories"
risk_body="Local Cargo can fill the internal SSD during an RCH-only swarm."
risk_out="$(remember_fielded_memory "risk" "$risk_body" "test://typed-fields-decide/risk" \
    --field "trigger=local Cargo during RCH-only swarm" \
    --field "blast-radius=fills internal SSD" \
    --field "safer-alternative=scripts/rch_verify.sh")"
assert_jq "$risk_out" '.success == true and .data.kind == "risk"' "risk memory persisted"
risk_id="$(memory_id_from_remember "$risk_out")"
risk_search="$(ee_json --workspace "$WS" search "internal SSD" \
    --kind risk --field "trigger~Cargo" --json)"
assert_search_returns_memory "$risk_search" "$risk_id" "risk contains search returns fielded memory"

antipattern_body="Fake abstention can close beads without working code."
antipattern_out="$(remember_fielded_memory "anti-pattern" "$antipattern_body" "test://typed-fields-decide/anti-pattern" \
    --field "trigger=fake abstention as implementation" \
    --field "blast-radius=closes beads without working code" \
    --field "safer-alternative=ship real tests and source")"
assert_jq "$antipattern_out" '.success == true and .data.kind == "anti-pattern"' \
    "anti-pattern memory persisted"
antipattern_id="$(memory_id_from_remember "$antipattern_out")"
antipattern_search="$(ee_json --workspace "$WS" search "fake abstention" \
    --kind anti-pattern --field "safer-alternative^ship real" --json)"
assert_search_returns_memory "$antipattern_search" "$antipattern_id" \
    "anti-pattern prefix search returns fielded memory"

step "note and dry-run expose explicit typed fields without mutation"
note_out="$(ee_json --workspace "$WS" note "A note about the prefetch family." \
    --kind failure --field "family=note-prefetch" --dry-run --json)"
assert_jq "$note_out" '.success == true and .data.dry_run == true and .data.persisted == false and .data.typedFields.family == "note-prefetch"' \
    "note dry-run validates and reports explicit fields"

step "decide record stores exact typed fields"
first_out="$(ee_json --workspace "$WS" decide record "Storage layer topic" \
    --chosen "FrankenSQLite" \
    --alternative "SQLx" \
    --rationale "Keep durable state in FrankenSQLite." \
    --revisit-by "2026-06-14T12:00:00Z" \
    --json)"
assert_jq "$first_out" '.success == true and .data.schema == "ee.decide.record.v1" and .data.decision.chosen == "FrankenSQLite"' \
    "first decision recorded"
first_id="$(decision_id_from_record "$first_out")"
first_show="$(ee_json --workspace "$WS" memory show "$first_id" --json)"
assert_jq "$first_show" '.success == true and .data.memory.typedFields.chosen == "FrankenSQLite" and .data.memory.typedFields.options[0] == "FrankenSQLite"' \
    "memory show renders first decision typed fields"

step "decide refuses same-topic fork without supersedes"
duplicate_stdout="$LOG_DIR/duplicate-fork.json"
duplicate_stderr="$LOG_DIR/duplicate-fork.err"
"$EE_BIN" --workspace "$WS" decide record "Storage layer topic" \
    --chosen "SQLx" \
    --alternative "FrankenSQLite" \
    --rationale "This fork should require explicit supersedes." \
    --json >"$duplicate_stdout" 2>"$duplicate_stderr"
duplicate_rc=$?
duplicate_json="$(cat "$duplicate_stdout")"
assert_eq "$duplicate_rc" "1" "same-topic fork exits usage"
assert_jq "$duplicate_json" '.schema == "ee.error.v2" and .error.code == "decision_topic_requires_supersedes"' \
    "same-topic fork returns stable error code"

step "decide supersedes prior decision and revisit surfaces overdue head"
second_out="$(ee_json --workspace "$WS" decide record "Storage layer topic" \
    --chosen "RCH remote" \
    --alternative "local Cargo" \
    --rationale "Remote proof avoids local build artifacts." \
    --supersedes "$first_id" \
    --revisit-by "2026-06-14T12:00:00Z" \
    --json)"
assert_jq "$second_out" '.success == true and .data.decision.chainDepth == 1 and .data.superseded.memoryId != null' \
    "superseding decision recorded"
second_id="$(decision_id_from_record "$second_out")"
list_out="$(ee_json --workspace "$WS" decide list --json)"
assert_jq_arg "$list_out" "second_id" "$second_id" \
    '.success == true and .data.returnedCount == 1 and .data.decisions[0].memoryId == $second_id' \
    "decide list returns only live head"
history_out="$(ee_json --workspace "$WS" decide list --include-superseded --json)"
assert_jq_two_args "$history_out" "first_id" "$first_id" "second_id" "$second_id" \
    '.success == true and .data.returnedCount == 2 and any(.data.decisions[]; .memoryId == $first_id and .superseded == true and (.validTo | type) == "string") and any(.data.decisions[]; .memoryId == $second_id and .superseded == false)' \
    "decide history returns superseded and live decisions"
revisit_out="$(ee_json --workspace "$WS" decide revisit --warning-days 30 --json)"
assert_jq_arg "$revisit_out" "second_id" "$second_id" \
    '.success == true and .data.dueCount == 1 and .data.decisions[0].memoryId == $second_id and .data.decisions[0].revisitStatus == "overdue"' \
    "decide revisit surfaces overdue live head"

step "decision field operators select live superseding decision"
exact_out="$(ee_json --workspace "$WS" search "remote proof" \
    --kind decision --field "chosen=RCH remote" --json)"
assert_jq "$exact_out" '.success == true' "decision exact field search succeeds"
assert_search_returns_memory "$exact_out" "$second_id" "decision exact field search returns superseding decision"
assert_search_omits_memory "$exact_out" "$first_id" "decision exact field search omits prior choice"
contains_out="$(ee_json --workspace "$WS" search "remote proof" \
    --kind decision --field "chosen~remote" --json)"
assert_search_returns_memory "$contains_out" "$second_id" "decision contains field search returns superseding decision"
prefix_out="$(ee_json --workspace "$WS" search "remote proof" \
    --kind decision --field "chosen^RCH" --json)"
assert_search_returns_memory "$prefix_out" "$second_id" "decision prefix field search returns superseding decision"
second_show="$(ee_json --workspace "$WS" memory show "$second_id" --json)"
assert_jq_arg "$second_show" "first_id" "$first_id" \
    '.success == true and .data.memory.typedFields.chosen == "RCH remote" and .data.memory.typedFields.supersedes == $first_id and .data.memory.typedFields.revisit_by == "2026-06-14T12:00:00Z"' \
    "memory show renders superseding decision typed fields"

step "remember batch JSONL writes explicit fields objects and indexes them"
batch_payload="$LOG_DIR/typed-fields-batch.jsonl"
printf '%s\n' \
    '{"content":"Inspect current decision heads with the canonical command.","level":"procedural","kind":"command","fields":{"command":"ee decide list --json","when_to_use":"inspect current decision heads","exit_meaning":"0 means listed"},"source":"test://typed-fields-decide/batch-command"}' \
    '{"content":"Protect the typed field batch regression.","level":"procedural","kind":"rule","fields":{"condition":"typed field batch regression","action":"run scripts/e2e_typed_fields_decide.sh","exceptions":["docs only"]},"source":"test://typed-fields-decide/batch-rule"}' \
    >"$batch_payload"
e2e_log_note "command remember batch --stdin payload=$batch_payload"
batch_out="$("$EE_BIN" --workspace "$WS" remember --batch --stdin --json <"$batch_payload")"
batch_rc=$?
assert_eq "$batch_rc" "0" "remember batch exits zero"
assert_jq "$batch_out" '.success == true and .data.mode == "batch" and .data.storedCount == 2 and .data.failedCount == 0' \
    "remember batch stores both fielded rows"
batch_command_id="$(json_value "$batch_out" '.data.results[0].memoryId // empty')"
batch_rule_id="$(json_value "$batch_out" '.data.results[1].memoryId // empty')"
batch_command_search="$(ee_json --workspace "$WS" search "decide list" \
    --kind command --field "command=ee decide list --json" --json)"
assert_search_returns_memory "$batch_command_search" "$batch_command_id" \
    "batch command exact field search returns stored row"
batch_rule_search="$(ee_json --workspace "$WS" search "batch regression" \
    --kind rule --field "condition~batch regression" --json)"
assert_search_returns_memory "$batch_rule_search" "$batch_rule_id" \
    "batch rule contains field search returns stored row"

end_temp_workspace
summary_rc=0
harness_summary || summary_rc=$?
printf 'Artifacts: %s\n' "$LOG_DIR" >&2
exit "$summary_rc"
