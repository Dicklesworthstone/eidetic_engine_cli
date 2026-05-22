#!/usr/bin/env bash
# SX7 — Adaptive context-pack budget E2E driver.
#
# This script exercises the public `ee context --explain --json` budget surface
# after enabling `[pack].adaptive_budget`. It does not build `ee`; callers
# provide an existing binary via EE_BINARY or the shared release-binary resolver.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
epic_setup "sx7_adaptive_budget"

enable_json="$(ee_workspace config set pack.adaptive_budget true --json 2>/dev/null || true)"
assert_jq "$enable_json" '.success // false' "true" "sx7_config_set_success"

seed_memory() {
    local ordinal="${1:?ordinal required}"
    local content
    content="SX7 adaptive budget memory ${ordinal}: refactor performance audit verification should receive a larger context budget than a simple lookup."
    ee_workspace remember "$content" \
        --level procedural \
        --kind rule \
        --no-propose-candidates \
        --json >/dev/null 2>&1 || true
}

for ordinal in $(seq 1 20); do
    seed_memory "$ordinal"
done
e2e_log_note "sx7_seeded_memories count=20"

context_json() {
    local query="${1:?query required}"
    shift
    ee_workspace context "$query" --explain --json "$@" 2>/dev/null || true
}

budget_signature() {
    jq -c '.data.pack.budget
      | {
          maxTokens,
          computedTokens,
          multiplier,
          classifierContributions
        }' 2>/dev/null
}

simple_json="$(context_json "simple lookup")"
assert_jq "$simple_json" '.schema' "ee.response.v2" "sx7_simple_schema"
assert_jq "$simple_json" '.success' "true" "sx7_simple_success"
assert_jq "$simple_json" '.data.pack.budget.schema // empty' \
    "ee.context.budget.v1" \
    "sx7_simple_budget_schema"
assert_jq "$simple_json" '.data.pack.budget.adaptive // false' \
    "true" \
    "sx7_simple_adaptive_enabled"
assert_jq "$simple_json" '(.data.pack.budget.computedTokens == .data.pack.budget.maxTokens)' \
    "true" \
    "sx7_simple_computed_budget_applied"
assert_jq "$simple_json" '(.data.pack.budget.computedTokens >= 1000 and .data.pack.budget.computedTokens <= 1200)' \
    "true" \
    "sx7_simple_budget_near_base"

complex_query="refactor performance audit verification adaptive budget retrieval"
complex_json="$(context_json "$complex_query")"
assert_jq "$complex_json" '.schema' "ee.response.v2" "sx7_complex_schema"
assert_jq "$complex_json" '.success' "true" "sx7_complex_success"
assert_jq "$complex_json" '.data.pack.budget.schema // empty' \
    "ee.context.budget.v1" \
    "sx7_complex_budget_schema"
assert_jq "$complex_json" '(.data.pack.budget.computedTokens > 1000)' \
    "true" \
    "sx7_complex_budget_above_base"
assert_jq "$complex_json" '(.data.pack.budget.classifierContributions.taskKeywordScore > 0)' \
    "true" \
    "sx7_complex_keyword_contribution"

simple_tokens="$(printf '%s' "$simple_json" | jq -r '.data.pack.budget.computedTokens // 0')"
complex_tokens="$(printf '%s' "$complex_json" | jq -r '.data.pack.budget.computedTokens // 0')"
if [ "$complex_tokens" -gt "$simple_tokens" ]; then
    e2e_log_assert_eq "true" "true" "sx7_complex_budget_exceeds_simple"
else
    e2e_log_assert_eq "$complex_tokens" ">$simple_tokens" "sx7_complex_budget_exceeds_simple"
fi

explicit_json="$(context_json "$complex_query" --max-tokens 512)"
assert_jq "$explicit_json" '.schema' "ee.response.v2" "sx7_explicit_schema"
assert_jq "$explicit_json" '.success' "true" "sx7_explicit_success"
assert_jq "$explicit_json" '.data.pack.budget.maxTokens' "512" "sx7_explicit_max_tokens_wins"
assert_jq "$explicit_json" '(.data.pack.budget.schema // null) == null' \
    "true" \
    "sx7_explicit_disables_adaptive_override"

complex_again_json="$(context_json "$complex_query")"
complex_sig_1="$(printf '%s' "$complex_json" | budget_signature)"
complex_sig_2="$(printf '%s' "$complex_again_json" | budget_signature)"
e2e_log_assert_eq "$complex_sig_1" "$complex_sig_2" "sx7_budget_signature_deterministic"

e2e_log_note "sx7_summary simple_tokens=${simple_tokens} complex_tokens=${complex_tokens}"
