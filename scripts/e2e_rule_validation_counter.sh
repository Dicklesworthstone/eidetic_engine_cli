#!/usr/bin/env bash
# bd-3qs2i.5.6 - F5.6 e2e for rule validation counters.
#
# Exercises validation_passed / validation_contradicted counters through the
# real CLI and verifies they do not interfere with outcome feedback counters.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

WORKSPACE="${WORKSPACE:-$(mktemp -d -t ee-e2e-f5-rule-validation-XXXX)}"
export WORKSPACE
TEST_NAME="${TEST_NAME:-e2e_rule_validation_counter}"
export TEST_NAME

# shellcheck source=scripts/e2e_lib/agent_ergonomics_lib.sh
# shellcheck disable=SC1091
source "$SCRIPT_DIR/e2e_lib/agent_ergonomics_lib.sh"

ee_workspace() {
    "$EE_BIN" --workspace "$WORKSPACE" "$@"
}

require_jq_value() {
    local json="${1:-}"
    local filter="${2:?jq filter required}"
    local want="${3:-}"
    local label="${4:?assertion label required}"

    local got
    got="$(printf '%s' "$json" | jq -r "$filter" 2>/dev/null || true)"
    if [ "$got" = "$want" ]; then
        record_pass "$label"
        e2e_log_assert_eq "$got" "$want" "$label"
        return 0
    fi

    record_failure "$label" "expected=$want actual=${got:-<empty>}"
    e2e_log_assert_eq "$got" "$want" "$label" || true
    return 1
}

require_contains_text() {
    local haystack="${1:-}"
    local needle="${2:?needle required}"
    local label="${3:?assertion label required}"

    if [[ "$haystack" == *"$needle"* ]]; then
        record_pass "$label"
        e2e_log_assert_eq "contains" "contains" "$label"
        return 0
    fi

    record_failure "$label" "missing substring"
    e2e_log_assert_eq "missing" "contains:$needle" "$label" || true
    return 1
}

if ! command -v jq >/dev/null 2>&1; then
    record_failure "jq_available" "jq is required for rule validation counter e2e"
    exit 1
fi

log_step "Initialize workspace and add candidate rule"
init_out="$(ee_workspace init --json)"
require_jq_value "$init_out" '.success' "true" "init succeeds"
rule_add_out="$(ee_workspace rule add \
    --maturity candidate \
    --scope workspace \
    --tag rust \
    --tag ci \
    --actor e2e-rule-validation-counter \
    "Run cargo fmt before tagging release." \
    --json)"
require_jq_value "$rule_add_out" '.success' "true" "rule add succeeds"
rule_id="$(printf '%s' "$rule_add_out" | jq -r '.data.ruleId // ""')"
if [ -n "$rule_id" ]; then
    record_pass "rule_id_present"
else
    record_failure "rule_id_present" "data.ruleId missing"
    exit 1
fi

log_step "Counters start at zero"
show_out="$(ee_workspace rule show "$rule_id" --json)"
require_jq_value "$show_out" '.success' "true" "initial rule show succeeds"
require_jq_value "$show_out" '.data.rule.validationPasses // 0' "0" \
    "validation passes start at zero"
require_jq_value "$show_out" '.data.rule.validationContradictions // 0' "0" \
    "validation contradictions start at zero"
require_jq_value "$show_out" '.data.rule.positiveFeedbackCount // 0' "0" \
    "helpful outcomes start at zero"

log_step "validation_passed bumps validation_passes only"
mark_out="$(ee_workspace rule mark "$rule_id" \
    --trigger validation_passed \
    --actor e2e-rule-validation-counter \
    --json)"
require_jq_value "$mark_out" '.success' "true" "validation_passed mark succeeds"
show_out="$(ee_workspace rule show "$rule_id" --json)"
require_jq_value "$show_out" '.data.rule.validationPasses' "1" \
    "validation passes is one"
require_jq_value "$show_out" '.data.rule.validationContradictions' "0" \
    "validation contradictions still zero"
require_jq_value "$show_out" '.data.rule.positiveFeedbackCount' "0" \
    "validation_passed does not bump helpful outcomes"

log_step "Repeated validation_passed increments to three"
ee_workspace rule mark "$rule_id" \
    --trigger validation_passed \
    --actor e2e-rule-validation-counter \
    --json >"$LOG_DIR/validation_passed_second.json"
ee_workspace rule mark "$rule_id" \
    --trigger validation_passed \
    --actor e2e-rule-validation-counter \
    --json >"$LOG_DIR/validation_passed_third.json"
show_out="$(ee_workspace rule show "$rule_id" --json)"
require_jq_value "$show_out" '.data.rule.validationPasses' "3" \
    "three validation passes recorded"
require_jq_value "$show_out" '.data.rule.validationContradictions' "0" \
    "repeated validation keeps contradictions zero"

log_step "outcome_helpful does not touch validation counters"
ee_workspace rule mark "$rule_id" \
    --trigger outcome_helpful \
    --helpful-outcomes 1 \
    --actor e2e-rule-validation-counter \
    --json >"$LOG_DIR/outcome_helpful.json"
show_out="$(ee_workspace rule show "$rule_id" --json)"
require_jq_value "$show_out" '.data.rule.validationPasses' "3" \
    "helpful outcome leaves validation passes unchanged"
require_jq_value "$show_out" '.data.rule.validationContradictions' "0" \
    "helpful outcome leaves contradictions unchanged"
require_jq_value "$show_out" '.data.rule.positiveFeedbackCount' "1" \
    "helpful outcome counter increments"

expected_passes=3
log_step "Bulk --validation-passes override adds five when supported"
if ee_workspace rule mark --help 2>&1 | grep -q -- '--validation-passes'; then
    ee_workspace rule mark "$rule_id" \
        --trigger validation_passed \
        --validation-passes 5 \
        --actor e2e-rule-validation-counter \
        --json >"$LOG_DIR/validation_passed_bulk.json"
    expected_passes=8
    show_out="$(ee_workspace rule show "$rule_id" --json)"
    require_jq_value "$show_out" '.data.rule.validationPasses' "$expected_passes" \
        "bulk validation passes add five"
else
    e2e_log_note "validation_passes_override=skipped"
    record_pass "bulk validation passes skipped because flag is absent"
fi

log_step "validation_contradicted bumps contradictions only"
ee_workspace rule mark "$rule_id" \
    --trigger validation_contradicted \
    --actor e2e-rule-validation-counter \
    --json >"$LOG_DIR/validation_contradicted.json"
show_out="$(ee_workspace rule show "$rule_id" --json)"
require_jq_value "$show_out" '.data.rule.validationPasses' "$expected_passes" \
    "contradiction leaves validation passes unchanged"
require_jq_value "$show_out" '.data.rule.validationContradictions' "1" \
    "validation contradiction counter increments"
require_jq_value "$show_out" '.data.rule.positiveFeedbackCount' "1" \
    "validation contradiction leaves helpful outcomes unchanged"

log_step "Tracing target fires for validation counter bump"
trace_log="$LOG_DIR/validation_bump_trace.log"
EE_LOG_JSON=1 RUST_LOG=ee::rule::validation_bump=info \
    "$EE_BIN" --workspace "$WORKSPACE" rule mark "$rule_id" \
    --trigger validation_passed \
    --actor e2e-rule-validation-counter \
    --json >"$LOG_DIR/validation_bump_trace_stdout.json" 2>"$trace_log"
require_contains_text "$(cat "$trace_log")" "ee::rule::validation_bump" \
    "validation bump trace target emitted"
require_contains_text "$(cat "$trace_log")" "validation_passes_delta" \
    "validation bump trace carries pass delta"
require_contains_text "$(cat "$trace_log")" "validation_contradictions_delta" \
    "validation bump trace carries contradiction delta"

# --- bd-3h6bz: Learn -> Retrieve -> Pack loop ------------------------------
# An applied rule must be retrievable by its OWN content via `ee search`
# (rules are first-class source=rule documents in the derived corpus) and
# must hydrate into the pack through its rule_source_memories linkage. The
# source memory content deliberately shares NO tokens with the query, so the
# only way the source memory can appear in the pack for this query is
# through rule-hit hydration — making the rule-attribution assertion
# deterministic instead of racing a direct memory match.
log_step "bd-3h6bz: applied rule is indexed, searchable, and pack-hydratable"
memory_out="$(ee_workspace remember \
    "Incident 2026-Q2 postmortem: a stale formatting baseline shipped." \
    --level semantic --kind fact --json)"
require_jq_value "$memory_out" '.success' "true" \
    "learn-loop source memory remember succeeds"
source_memory_id="$(printf '%s' "$memory_out" | jq -r '.data.memory_id // .data.memoryId // empty')"
if [ -n "$source_memory_id" ]; then
    record_pass "learn_loop_source_memory_id_present"
else
    record_failure "learn_loop_source_memory_id_present" "remember returned no memory id"
    exit 1
fi

learn_rule_out="$(ee_workspace rule add \
    --maturity candidate \
    --scope workspace \
    --tag release \
    --source-memory "$source_memory_id" \
    --actor e2e-rule-validation-counter \
    "Always run the zephyr frobnicator fmt gate before tagging a release." \
    --json)"
require_jq_value "$learn_rule_out" '.success' "true" "learn-loop rule add succeeds"
learn_rule_id="$(printf '%s' "$learn_rule_out" | jq -r '.data.ruleId // empty')"
if [ -n "$learn_rule_id" ]; then
    record_pass "learn_loop_rule_id_present"
else
    record_failure "learn_loop_rule_id_present" "learn-loop rule add returned no ruleId"
    exit 1
fi

rebuild_out="$(ee_workspace index rebuild --json)"
require_jq_value "$rebuild_out" '.success' "true" "learn-loop index rebuild succeeds"

search_out="$(ee_workspace search "zephyr frobnicator fmt gate" --limit 20 --json)"
require_jq_value "$search_out" '.success' "true" "learn-loop search succeeds"
require_jq_value "$search_out" \
    "any(.data.results[]?; ((.docId // .memoryId // \"\") == \"$learn_rule_id\") or ((.content // .contentPreview // \"\") | contains(\"zephyr frobnicator fmt gate\")))" \
    "true" "search returns the applied rule by its own content"

pack_out="$(ee_workspace pack "zephyr frobnicator fmt gate" --max-tokens 2000 --json)"
require_jq_value "$pack_out" '.success' "true" "learn-loop pack succeeds"
require_jq_value "$pack_out" \
    "any(.data.pack.items[]?; .memoryId == \"$source_memory_id\")" \
    "true" "pack hydrates the rule hit through its source memory"
require_jq_value "$pack_out" \
    "any(.data.pack.items[]?; .memoryId == \"$source_memory_id\" and ((.why // \"\") | contains(\"$learn_rule_id\")))" \
    "true" "hydrated pack item attributes the applied procedural rule"
