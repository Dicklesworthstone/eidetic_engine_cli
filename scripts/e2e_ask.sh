#!/usr/bin/env bash
# bd-169v0.5 - Ask public CLI end-to-end coverage.
#
# Real-binary, no-Cargo E2E for `ee ask`:
#   - direct citation-backed answer
#   - multi-memory corroboration confidence lift
#   - explicit contradicts link plus conflict answer sides
#   - calibrated abstention and fail-closed `--require-confidence`
#   - semantic-degraded honesty row in the response envelope
#
# The script intentionally keeps its temp workspace and command artifacts. The
# shared harness cleanup path uses `rm -rf`, which AGENTS.md forbids for this
# repo, so retained artifacts are the safe default.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_lib.sh
# shellcheck disable=SC1091
source "$E2E_DIR/e2e_lib.sh"

export EE_E2E_KEEP_ARTIFACTS="${EE_E2E_KEEP_ARTIFACTS:-1}"

harness_init "ask"

json_value() {
    local json="$1" filter="$2"
    printf '%s' "$json" | jq -r "$filter" 2>/dev/null || true
}

memory_id_from_remember() {
    json_value "$1" '.data.memoryId // .data.memory_id // empty'
}

finish() {
    local rc="${1:-0}"
    unset EE_DATABASE_PATH EE_INDEX_DIR
    summary_rc=0
    harness_summary || summary_rc=$?
    printf 'Artifacts: %s\n' "$LOG_DIR" >&2
    if [ "$rc" -ne 0 ]; then
        exit "$rc"
    fi
    exit "$summary_rc"
}

fail_now() {
    local label="$1" payload="${2:-}"
    _harness_fail "$label"
    if [ -n "$payload" ]; then
        e2e_log_note "failure_payload label=$label payload=$payload"
        printf '[ask-e2e] failure payload (%s): %s\n' "$label" "$payload" >&2
    fi
    finish 1
}

pass_now() {
    local label="$1"
    e2e_log_assert_eq "true" "true" "$label" || true
    _harness_pass "$label"
}

assert_json_filter() {
    local json="$1" filter="$2" label="$3"
    if printf '%s' "$json" | jq -e "$filter" >/dev/null 2>&1; then
        pass_now "$label"
    else
        local payload="$LOG_DIR/${label//[^A-Za-z0-9_]/_}.failed.json"
        printf '%s\n' "$json" >"$payload"
        e2e_log_assert_eq "false" "true" "$label" || true
        fail_now "$label" "$payload"
    fi
}

assert_json_filter_arg() {
    local json="$1" arg_name="$2" arg_value="$3" filter="$4" label="$5"
    if printf '%s' "$json" | jq -e --arg "$arg_name" "$arg_value" "$filter" >/dev/null 2>&1; then
        pass_now "$label"
    else
        local payload="$LOG_DIR/${label//[^A-Za-z0-9_]/_}.failed.json"
        printf '%s\n' "$json" >"$payload"
        e2e_log_assert_eq "false" "true" "$label" || true
        fail_now "$label" "$payload"
    fi
}

run_json() {
    local label="$1"
    shift
    LAST_STDOUT_FILE="$LOG_DIR/${label}.stdout.json"
    if e2e_log_command "$EE_BIN" "$@" >"$LAST_STDOUT_FILE"; then
        LAST_RC=0
    else
        LAST_RC=$?
    fi
    LAST_JSON="$(cat "$LAST_STDOUT_FILE")"
    e2e_log_note "command_label=$label rc=$LAST_RC stdout=$LAST_STDOUT_FILE"
}

assert_rc() {
    local expected="$1" label="$2"
    if [ "${LAST_RC:-999}" = "$expected" ]; then
        pass_now "$label"
    else
        fail_now "$label" "${LAST_STDOUT_FILE:-}"
    fi
}

remember_memory() {
    local label="$1" level="$2" kind="$3" confidence="$4" source="$5" content="$6"
    run_json "$label" --workspace "$WS" --json remember \
        --level "$level" --kind "$kind" --confidence "$confidence" \
        --tags ask,e2e --source "$source" \
        --no-auto-link --no-propose-candidates \
        "$content"
    assert_rc 0 "$label exits zero"
    assert_json_filter "$LAST_JSON" '.success == true and .data.persisted == true' "$label persists"
    LAST_MEMORY_ID="$(memory_id_from_remember "$LAST_JSON")"
    if [ -z "$LAST_MEMORY_ID" ]; then
        fail_now "$label returns a memory ID" "$LAST_STDOUT_FILE"
    fi
}

with_temp_workspace WS

step "init ask workspace"
run_json "00-init" --workspace "$WS" --json init
assert_rc 0 "init exits zero"
assert_json_filter "$LAST_JSON" '.success == true' "init succeeds"

step "seed answer, corroboration, conflict, and abstention memories"
remember_memory "01-remember-toolchain" procedural fact 0.99 \
    "manual://ask_e2e/toolchain" \
    "Zephyr toolchain Rust nightly 1.96.0 active toolchain release verification."
toolchain_id="$LAST_MEMORY_ID"
remember_memory "02-remember-release-primary" procedural rule 0.99 \
    "manual://ask_e2e/release-primary" \
    "Project Zephyr release readiness requires smoke gate alpha before deploy."
release_primary_id="$LAST_MEMORY_ID"
remember_memory "04-remember-conflict-affirm" episodic observation 0.99 \
    "manual://ask_e2e/remote-cache-affirm" \
    "Remote cache delta enabled Project Zephyr hz2 worker pool."
remote_affirm_id="$LAST_MEMORY_ID"
remember_memory "05-remember-conflict-negate" episodic observation 0.99 \
    "manual://ask_e2e/remote-cache-negate" \
    "Zephyr hz2 workers cannot use cache delta."
remote_negate_id="$LAST_MEMORY_ID"
remember_memory "06-remember-abstention-distractor" semantic fact 0.70 \
    "manual://ask_e2e/invoice-distractor" \
    "Project Zephyr billing sandbox invoice identifiers are unrelated to approval flows."

run_json "07-link-contradicts" --workspace "$WS" --json link \
    "$remote_affirm_id" "$remote_negate_id" --relation contradicts
assert_rc 0 "explicit contradicts link exits zero"
assert_json_filter "$LAST_JSON" '.success == true' "explicit contradicts link persists"

run_json "08-conflict-explain" --workspace "$WS" --json conflict explain "$remote_affirm_id"
assert_rc 0 "conflict explain exits zero"
assert_json_filter_arg "$LAST_JSON" "memory_id" "$remote_negate_id" \
    'any(.data.pairs[]?; .memoryA.id == $memory_id or .memoryB.id == $memory_id)' \
    "conflict explain references the opposing memory"

step "direct answer cites resolvable stored memory"
run_json "09-ask-direct" --workspace "$WS" --json ask \
    "Zephyr toolchain Rust nightly 1.96.0 active toolchain release verification"
assert_rc 0 "direct ask exits zero"
direct_json="$LAST_JSON"
assert_json_filter "$direct_json" \
    '.schema == "ee.response.v2" and .success == true and .data.schema == "ee.ask.v1" and .data.abstained == false and (.data.answerText | contains("Rust nightly 1.96.0"))' \
    "direct ask returns answer payload"
assert_json_filter_arg "$direct_json" "memory_id" "$toolchain_id" \
    'any(.data.citations[]?; .memoryId == $memory_id)' \
    "direct ask cites the toolchain memory"
assert_json_filter "$direct_json" \
    'any(.degraded[]?; .code == "ask_semantic_degraded" and .severity == "info") and .data._semanticDegraded == true' \
    "direct ask surfaces semantic degraded honesty row"

run_json "10-memory-show-citation" --workspace "$WS" --json memory show "$toolchain_id"
assert_rc 0 "cited memory show exits zero"
assert_json_filter_arg "$LAST_JSON" "memory_id" "$toolchain_id" \
    'any(.. | objects | (.id? // .memoryId? // .memory_id? // empty); . == $memory_id)' \
    "direct citation resolves through memory show"

step "lexical release rule answers the question without semantic scoring"
remember_memory "10b-remember-format-rule" procedural rule 0.85 \
    "manual://ask_e2e/release-tag-format" \
    "Run cargo fmt --check before every release tag."
format_rule_id="$LAST_MEMORY_ID"
run_json "10c-ask-release-tag" --workspace "$WS" --json ask \
    "Which command must run before every release tag?"
assert_rc 0 "release tag ask exits zero"
assert_json_filter_arg "$LAST_JSON" "memory_id" "$format_rule_id" \
    '.success == true and .data.abstained == false and (.data.answerText | contains("cargo fmt --check")) and (.data.citations | length) == 1 and .data.citations[0].memoryId == $memory_id' \
    "release tag ask returns exactly the grounded format rule"
assert_json_filter "$LAST_JSON" \
    'any(.degraded[]?; .code == "ask_semantic_degraded")' \
    "lexical answer preserves semantic degradation evidence"
run_json "10d-ask-unrelated-dashboard" --workspace "$WS" --json ask \
    "What colour is the CI dashboard?"
assert_rc 0 "unrelated dashboard ask exits zero"
assert_json_filter "$LAST_JSON" \
    '.success == true and .data.abstained == true and (.data.citations | length) == 0' \
    "unrelated dashboard question abstains without citations"

step "multi-memory corroboration raises confidence components"
run_json "11a-ask-single-release" --workspace "$WS" --json ask \
    "Project Zephyr release readiness smoke gate alpha before deploy"
assert_rc 0 "single release answer exits zero"
single_release_confidence="$(json_value "$LAST_JSON" '.data.confidence')"
assert_json_filter_arg "$LAST_JSON" "memory_id" "$release_primary_id" \
    '.data.abstained == false and (.data.citations | length) == 1 and .data.citations[0].memoryId == $memory_id and .data.confidenceComponents.corroboration == 1.0' \
    "single release answer has one uncorroborated source"
remember_memory "11b-remember-release-support" episodic decision 0.96 \
    "manual://ask_e2e/release-support" \
    "Project Zephyr release readiness requires smoke gate alpha before deploy."
run_json "11-ask-corroborated" --workspace "$WS" --json ask \
    "Project Zephyr release readiness smoke gate alpha before deploy"
assert_rc 0 "corroborated ask exits zero"
corroborated_json="$LAST_JSON"
assert_json_filter "$corroborated_json" \
    '.success == true and .data.abstained == false and (.data.confidenceComponents.corroboration > 1.0)' \
    "corroborated ask reports confidence lift"
assert_json_filter_arg "$corroborated_json" "memory_id" "$release_primary_id" \
    'any(.data.citations[]?; .memoryId == $memory_id)' \
    "corroborated ask cites primary release memory"
corroborated_confidence="$(json_value "$corroborated_json" '.data.confidence')"
if awk -v single="$single_release_confidence" -v corroborated="$corroborated_confidence" \
    'BEGIN { exit !(corroborated > single) }'; then
    pass_now "adding corroboration raises confidence for the same question"
else
    fail_now "corroborated confidence regressed" "$LOG_DIR/11-ask-corroborated.stdout.json"
fi

step "conflicting evidence emits sides and warning degraded code"
run_json "12-ask-conflict" --workspace "$WS" --json ask \
    "Remote cache delta enabled Project Zephyr hz2 worker pool"
assert_rc 0 "conflict ask exits zero"
assert_json_filter "$LAST_JSON" \
    '.success == true and .data.abstained == false and .data._conflictDetected == true and (.data.sides | length) == 2' \
    "conflict ask emits two sides"
assert_json_filter "$LAST_JSON" \
    'any(.degraded[]?; .code == "ask_conflicting_evidence" and .severity == "warning")' \
    "conflict ask emits warning degraded code"
assert_json_filter_arg "$LAST_JSON" "memory_id" "$remote_affirm_id" \
    'any(.data.sides[]?.citations[]?; .memoryId == $memory_id)' \
    "conflict ask cites affirming memory"
assert_json_filter_arg "$LAST_JSON" "memory_id" "$remote_negate_id" \
    'any(.data.sides[]?.citations[]?; .memoryId == $memory_id)' \
    "conflict ask cites negating memory"
assert_json_filter_arg "$LAST_JSON" "memory_id" "$remote_affirm_id" \
    '.data.conflictLink.id != null and .data.conflictLink.srcMemoryId == $memory_id and .data.conflictLink.source == "agent"' \
    "conflict ask exposes the stored contradiction edge"

run_json "12b-conflict-require-confidence" --workspace "$WS" --json ask \
    "Is remote cache delta enabled for Project Zephyr?" --require-confidence 0.95
assert_rc 6 "conflict confidence requirement still uses penalized confidence"
assert_json_filter "$LAST_JSON" \
    '.schema == "ee.error.v2" and .error.code == "unsatisfied_degraded_mode"' \
    "supported conflict sides cannot bypass require-confidence"

step "unanswerable question abstains and fail-closed mode exits 6"
run_json "13-ask-abstain" --workspace "$WS" --json ask \
    "Who approved the lunar invoice for Project Zephyr?"
assert_rc 0 "abstention ask exits zero"
assert_json_filter "$LAST_JSON" \
    '.success == true and .data.abstained == true and (.data.nearestEvidence | length) >= 1 and (.data.counterfactualHint | type == "string")' \
    "abstention payload includes nearest evidence and hint"
assert_json_filter "$LAST_JSON" \
    'any(.degraded[]?; .code == "no_confident_answer" and .severity == "info")' \
    "abstention ask emits no_confident_answer"
log_drop 1 "ask abstained because no memory reached the confidence threshold"

run_json "14-ask-require-confidence" --workspace "$WS" --json ask \
    "Who approved the lunar invoice for Project Zephyr?" --require-confidence 0.95
assert_rc 6 "require-confidence exits with degraded code 6"
assert_json_filter "$LAST_JSON" \
    '.schema == "ee.error.v2" and .error.code == "unsatisfied_degraded_mode"' \
    "require-confidence emits structured error envelope"

step "ask eval fixture remains discoverable through public eval surface"
run_json "15-eval-ask-v1" --json eval run ask_v1
assert_rc 0 "ask_v1 quality gate exits zero"
assert_json_filter "$LAST_JSON" \
    '.schema == "ee.response.v2" and .success == true and .data.report.fixture_id == "ask_v1" and .data.report.fixture_family == "ask" and .data.report.status == "passed" and (.data.report.metrics.queries_evaluated > 0)' \
    "ask_v1 quality gate reports passing fixture metrics"

e2e_log_note "ask_e2e_workspace_retained=$WS"
finish 0
