#!/usr/bin/env bash
# bd-1oep7 - Consolidation Maintain-loop e2e (real binary, no mocks).
#
# Proves the fifth core job closes the consolidation loop through public
# surfaces only: duplicate fixture -> steward consolidation_pass (dry-run
# non-mutation proof, zero-item-budget cancellation, real run, dedupe
# determinism) -> ee curate validate/apply (consolidate-absorb: derived_from
# lineage, tombstoned duplicate, preserved survivor, append-only audit chain)
# -> workflow-emitted index_coalesce (no manual rebuild) -> truthful
# generation/per-kind counts -> deduplicated search -> idempotent re-runs.
# The script intentionally does not build; central RCH verify provides the
# binary.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [ -d /private/tmp ]; then
    EE_E2E_TMPDIR="${EE_E2E_TMPDIR:-/private/tmp}"
    export EE_E2E_TMPDIR
fi

# shellcheck source=scripts/e2e_lib.sh
# shellcheck disable=SC1091
source "$E2E_DIR/e2e_lib.sh"

harness_init "consolidation"

ee_json() {
    e2e_log_command "$EE_BIN" "$@" || true
}

json_scalar() {
    local json="${1:?json required}"
    local filter="${2:?jq filter required}"
    printf '%s' "$json" | jq -r "$filter" 2>/dev/null || true
}

with_temp_workspace WS

GROUP_PHRASE="Zephyr quill consolidation gate: run cargo fmt --check before release."
DUPLICATE_PHRASE="  zephyr   QUILL consolidation gate: run cargo fmt --check before release. "
WORDING_CONTROL_PHRASE="Zephyr quill consolidation gate: run cargo fmt --check before publishing."
SEARCH_QUERY="zephyr quill consolidation gate"

step "init isolated workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.schema == "ee.response.v2" and .success == true' \
    "ee init returns a success response envelope"
log_event "consolidation_workspace" \
    workspaceHash "$(printf '%s' "$WS" | shasum -a 256 | awk '{print $1}')" \
    bead "bd-1oep7"

step "seed duplicates plus near-but-not-eligible controls"
remember_memory() {
    local content="$1" kind="$2" confidence="$3" label="$4"
    local out
    out="$(ee_json remember "$content" --workspace "$WS" \
        --level semantic --kind "$kind" --confidence "$confidence" \
        --no-propose-candidates --no-auto-link --json)"
    assert_jq "$out" '.schema == "ee.response.v2" and .success == true' \
        "remember $label succeeds"
    json_scalar "$out" '.data.memory_id // .data.memoryId // empty'
}
SURVIVOR_ID="$(remember_memory "$GROUP_PHRASE" fact 0.9 survivor)"
DUPLICATE_ID="$(remember_memory "$DUPLICATE_PHRASE" fact 0.4 duplicate)"
WORDING_CONTROL_ID="$(remember_memory "$WORDING_CONTROL_PHRASE" fact 0.4 wording_control)"
KIND_CONTROL_ID="$(remember_memory "$GROUP_PHRASE" decision 0.4 kind_control)"
if [ -z "$SURVIVOR_ID" ] || [ -z "$DUPLICATE_ID" ] || [ -z "$WORDING_CONTROL_ID" ] || [ -z "$KIND_CONTROL_ID" ]; then
    e2e_log_assert_eq "missing" "present" "consolidation fixture memory ids created"
    harness_summary
    exit 1
fi
log_event "consolidation_fixture" \
    survivorId "$SURVIVOR_ID" duplicateId "$DUPLICATE_ID" \
    wordingControlId "$WORDING_CONTROL_ID" kindControlId "$KIND_CONTROL_ID"

step "workflow-emitted indexing establishes a truthful baseline"
coalesce_out="$(ee_json daemon --workspace "$WS" --foreground --once --job index_coalesce --json)"
assert_jq "$coalesce_out" '.schema == "ee.response.v2" and .success == true' \
    "baseline index_coalesce succeeds"
baseline_status="$(ee_json index status --workspace "$WS" --json)"
assert_jq "$baseline_status" '.data.dbGeneration != null and .data.dbGeneration == .data.indexGeneration' \
    "baseline generation is truthful"
assert_jq "$baseline_status" '.data.indexDocumentCounts.memories == 4 and .data.health == "ready"' \
    "baseline index holds all four fixture memories"
BASELINE_DB_GEN="$(json_scalar "$baseline_status" '.data.dbGeneration')"

step "dry-run consolidation_pass plans without mutating"
dry_out="$(ee_json daemon --workspace "$WS" --foreground --once --job consolidation_pass --dry-run --json)"
assert_jq "$dry_out" '.data.ticks[0].runner.results[0].details.dryRun == true' \
    "dry-run reports dryRun"
assert_jq "$dry_out" '.data.ticks[0].runner.results[0].details.plannedCandidates == 1' \
    "dry-run plans exactly one candidate"
assert_jq "$dry_out" '.data.ticks[0].runner.results[0].details.insertedCandidates == 0 and .data.ticks[0].runner.results[0].details.durableMutation == false' \
    "dry-run inserts nothing"
dry_candidates="$(ee_json curate candidates --workspace "$WS" --type consolidate --all --json)"
assert_jq "$dry_candidates" '(.data.candidates | length) == 0' \
    "dry-run persists no candidate rows"
dry_status="$(ee_json index status --workspace "$WS" --json)"
assert_jq "$dry_status" "(.data.dbGeneration == ${BASELINE_DB_GEN:-null}) and (.data.dbGeneration == .data.indexGeneration)" \
    "dry-run moves neither workspace generation nor index generation"
dry_audit="$(ee_json audit timeline --workspace "$WS" --action curation_candidate.create --json)"
assert_jq "$dry_audit" '(.pagination.total_count // .data.pagination.total_count) == 0' \
    "dry-run writes no creation audit rows"

step "zero item budget cancels before any mutation"
cancel_out="$(ee_json daemon --workspace "$WS" --foreground --once --job consolidation_pass --item-limit 0 --json)"
assert_jq "$cancel_out" '.data.ticks[0].runner.results[0].outcome == "cancelled"' \
    "zero item budget cancels the steward job"
cancel_candidates="$(ee_json curate candidates --workspace "$WS" --type consolidate --all --json)"
assert_jq "$cancel_candidates" '(.data.candidates | length) == 0' \
    "cancelled run persists no candidate rows"

step "real consolidation_pass inserts one deterministic deduplicated candidate"
real_out="$(ee_json daemon --workspace "$WS" --foreground --once --job consolidation_pass --json)"
assert_jq "$real_out" '.data.ticks[0].runner.results[0].details.insertedCandidates == 1 and .data.ticks[0].runner.results[0].details.durableMutation == true' \
    "real run inserts exactly one candidate"
assert_jq "$real_out" '.data.ticks[0].runner.results[0].budgetUsed != null' \
    "real run reports bounded resource accounting"
CANDIDATE_ID="$(json_scalar "$real_out" '.data.ticks[0].runner.results[0].details.candidateIds[0] // empty')"
assert_eq "$([ -n "$CANDIDATE_ID" ] && echo present || echo missing)" "present" \
    "real run emits the candidate id"
rerun_out="$(ee_json daemon --workspace "$WS" --foreground --once --job consolidation_pass --json)"
assert_jq "$rerun_out" '.data.ticks[0].runner.results[0].details.insertedCandidates == 0 and .data.ticks[0].runner.results[0].details.alreadyPendingCandidates == 1' \
    "re-run dedupes instead of duplicating"
RERUN_CANDIDATE_ID="$(json_scalar "$rerun_out" '.data.ticks[0].runner.results[0].details.candidateIds[0] // empty')"
assert_eq "$RERUN_CANDIDATE_ID" "$CANDIDATE_ID" \
    "re-run plans the same deterministic candidate id"
create_audit="$(ee_json audit timeline --workspace "$WS" --action curation_candidate.create --json)"
assert_jq "$create_audit" '(.pagination.total_count // .data.pagination.total_count) == 1' \
    "exactly one creation audit row exists"

step "validate and apply through the public curation commands"
validate_out="$(ee_json curate validate "$CANDIDATE_ID" --workspace "$WS" --json)"
assert_jq "$validate_out" '.data.mutation.toStatus == "approved"' \
    "validate approves the candidate"
apply_out="$(ee_json curate apply "$CANDIDATE_ID" --workspace "$WS" --json)"
assert_jq "$apply_out" '.data.application.decision == "consolidate_absorb" and .data.application.status == "applied"' \
    "apply runs the consolidate-absorb decision"
assert_jq "$apply_out" '.data.durableMutation == true' \
    "apply reports a durable mutation"

step "absorb preserves the source and tombstones only the duplicate"
memories_out="$(ee_json memory list --workspace "$WS" --json)"
duplicate_state="$(printf '%s' "$memories_out" | jq -r --arg id "$DUPLICATE_ID" \
    '.data.memories[]? | select(.id == $id) | if (.is_tombstoned // .isTombstoned // false) then "tombstoned" else "active" end' 2>/dev/null || true)"
assert_eq "$duplicate_state" "tombstoned" \
    "absorbed duplicate is preserved as a tombstoned row"
survivor_state="$(printf '%s' "$memories_out" | jq -r --arg id "$SURVIVOR_ID" --arg content "$GROUP_PHRASE" \
    '.data.memories[]? | select(.id == $id) | if ((.is_tombstoned // .isTombstoned // false) | not) and .content == $content then "intact" else "mutated" end' 2>/dev/null || true)"
assert_eq "$survivor_state" "intact" \
    "survivor keeps its content with no opaque rewrite"

step "why explains lineage and the audit chain stays intact"
why_out="$(ee_json why "$SURVIVOR_ID" --workspace "$WS" --json)"
lineage="$(printf '%s' "$why_out" | jq -r --arg id "$DUPLICATE_ID" \
    '[.data.links[]? | select(.relation == "derived_from" and .linkedMemoryId == $id)] | length' 2>/dev/null || echo 0)"
e2e_log_assert_num "${lineage:-0}" -ge 1 \
    "why survivor explains the derived_from lineage"
apply_audit="$(ee_json audit timeline --workspace "$WS" --action curation_candidate.apply --json)"
assert_jq "$apply_audit" '(.pagination.total_count // .data.pagination.total_count) == 1' \
    "exactly one apply audit row exists"
verify_out="$(ee_json audit verify --workspace "$WS" --json)"
assert_jq "$verify_out" '(.integrity_ok // .data.integrity_ok) == true' \
    "append-only audit hash chain verifies"

step "workflow job restores a truthful deduplicated index"
stale_status="$(ee_json index status --workspace "$WS" --json)"
assert_jq "$stale_status" '.data.dbGeneration > .data.indexGeneration and .data.health == "stale"' \
    "apply leaves an honestly stale index until the workflow job runs"
post_coalesce="$(ee_json daemon --workspace "$WS" --foreground --once --job index_coalesce --json)"
assert_jq "$post_coalesce" '.schema == "ee.response.v2" and .success == true' \
    "post-apply index_coalesce succeeds"
truthful_status="$(ee_json index status --workspace "$WS" --json)"
assert_jq "$truthful_status" '.data.dbGeneration == .data.indexGeneration and .data.health == "ready"' \
    "generation is truthful after the workflow-emitted job"
assert_jq "$truthful_status" '.data.indexDocumentCounts.memories == 3' \
    "per-kind index counts drop the absorbed duplicate exactly once"

step "search selects the consolidated result once and keeps controls distinct"
search_out="$(ee_json search "$SEARCH_QUERY" --workspace "$WS" --limit 10 --json)"
survivor_hits="$(printf '%s' "$search_out" | jq -r --arg id "$SURVIVOR_ID" \
    '[.. | strings | select(. == $id)] | length' 2>/dev/null || echo 0)"
duplicate_hits="$(printf '%s' "$search_out" | jq -r --arg id "$DUPLICATE_ID" \
    '[.data.results[]? | .. | strings | select(. == $id)] | length' 2>/dev/null || echo 0)"
wording_hits="$(printf '%s' "$search_out" | jq -r --arg id "$WORDING_CONTROL_ID" \
    '[.. | strings | select(. == $id)] | length' 2>/dev/null || echo 0)"
kind_hits="$(printf '%s' "$search_out" | jq -r --arg id "$KIND_CONTROL_ID" \
    '[.. | strings | select(. == $id)] | length' 2>/dev/null || echo 0)"
e2e_log_assert_num "${survivor_hits:-0}" -ge 1 "consolidated survivor is selected"
e2e_log_assert_num "${duplicate_hits:-0}" -eq 0 "absorbed duplicate never surfaces in search"
e2e_log_assert_num "${wording_hits:-0}" -ge 1 "wording control remains distinct"
e2e_log_assert_num "${kind_hits:-0}" -ge 1 "kind control remains distinct"

step "steward and apply re-runs are idempotent"
idempotent_out="$(ee_json daemon --workspace "$WS" --foreground --once --job consolidation_pass --json)"
assert_jq "$idempotent_out" '.data.ticks[0].runner.results[0].details.plannedCandidates == 0 and .data.ticks[0].runner.results[0].details.insertedCandidates == 0' \
    "steward re-run after absorb has nothing to plan"
replay_out="$(ee_json curate apply "$CANDIDATE_ID" --workspace "$WS" --json)"
assert_jq "$replay_out" '.data.application.status == "already_applied" and .data.durableMutation == false' \
    "apply replay is an idempotent no-op"
replay_audit="$(ee_json audit timeline --workspace "$WS" --action curation_candidate.apply --json)"
assert_jq "$replay_audit" '(.pagination.total_count // .data.pagination.total_count) == 1' \
    "replay appends no duplicate audit rows"

if [ "${EE_GRAPH_E2E_INJECT_FAILURE:-0}" = "1" ]; then
    e2e_log_assert_eq "actual-consolidation" "expected-consolidation" "consolidation_injected_failure_diff" || true
fi

harness_summary
