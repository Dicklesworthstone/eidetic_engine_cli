#!/usr/bin/env bash
# Logged E2E driver for the source-derived candidate lifecycle.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
# shellcheck source=scripts/e2e_overhaul/lib/derivation_reflection.sh
source "$SCRIPT_DIR/lib/derivation_reflection.sh"

require_jq
epic_setup "external_derivation_lifecycle"
drh_seed_derivation_sources "external_derivation_lifecycle"
DB_INSPECT_DATABASE=$(python3 - "$EPIC_WORKSPACE" <<'PY'
import os
import sys

print(os.path.realpath(os.path.join(sys.argv[1], ".ee", "ee.db")))
PY
)

assert_json_success() {
    local json="${1:-}"
    local label="${2:?label required}"
    if printf '%s' "$json" | jq . >/dev/null 2>&1; then
        assert_jq "$json" 'if has("success") then (.success == true) elif has("error") then false else ((.schema // "") | type == "string") end' "true" "$label"
    else
        e2e_log_assert_eq "parseable-json" "invalid-json" "${label}_parseable"
    fi
}

assert_json_failure() {
    local json="${1:-}"
    local label="${2:?label required}"
    if printf '%s' "$json" | jq . >/dev/null 2>&1; then
        assert_jq "$json" 'if has("success") then (.success == false) elif has("error") then true else false end' "true" "$label"
    else
        e2e_log_assert_eq "parseable-json" "invalid-json" "${label}_parseable"
    fi
}

candidate_id_from_json() {
    printf '%s' "${1:-}" | jq -r '
        .data.candidateId //
        .data.candidate_id //
        .data.candidates[0].candidateId //
        .data.candidates[0].id //
        empty
    ' 2>/dev/null || true
}

created_memory_id_from_apply_json() {
    printf '%s' "${1:-}" | jq -r '
        .data.application.createdMemoryId //
        .data.application.created_memory_id //
        .data.createdMemoryId //
        .data.created_memory_id //
        empty
    ' 2>/dev/null || true
}

candidate_status_from_show_json() {
    printf '%s' "${1:-}" | jq -r '
        .data.candidate.status //
        .data.status //
        empty
    ' 2>/dev/null || true
}

candidate_review_state_from_show_json() {
    printf '%s' "${1:-}" | jq -r '
        .data.candidate.reviewState //
        .data.candidate.review_state //
        .data.reviewState //
        .data.review_state //
        empty
    ' 2>/dev/null || true
}

assert_candidate_visible() {
    local json="${1:-}"
    local candidate_id="${2:?candidate id required}"
    local label="${3:?label required}"
    local visible
    visible=$(printf '%s' "$json" | jq --arg id "$candidate_id" '
        [.data.candidates[]? | select((.candidateId // .candidate_id // .id // "") == $id)] | length
    ' 2>/dev/null || echo 0)
    e2e_log_assert_num "$visible" -ge 1 "$label"
}

DRH_RECOVERY_ACTIONS='["ee curate propose-derived","ee curate candidates","ee curate validate","ee curate apply","ee why"]'
drh_log_state "setup" "source_memories_created" "ok"

PROPOSE_DRY_RUN_JSON=$(ee_workspace curate propose-derived \
    --source-memory "$DRH_SOURCE_MEMORY_A_ID" \
    --source-memory "$DRH_SOURCE_MEMORY_B_ID" \
    --level semantic \
    --kind insight \
    --content "External derivation lifecycle dry-run should not persist this candidate." \
    --tag derivation \
    --tag lifecycle \
    --tag e2e \
    --confidence 0.72 \
    --producer-kind external_derivation_lifecycle \
    --producer-model bd-17bob \
    --dry-run \
    --json 2>/dev/null || true)
drh_extract_degraded_codes "$PROPOSE_DRY_RUN_JSON"
drh_extract_recovery_actions "$PROPOSE_DRY_RUN_JSON"
DRY_RUN_CANDIDATE_ID=$(candidate_id_from_json "$PROPOSE_DRY_RUN_JSON")
DRH_CANDIDATE_ID="$DRY_RUN_CANDIDATE_ID"
assert_json_success "$PROPOSE_DRY_RUN_JSON" "external_derivation_propose_dry_run_success"
assert_jq "$PROPOSE_DRY_RUN_JSON" '.data.dryRun // false' "true" "external_derivation_propose_dry_run_flag"
assert_jq "$PROPOSE_DRY_RUN_JSON" '.data.persisted == false' "true" "external_derivation_propose_dry_run_not_persisted"
assert_jq "$PROPOSE_DRY_RUN_JSON" '.data.targetMemoryId == null' "true" "external_derivation_propose_dry_run_target_null"
assert_jq_nonempty "$PROPOSE_DRY_RUN_JSON" '.data.nextCommands[]? | select(test("^ee curate validate "))' "external_derivation_propose_dry_run_next_validate"
DRY_RUN_LIST_JSON=$(ee_workspace curate candidates --status pending --json 2>/dev/null || true)
if [ -n "$DRY_RUN_CANDIDATE_ID" ]; then
    DRY_RUN_VISIBLE=$(printf '%s' "$DRY_RUN_LIST_JSON" | jq --arg id "$DRY_RUN_CANDIDATE_ID" '
        [.data.candidates[]? | select((.candidateId // .candidate_id // .id // "") == $id)] | length
    ' 2>/dev/null || echo 0)
    e2e_log_assert_num "$DRY_RUN_VISIBLE" -eq 0 "external_derivation_propose_dry_run_absent_from_queue"
fi
drh_log_state "propose_dry_run" "propose_dry_run" "ok"

PROPOSE_APPLY_JSON=$(ee_workspace curate propose-derived \
    --source-memory "$DRH_SOURCE_MEMORY_A_ID" \
    --source-memory "$DRH_SOURCE_MEMORY_B_ID" \
    --level semantic \
    --kind insight \
    --content "bd-17bob lifecycle proof: run ee curate validate before ee curate apply for scripts/e2e_overhaul/external_derivation_lifecycle.sh; preserve derived_source_memory_tombstoned recovery evidence." \
    --tag derivation \
    --tag lifecycle \
    --tag e2e \
    --confidence 0.81 \
    --producer-kind external_derivation_lifecycle \
    --producer-model bd-17bob \
    --producer-note "canonical bd-17bob apply path" \
    --json 2>/dev/null || true)
drh_extract_degraded_codes "$PROPOSE_APPLY_JSON"
drh_extract_recovery_actions "$PROPOSE_APPLY_JSON"
DRH_CANDIDATE_ID=$(candidate_id_from_json "$PROPOSE_APPLY_JSON")
assert_json_success "$PROPOSE_APPLY_JSON" "external_derivation_propose_apply_candidate_insert_success"
assert_jq_nonempty "$PROPOSE_APPLY_JSON" '.data.candidateId // .data.candidate_id // empty' "external_derivation_propose_apply_candidate_id"
assert_jq "$PROPOSE_APPLY_JSON" '.data.persisted // false' "true" "external_derivation_propose_apply_candidate_persisted"
assert_jq "$PROPOSE_APPLY_JSON" '.data.candidateType // .data.candidate_type // empty' "create_derived_memory" "external_derivation_propose_apply_candidate_type"
drh_log_state "propose_apply_candidate_insert" "propose_apply_candidate_insert" "ok"

LIST_JSON=$(ee_workspace curate candidates --status pending --json 2>/dev/null || true)
drh_extract_degraded_codes "$LIST_JSON"
drh_extract_recovery_actions "$LIST_JSON"
assert_json_success "$LIST_JSON" "external_derivation_list_success"
assert_candidate_visible "$LIST_JSON" "$DRH_CANDIDATE_ID" "external_derivation_list_candidate_visible"
SHOW_BEFORE_VALIDATE_JSON=$(ee_workspace curate show "$DRH_CANDIDATE_ID" --json 2>/dev/null || true)
assert_json_success "$SHOW_BEFORE_VALIDATE_JSON" "external_derivation_show_before_validate_success"
e2e_log_assert_eq "$(candidate_status_from_show_json "$SHOW_BEFORE_VALIDATE_JSON")" "pending" "external_derivation_show_before_validate_pending"
drh_log_state "list" "list" "ok"

VALIDATE_DRY_RUN_JSON=$(ee_workspace curate validate "$DRH_CANDIDATE_ID" --dry-run --actor bd-17bob-e2e --json 2>/dev/null || true)
drh_extract_degraded_codes "$VALIDATE_DRY_RUN_JSON"
drh_extract_recovery_actions "$VALIDATE_DRY_RUN_JSON"
assert_json_success "$VALIDATE_DRY_RUN_JSON" "external_derivation_validate_dry_run_success"
assert_jq "$VALIDATE_DRY_RUN_JSON" '.data.mutation.persisted == false' "true" "external_derivation_validate_dry_run_not_persisted"
SHOW_AFTER_DRY_VALIDATE_JSON=$(ee_workspace curate show "$DRH_CANDIDATE_ID" --json 2>/dev/null || true)
assert_json_success "$SHOW_AFTER_DRY_VALIDATE_JSON" "external_derivation_show_after_dry_validate_success"
e2e_log_assert_eq "$(candidate_status_from_show_json "$SHOW_AFTER_DRY_VALIDATE_JSON")" "pending" "external_derivation_show_after_dry_validate_pending"
VALIDATE_JSON=$(ee_workspace curate validate "$DRH_CANDIDATE_ID" --actor bd-17bob-e2e --json 2>/dev/null || true)
drh_extract_degraded_codes "$VALIDATE_JSON"
drh_extract_recovery_actions "$VALIDATE_JSON"
assert_json_success "$VALIDATE_JSON" "external_derivation_validate_success"
assert_jq "$VALIDATE_JSON" '.data.validation.status // empty' "passed" "external_derivation_validate_passed"
assert_jq "$VALIDATE_JSON" '.data.validation.decision // empty' "approved" "external_derivation_validate_approved"
assert_jq "$VALIDATE_JSON" '.data.mutation.toStatus // .data.mutation.to_status // empty' "approved" "external_derivation_validate_to_approved"
SHOW_AFTER_VALIDATE_JSON=$(ee_workspace curate show "$DRH_CANDIDATE_ID" --json 2>/dev/null || true)
assert_json_success "$SHOW_AFTER_VALIDATE_JSON" "external_derivation_show_after_validate_success"
e2e_log_assert_eq "$(candidate_status_from_show_json "$SHOW_AFTER_VALIDATE_JSON")" "approved" "external_derivation_show_after_validate_approved"
drh_log_state "validate" "validate" "ok"

APPLY_PREVIEW_JSON=$(ee_workspace curate apply "$DRH_CANDIDATE_ID" --dry-run --actor bd-17bob-e2e --json 2>/dev/null || true)
drh_extract_degraded_codes "$APPLY_PREVIEW_JSON"
drh_extract_recovery_actions "$APPLY_PREVIEW_JSON"
assert_json_success "$APPLY_PREVIEW_JSON" "external_derivation_apply_preview_success"
assert_jq "$APPLY_PREVIEW_JSON" '.data.mutation.persisted == false' "true" "external_derivation_apply_preview_not_persisted"
APPLY_JSON=$(ee_workspace curate apply "$DRH_CANDIDATE_ID" --actor bd-17bob-e2e --json 2>/dev/null || true)
drh_extract_degraded_codes "$APPLY_JSON"
drh_extract_recovery_actions "$APPLY_JSON"
assert_json_success "$APPLY_JSON" "external_derivation_apply_success"
DRH_CREATED_MEMORY_ID=$(created_memory_id_from_apply_json "$APPLY_JSON")
assert_jq_nonempty "$APPLY_JSON" '.data.application.createdMemoryId // .data.application.created_memory_id // .data.createdMemoryId // .data.created_memory_id // empty' "external_derivation_apply_created_memory_id"
assert_jq "$APPLY_JSON" '.data.application.status // empty' "applied" "external_derivation_apply_status"
SHOW_AFTER_APPLY_JSON=$(ee_workspace curate show "$DRH_CANDIDATE_ID" --json 2>/dev/null || true)
assert_json_success "$SHOW_AFTER_APPLY_JSON" "external_derivation_show_after_apply_success"
e2e_log_assert_eq "$(candidate_status_from_show_json "$SHOW_AFTER_APPLY_JSON")" "applied" "external_derivation_show_after_apply_applied"
drh_log_state "apply" "apply" "ok"

WHY_JSON=$(ee_workspace why "$DRH_CREATED_MEMORY_ID" --database "$DB_INSPECT_DATABASE" --json 2>/dev/null || true)
drh_extract_degraded_codes "$WHY_JSON"
drh_extract_recovery_actions "$WHY_JSON"
assert_json_success "$WHY_JSON" "external_derivation_why_success"
assert_jq "$WHY_JSON" '.data.memoryId // .data.memory_id // empty' "$DRH_CREATED_MEMORY_ID" "external_derivation_why_created_memory_id"
WHY_HAS_PROVENANCE=$(printf '%s' "$WHY_JSON" | jq --arg a "$DRH_SOURCE_MEMORY_A_ID" --arg b "$DRH_SOURCE_MEMORY_B_ID" --arg c "$DRH_CANDIDATE_ID" '
    (.data | tostring | contains($a)) and
    (.data | tostring | contains($b)) and
    (.data | tostring | contains($c))
' 2>/dev/null || echo false)
e2e_log_assert_eq "$WHY_HAS_PROVENANCE" "true" "external_derivation_why_candidate_and_source_provenance"
drh_log_state "why" "why" "ok"

DB_CANDIDATES_JSON=$(ee_workspace db inspect curation_candidates --database "$DB_INSPECT_DATABASE" --limit 20 --json 2>/dev/null || true)
drh_extract_degraded_codes "$DB_CANDIDATES_JSON"
drh_extract_recovery_actions "$DB_CANDIDATES_JSON"
assert_json_success "$DB_CANDIDATES_JSON" "external_derivation_db_inspect_candidates_success"
DB_HAS_APPLIED_CANDIDATE=$(printf '%s' "$DB_CANDIDATES_JSON" | jq --arg id "$DRH_CANDIDATE_ID" '
    [.data.report.rows[]? | select((.values.id // "") == $id and (.values.status // "") == "applied")] | length
' 2>/dev/null || echo 0)
e2e_log_assert_num "$DB_HAS_APPLIED_CANDIDATE" -ge 1 "external_derivation_db_inspect_applied_candidate"
DB_LINKS_JSON=$(ee_workspace db inspect memory_links --database "$DB_INSPECT_DATABASE" --limit 50 --json 2>/dev/null || true)
assert_json_success "$DB_LINKS_JSON" "external_derivation_db_inspect_links_success"
DB_HAS_DERIVED_LINK=$(printf '%s' "$DB_LINKS_JSON" | jq --arg created "$DRH_CREATED_MEMORY_ID" --arg a "$DRH_SOURCE_MEMORY_A_ID" --arg b "$DRH_SOURCE_MEMORY_B_ID" '
    [.data.report.rows[]?
     | select((.values.src_memory_id // .values.source_memory_id // .values.sourceMemoryId // "") == $created)
     | select((.values.dst_memory_id // .values.target_memory_id // .values.targetMemoryId // "") == $a or (.values.dst_memory_id // .values.target_memory_id // .values.targetMemoryId // "") == $b)
     | select((.values.relation // "") == "derived_from")] | length
' 2>/dev/null || echo 0)
e2e_log_assert_num "$DB_HAS_DERIVED_LINK" -ge 2 "external_derivation_db_inspect_derived_links"
drh_log_state "db_inspect" "db_inspect" "ok"

DRIFT_PROPOSE_JSON=$(ee_workspace curate propose-derived \
    --source-memory "$DRH_SOURCE_MEMORY_A_ID" \
    --level semantic \
    --kind insight \
    --content "bd-17bob source drift proof: ee curate apply must block after ee curate tombstone marks the cited source memory tombstoned; expect derived_source_memory_tombstoned repair." \
    --tag derivation \
    --tag lifecycle \
    --tag source-drift \
    --confidence 0.73 \
    --producer-kind external_derivation_lifecycle \
    --producer-model bd-17bob-source-drift \
    --json 2>/dev/null || true)
DRIFT_CANDIDATE_ID=$(candidate_id_from_json "$DRIFT_PROPOSE_JSON")
DRH_CANDIDATE_ID="$DRIFT_CANDIDATE_ID"
assert_json_success "$DRIFT_PROPOSE_JSON" "external_derivation_negative_source_drift_propose_success"
DRIFT_VALIDATE_JSON=$(ee_workspace curate validate "$DRIFT_CANDIDATE_ID" --actor bd-17bob-e2e --json 2>/dev/null || true)
assert_json_success "$DRIFT_VALIDATE_JSON" "external_derivation_negative_source_drift_validate_success"
TOMBSTONE_JSON=$(ee_workspace curate tombstone "$DRH_SOURCE_MEMORY_A_ID" --reason "bd-17bob source drift probe" --actor bd-17bob-e2e --allow-tombstone-load-bearing --json 2>/dev/null || true)
assert_json_success "$TOMBSTONE_JSON" "external_derivation_negative_source_drift_tombstone_success"
DRIFT_APPLY_JSON=$(ee_workspace curate apply "$DRIFT_CANDIDATE_ID" --actor bd-17bob-e2e --json 2>/dev/null || true)
drh_extract_degraded_codes "$DRIFT_APPLY_JSON"
drh_extract_recovery_actions "$DRIFT_APPLY_JSON"
assert_json_success "$DRIFT_APPLY_JSON" "external_derivation_negative_source_drift_apply_returns_report"
assert_jq "$DRIFT_APPLY_JSON" '.data.application.status // empty' "blocked" "external_derivation_negative_source_drift_apply_blocked"
DRIFT_HAS_SOURCE_ERROR=$(printf '%s' "$DRIFT_APPLY_JSON" | jq '
    [.data.application.errors[]?.code] | any(. == "derived_source_memory_tombstoned" or . == "derived_source_invalid")
' 2>/dev/null || echo false)
e2e_log_assert_eq "$DRIFT_HAS_SOURCE_ERROR" "true" "external_derivation_negative_source_drift_error_code"
DRIFT_CREATED_ID=$(created_memory_id_from_apply_json "$DRIFT_APPLY_JSON")
e2e_log_assert_eq "$DRIFT_CREATED_ID" "" "external_derivation_negative_source_drift_no_created_memory"
DRIFT_SHOW_JSON=$(ee_workspace curate show "$DRIFT_CANDIDATE_ID" --json 2>/dev/null || true)
assert_json_success "$DRIFT_SHOW_JSON" "external_derivation_negative_source_drift_show_success"
e2e_log_assert_eq "$(candidate_status_from_show_json "$DRIFT_SHOW_JSON")" "approved" "external_derivation_negative_source_drift_candidate_remains_approved"
drh_log_state "negative_source_drift" "negative_source_drift" "ok"

REJECT_PROPOSE_JSON=$(ee_workspace curate propose-derived \
    --source-memory "$DRH_SOURCE_MEMORY_B_ID" \
    --level semantic \
    --kind insight \
    --content "External derivation lifecycle: rejected candidate must never become a memory." \
    --tag derivation \
    --tag lifecycle \
    --tag reject \
    --confidence 0.41 \
    --producer-kind external_derivation_lifecycle \
    --producer-model bd-17bob-reject \
    --json 2>/dev/null || true)
REJECT_CANDIDATE_ID=$(candidate_id_from_json "$REJECT_PROPOSE_JSON")
DRH_CANDIDATE_ID="$REJECT_CANDIDATE_ID"
assert_json_success "$REJECT_PROPOSE_JSON" "external_derivation_reject_propose_success"
REJECT_SHOW_BEFORE_JSON=$(ee_workspace curate show "$REJECT_CANDIDATE_ID" --json 2>/dev/null || true)
assert_json_success "$REJECT_SHOW_BEFORE_JSON" "external_derivation_reject_show_before_success"
REJECT_JSON=$(ee_workspace curate reject "$REJECT_CANDIDATE_ID" --reason "bd-17bob deliberate reject branch" --actor bd-17bob-e2e --json 2>/dev/null || true)
drh_extract_degraded_codes "$REJECT_JSON"
drh_extract_recovery_actions "$REJECT_JSON"
assert_json_success "$REJECT_JSON" "external_derivation_reject_success"
assert_jq "$REJECT_JSON" '.data.review.action // empty' "reject" "external_derivation_reject_action"
assert_jq "$REJECT_JSON" '.data.mutation.toStatus // .data.mutation.to_status // empty' "rejected" "external_derivation_reject_to_status"
REJECT_SHOW_AFTER_JSON=$(ee_workspace curate show "$REJECT_CANDIDATE_ID" --json 2>/dev/null || true)
assert_json_success "$REJECT_SHOW_AFTER_JSON" "external_derivation_reject_show_after_success"
e2e_log_assert_eq "$(candidate_status_from_show_json "$REJECT_SHOW_AFTER_JSON")" "rejected" "external_derivation_reject_terminal_status"
REJECT_REVIEW_STATE=$(candidate_review_state_from_show_json "$REJECT_SHOW_AFTER_JSON")
e2e_log_assert_eq "$REJECT_REVIEW_STATE" "rejected" "external_derivation_reject_terminal_review_state"
REJECT_SEARCH_JSON=$(ee_workspace search "rejected candidate must never become a memory" --limit 10 --json 2>/dev/null || true)
if printf '%s' "$REJECT_SEARCH_JSON" | jq . >/dev/null 2>&1; then
    REJECT_MEMORY_HITS=$(printf '%s' "$REJECT_SEARCH_JSON" | jq '
        [.data.results[]? | select((.content // "") | contains("rejected candidate must never become a memory"))] | length
    ' 2>/dev/null || echo 0)
    e2e_log_assert_num "$REJECT_MEMORY_HITS" -eq 0 "external_derivation_reject_no_created_memory_search_hit"
else
    e2e_log_assert_eq "parseable-json" "invalid-json" "external_derivation_reject_search_parseable"
fi
drh_log_state "reject" "reject" "ok"

ALL_CANDIDATES_JSON=$(ee_workspace curate candidates --all --json 2>/dev/null || true)
assert_json_success "$ALL_CANDIDATES_JSON" "external_derivation_invariant_all_candidates_success"
assert_candidate_visible "$ALL_CANDIDATES_JSON" "$REJECT_CANDIDATE_ID" "external_derivation_invariant_rejected_candidate_visible"
assert_candidate_visible "$ALL_CANDIDATES_JSON" "$DRIFT_CANDIDATE_ID" "external_derivation_invariant_drift_candidate_visible"
APPLY_REPLAY_JSON=$(ee_workspace curate apply "$DRH_CANDIDATE_ID" --actor bd-17bob-e2e --json 2>/dev/null || true)
assert_json_success "$APPLY_REPLAY_JSON" "external_derivation_invariant_rejected_apply_returns_report"
assert_jq "$APPLY_REPLAY_JSON" '.data.application.status // empty' "blocked" "external_derivation_invariant_rejected_apply_blocked"
COT_PROBE_JSON=$(ee_workspace search "private thinking raw chain of thought" --limit 10 --json 2>/dev/null || true)
if printf '%s' "$COT_PROBE_JSON" | jq . >/dev/null 2>&1; then
    COT_HITS=$(printf '%s' "$COT_PROBE_JSON" | jq '
        [.data.results[]? | select((.content // "") | test("(?i)private thinking|raw chain.of.thought"))] | length
    ' 2>/dev/null || echo 0)
    e2e_log_assert_num "$COT_HITS" -eq 0 "external_derivation_invariant_no_chain_of_thought_persistence"
else
    e2e_log_assert_eq "parseable-json" "invalid-json" "external_derivation_invariant_cot_probe_parseable"
fi
DRH_CANDIDATE_ID="$REJECT_CANDIDATE_ID"
drh_log_state "invariant_check" "invariant_check" "ok"

drh_assert_test_log_contract "external_derivation_lifecycle_log_contract"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
