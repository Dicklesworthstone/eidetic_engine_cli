#!/usr/bin/env bash
# N7.1 — Bayesian memory posterior e2e driver.
#
# Exercises the public posterior surfaces introduced by bd-17c65.14.7.2:
# Jeffreys-prior why output, helpful/harmful outcome updates, and the
# outcome.bayes_update audit action. The migration-backfill modes are pinned by
# golden fixtures because this script starts from a fresh v0.2 workspace.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
epic_setup "bayes_posteriors"

MEMORY_JSON=$(ee_workspace remember \
    "Run cargo fmt --check before cutting a release." \
    --level procedural --kind rule --json)
MEMORY_ID=$(printf '%s' "$MEMORY_JSON" | jq -r '.data.memory_id')
e2e_log_note "bayes_e2e_memory_id=$MEMORY_ID"

WHY_INITIAL=$(ee_workspace why "$MEMORY_ID" --json)
assert_jq "$WHY_INITIAL" '.data.bayesPosterior.schema' "ee.bayes.posterior.v1" \
    "bayes_initial_schema"
assert_jq "$WHY_INITIAL" '.data.bayesPosterior.alpha' "0.5" \
    "bayes_initial_alpha"
assert_jq "$WHY_INITIAL" '.data.bayesPosterior.beta' "0.5" \
    "bayes_initial_beta"
assert_jq "$WHY_INITIAL" '.data.bayesPosterior.effectiveSampleSize' "1" \
    "bayes_initial_effective_sample_size"

ee_workspace outcome "$MEMORY_ID" --signal helpful \
    --reason "The rule helped the release check." --json >/dev/null
WHY_HELPFUL=$(ee_workspace why "$MEMORY_ID" --json)
assert_jq "$WHY_HELPFUL" '.data.bayesPosterior.alpha' "1.5" \
    "bayes_helpful_alpha"
assert_jq "$WHY_HELPFUL" '.data.bayesPosterior.beta' "0.5" \
    "bayes_helpful_beta"

ee_workspace outcome "$MEMORY_ID" --signal harmful \
    --reason "The rule missed a release failure." --json >/dev/null
WHY_HARMFUL=$(ee_workspace why "$MEMORY_ID" --json)
assert_jq "$WHY_HARMFUL" '.data.bayesPosterior.alpha' "1.5" \
    "bayes_harmful_alpha"
assert_jq "$WHY_HARMFUL" '.data.bayesPosterior.beta' "3" \
    "bayes_harmful_beta_default_weight"
assert_jq "$WHY_HARMFUL" '.data.bayesPosterior.credibleInterval90.level' "0.9" \
    "bayes_harmful_ci90_level"

AUDIT_JSON=$(ee_workspace audit timeline --action outcome.bayes_update --limit 10 --json)
assert_jq "$AUDIT_JSON" '[.entries[]? | select(.mutation_kind == "outcome.bayes_update")] | length' \
    "2" "bayes_outcome_audit_row_count"

BAYES_GOLDEN_DIR="$REPO_ROOT/tests/fixtures/golden/bayes"
for fixture in \
    jeffreys_default_backfill.json \
    utility_derived_backfill.json \
    feedback_replay_backfill.json \
    outcome_update.json; do
    if jq -e '.schema == "ee.bayes.golden.v1" and (.memories | length >= 1)' \
        "$BAYES_GOLDEN_DIR/$fixture" >/dev/null; then
        e2e_log_assert_eq "true" "true" "bayes_fixture_${fixture}_shape"
    else
        e2e_log_assert_eq "invalid" "valid" "bayes_fixture_${fixture}_shape"
    fi
done
