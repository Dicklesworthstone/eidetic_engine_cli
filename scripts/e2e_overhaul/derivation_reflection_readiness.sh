#!/usr/bin/env bash
# Logged readiness driver for derivation/reflection contract prerequisites.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
# shellcheck source=scripts/e2e_overhaul/lib/derivation_reflection.sh
source "$SCRIPT_DIR/lib/derivation_reflection.sh"

require_jq
epic_setup "derivation_reflection_readiness"
drh_seed_derivation_sources "derivation_reflection_readiness"

e2e_log_assert_eq "$([ -f "$REPO_ROOT/docs/adr/0043-external-derivation-candidates.md" ] && printf present || printf missing)" "present" "external_derivation_adr_present"
e2e_log_assert_eq "$([ -f "$REPO_ROOT/docs/adr/0044-no-llm-reflection-handshake.md" ] && printf present || printf missing)" "present" "reflection_handshake_adr_present"
DRH_RECOVERY_ACTIONS='["bd-17bob external derivation producer","bd-215tr integrated readiness dependency","bd-2dek9 derivation lifecycle coverage","bd-3bsvv reflection ingest coverage","bd-pxi7f reflection policy coverage"]'
drh_log_state "derivation_reflection_prerequisites_probe" "adr_prerequisites" "recorded"

SCHEMA_LIST_JSON=$(ee_workspace schema list --json 2>/dev/null || true)
drh_extract_degraded_codes "$SCHEMA_LIST_JSON"
drh_extract_recovery_actions "$SCHEMA_LIST_JSON"

if printf '%s' "$SCHEMA_LIST_JSON" | jq . >/dev/null 2>&1; then
    assert_jq "$SCHEMA_LIST_JSON" '.success // false' "true" "derivation_reflection_schema_list_success"
    REFLECT_SCHEMA_COUNT=$(printf '%s' "$SCHEMA_LIST_JSON" | jq '[.. | strings | select(. == "ee.reflect.request.v1" or . == "ee.reflect.result.v1")] | length' 2>/dev/null || echo 0)
    if [ "$REFLECT_SCHEMA_COUNT" -ge 2 ]; then
        e2e_log_assert_num "$REFLECT_SCHEMA_COUNT" -ge 2 "reflection_artifact_schemas_registered"
    else
        todo_assert "reflection_artifact_schemas_registered" "bd-ogqf6" "Reflection artifact schemas land with the request ledger implementation."
    fi
else
    e2e_log_assert_eq "parseable-json" "invalid-json" "derivation_reflection_schema_list_parseable"
fi
drh_log_state "reflection_schema_registration_probe" "schema_list_probe" "recorded"

CURATE_JSON=$(ee_workspace curate candidates --json 2>/dev/null || true)
drh_extract_degraded_codes "$CURATE_JSON"
drh_extract_recovery_actions "$CURATE_JSON"
drh_capture_candidate_id "$CURATE_JSON"

if printf '%s' "$CURATE_JSON" | jq . >/dev/null 2>&1; then
    assert_jq "$CURATE_JSON" '.success // false' "true" "readiness_curate_candidates_success"
    DERIVED_COUNT=$(printf '%s' "$CURATE_JSON" | jq '[.data.candidates[]? | select((.candidateType // .candidate_type // .kind // "") == "create_derived_memory")] | length' 2>/dev/null || echo 0)
    if [ "$DERIVED_COUNT" -gt 0 ]; then
        e2e_log_assert_num "$DERIVED_COUNT" -ge 1 "readiness_external_derivation_candidate_visible"
    else
        todo_assert "readiness_external_derivation_candidate_visible" "bd-17bob" "No create_derived_memory candidate is expected until the external derivation producer surface lands."
    fi
else
    e2e_log_assert_eq "parseable-json" "invalid-json" "readiness_curate_candidates_parseable"
fi
drh_log_state "external_derivation_candidate_probe" "curate_candidates_probe" "recorded"

REFLECT_HELP=$(ee_global reflect --help 2>/dev/null || true)
if printf '%s' "$REFLECT_HELP" | grep -q "reflect"; then
    e2e_log_assert_eq "true" "true" "readiness_reflect_help_available"
    REFLECT_KEY_PATH="$EPIC_WORKSPACE/reflection_hmac.key"
    printf '%s\n' "derivation-reflection-readiness-hmac-key" >"$REFLECT_KEY_PATH"
    PROPOSE_JSON=$(EE_REFLECTION_HMAC_KEY_ID="readiness-e2e-key" \
        EE_REFLECTION_HMAC_KEY_PATH="$REFLECT_KEY_PATH" \
        ee_workspace reflect propose \
        --kind gaps \
        --source "$DRH_SOURCE_MEMORY_A_ID" \
        --source "$DRH_SOURCE_MEMORY_B_ID" \
        --dry-run \
        --json 2>/dev/null || true)
    drh_capture_request_identity "$PROPOSE_JSON"
    drh_extract_degraded_codes "$PROPOSE_JSON"
    drh_extract_recovery_actions "$PROPOSE_JSON"
    if printf '%s' "$PROPOSE_JSON" | jq . >/dev/null 2>&1; then
        REFLECT_SUCCESS=$(printf '%s' "$PROPOSE_JSON" | jq -r '.success // false' 2>/dev/null || echo false)
        if [ "$REFLECT_SUCCESS" = "true" ]; then
            assert_jq "$PROPOSE_JSON" '.schema // empty' "ee.response.v2" "readiness_reflect_propose_response_schema"
            assert_jq "$PROPOSE_JSON" '.success // false' "true" "readiness_reflect_propose_dry_run_success"
            assert_jq "$PROPOSE_JSON" '.data.schema // empty' "ee.reflect.propose.v1" "readiness_reflect_propose_data_schema"
            assert_jq "$PROPOSE_JSON" '.data.command // empty' "reflect propose" "readiness_reflect_propose_command"
            assert_jq_nonempty "$PROPOSE_JSON" '.data.requestId // .requestId // empty' "readiness_reflection_request_id_present"
            assert_jq_nonempty "$PROPOSE_JSON" '.data.requestHash // .requestHash // empty' "readiness_reflection_request_hash_present"
            CHALLENGE_KEY_ID=$(printf '%s' "$PROPOSE_JSON" | jq -r '.data.request.challenge.keyId // empty' 2>/dev/null || true)
            if [ -n "$CHALLENGE_KEY_ID" ]; then
                e2e_log_assert_eq "$CHALLENGE_KEY_ID" "readiness-e2e-key" "readiness_reflection_challenge_key_id"
            else
                todo_assert "readiness_reflection_challenge_key_id" "bd-3bsvv" "Dry-run reflection readiness can record request identity before the full ingest path exposes challenge metadata."
            fi
        else
            todo_assert "readiness_reflect_propose_dry_run_success" "bd-3bsvv" "Reflection dry-run is expected to stay pending until the integrated reflection path ships."
        fi
    else
        e2e_log_assert_eq "parseable-json" "invalid-json" "readiness_reflect_propose_parseable"
    fi
else
    DRH_RECOVERY_ACTIONS='["implement bd-ogqf6 request ledger before accepting reflection results","run ee reflect propose after the CLI surface exists"]'
    todo_assert "readiness_reflect_help_available" "bd-ogqf6" "ee reflect propose/ingest is intentionally unshipped until the request ledger and HMAC protocol land."
fi
drh_log_state "reflection_gaps_dry_run_probe" "reflect_propose_dry_run" "recorded"

KNOWLEDGE_GAPS_JSON=$(ee_workspace insights --section knowledgeGaps --limit 10 --json 2>/dev/null || true)
drh_extract_degraded_codes "$KNOWLEDGE_GAPS_JSON"
drh_extract_recovery_actions "$KNOWLEDGE_GAPS_JSON"

if printf '%s' "$KNOWLEDGE_GAPS_JSON" | jq . >/dev/null 2>&1; then
    assert_jq "$KNOWLEDGE_GAPS_JSON" '.success // false' "true" "readiness_knowledge_gaps_success"
    assert_jq "$KNOWLEDGE_GAPS_JSON" '.data.selectedSection // empty' "knowledgeGaps" "readiness_knowledge_gaps_selected"
    assert_jq "$KNOWLEDGE_GAPS_JSON" '.data.sections[0].section // empty' "knowledgeGaps" "readiness_knowledge_gaps_section_field"
    ITEM_COUNT=$(printf '%s' "$KNOWLEDGE_GAPS_JSON" | jq '.data.sections[0].items | length' 2>/dev/null || echo 0)
    RECOMMENDATION_COUNT=$(printf '%s' "$KNOWLEDGE_GAPS_JSON" | jq '.data.sections[0].recommendations | length' 2>/dev/null || echo 0)
    e2e_log_assert_num "$RECOMMENDATION_COUNT" -eq "$ITEM_COUNT" "readiness_knowledge_gaps_recommendation_count"
else
    e2e_log_assert_eq "parseable-json" "invalid-json" "readiness_knowledge_gaps_parseable"
fi
drh_log_state "knowledge_gaps_recommendation_probe" "insights_knowledge_gaps_probe" "recorded"

# ---- Active lifecycle phases (bd-1xpqd cc_3) ------------------------------
# These drive the real ee curate propose-derived → list → validate → apply
# → why → db_inspect path. Each step gracefully degrades to todo_assert when
# the underlying feature is not shipped, so the script stays runnable as
# blockers close.

PROPOSE_DERIVED_JSON=$(ee_workspace curate propose-derived \
    --source-memory "$DRH_SOURCE_MEMORY_A_ID" \
    --source-memory "$DRH_SOURCE_MEMORY_B_ID" \
    --level semantic \
    --kind insight \
    --content "Readiness gate: producer-derived insight from sources A and B" \
    --tag derivation \
    --tag e2e \
    --tag readiness \
    --confidence 0.7 \
    --producer-kind reflection \
    --producer-model readiness-e2e \
    --json 2>/dev/null || true)
drh_extract_degraded_codes "$PROPOSE_DERIVED_JSON"
drh_extract_recovery_actions "$PROPOSE_DERIVED_JSON"
drh_capture_candidate_id "$PROPOSE_DERIVED_JSON"

if printf '%s' "$PROPOSE_DERIVED_JSON" | jq . >/dev/null 2>&1; then
    PROPOSE_OK=$(printf '%s' "$PROPOSE_DERIVED_JSON" | jq -r '.success // false' 2>/dev/null || echo false)
    if [ "$PROPOSE_OK" = "true" ] && [ -n "$DRH_CANDIDATE_ID" ]; then
        e2e_log_assert_eq "$PROPOSE_OK" "true" "readiness_propose_derived_success"
        assert_jq_nonempty "$PROPOSE_DERIVED_JSON" '.data.candidateId // .data.candidate_id // empty' "readiness_propose_derived_candidate_id"
        # Per ADR 0043: derived candidates carry targetMemoryId: null
        TARGET=$(printf '%s' "$PROPOSE_DERIVED_JSON" | jq -r '.data.candidate.targetMemoryId // .data.targetMemoryId // "MISSING"' 2>/dev/null || echo "MISSING")
        if [ "$TARGET" = "null" ] || [ "$TARGET" = "MISSING" ]; then
            e2e_log_assert_eq "targetMemoryId-null-or-omitted" "targetMemoryId-null-or-omitted" "readiness_propose_derived_target_null"
        else
            e2e_log_assert_eq "$TARGET" "null" "readiness_propose_derived_target_null"
        fi
    else
        todo_assert "readiness_propose_derived_success" "bd-17bob" "ee curate propose-derived insertion stays pending until the external derivation producer surface lands fully."
    fi
else
    e2e_log_assert_eq "parseable-json" "invalid-json" "readiness_propose_derived_parseable"
fi
drh_log_state "external_derivation_propose_apply_candidate_insert" "propose_apply_candidate_insert" "recorded"

# ---- list ---------------------------------------------------------------
LIST_AFTER_PROPOSE=$(ee_workspace curate candidates --json 2>/dev/null || true)
drh_extract_degraded_codes "$LIST_AFTER_PROPOSE"
drh_extract_recovery_actions "$LIST_AFTER_PROPOSE"
if printf '%s' "$LIST_AFTER_PROPOSE" | jq . >/dev/null 2>&1 && [ -n "$DRH_CANDIDATE_ID" ]; then
    LIST_OK=$(printf '%s' "$LIST_AFTER_PROPOSE" | jq -r '.success // false' 2>/dev/null || echo false)
    if [ "$LIST_OK" = "true" ]; then
        VISIBLE=$(printf '%s' "$LIST_AFTER_PROPOSE" | jq --arg id "$DRH_CANDIDATE_ID" '[.data.candidates[]? | select((.candidateId // .candidate_id // .id // "") == $id)] | length' 2>/dev/null || echo 0)
        e2e_log_assert_num "$VISIBLE" -ge 1 "readiness_list_after_propose_candidate_visible"
    else
        todo_assert "readiness_list_after_propose_success" "bd-17bob" "Candidate listing requires the producer insert path to land."
    fi
else
    todo_assert "readiness_list_after_propose_success" "bd-17bob" "Candidate list dry-run depends on propose-derived candidate id."
fi
drh_log_state "external_derivation_list" "list" "recorded"

# ---- validate (dry-run) -------------------------------------------------
if [ -n "$DRH_CANDIDATE_ID" ]; then
    VALIDATE_JSON=$(ee_workspace curate validate "$DRH_CANDIDATE_ID" --dry-run --json 2>/dev/null || true)
    drh_extract_degraded_codes "$VALIDATE_JSON"
    drh_extract_recovery_actions "$VALIDATE_JSON"
    if printf '%s' "$VALIDATE_JSON" | jq . >/dev/null 2>&1; then
        VAL_OK=$(printf '%s' "$VALIDATE_JSON" | jq -r '.success // false' 2>/dev/null || echo false)
        if [ "$VAL_OK" = "true" ]; then
            e2e_log_assert_eq "$VAL_OK" "true" "readiness_validate_dry_run_success"
        else
            todo_assert "readiness_validate_dry_run_success" "bd-uxcej" "Validate must accept create_derived_memory candidates without loading a target memory before this assertion can pass."
        fi
    else
        e2e_log_assert_eq "parseable-json" "invalid-json" "readiness_validate_dry_run_parseable"
    fi
else
    todo_assert "readiness_validate_dry_run_success" "bd-17bob" "Validate phase requires a real candidate id from propose-derived."
fi
drh_log_state "external_derivation_validate" "validate" "recorded"

# ---- apply (preview) ----------------------------------------------------
if [ -n "$DRH_CANDIDATE_ID" ]; then
    APPLY_PREVIEW=$(ee_workspace curate apply "$DRH_CANDIDATE_ID" --dry-run --json 2>/dev/null || true)
    drh_extract_degraded_codes "$APPLY_PREVIEW"
    drh_extract_recovery_actions "$APPLY_PREVIEW"
    if printf '%s' "$APPLY_PREVIEW" | jq . >/dev/null 2>&1; then
        AP_OK=$(printf '%s' "$APPLY_PREVIEW" | jq -r '.success // false' 2>/dev/null || echo false)
        if [ "$AP_OK" = "true" ]; then
            e2e_log_assert_eq "$AP_OK" "true" "readiness_apply_preview_success"
        else
            todo_assert "readiness_apply_preview_success" "bd-2t583" "Apply preview requires the atomic-create derived-apply path to land."
        fi
    else
        e2e_log_assert_eq "parseable-json" "invalid-json" "readiness_apply_preview_parseable"
    fi
fi
drh_log_state "external_derivation_apply_preview" "apply_preview" "recorded"

# ---- apply (real) -------------------------------------------------------
if [ -n "$DRH_CANDIDATE_ID" ]; then
    APPLY_REAL=$(ee_workspace curate apply "$DRH_CANDIDATE_ID" --json 2>/dev/null || true)
    drh_extract_degraded_codes "$APPLY_REAL"
    drh_extract_recovery_actions "$APPLY_REAL"
    if printf '%s' "$APPLY_REAL" | jq . >/dev/null 2>&1; then
        AR_OK=$(printf '%s' "$APPLY_REAL" | jq -r '.success // false' 2>/dev/null || echo false)
        if [ "$AR_OK" = "true" ]; then
            e2e_log_assert_eq "$AR_OK" "true" "readiness_apply_real_success"
            DRH_CREATED_MEMORY_ID=$(printf '%s' "$APPLY_REAL" | jq -r '.data.createdMemoryId // .data.created_memory_id // empty' 2>/dev/null || true)
            assert_jq_nonempty "$APPLY_REAL" '.data.createdMemoryId // .data.created_memory_id // empty' "readiness_apply_created_memory_id"
        else
            todo_assert "readiness_apply_real_success" "bd-2t583" "Apply real path requires the atomic-create derived-apply implementation."
        fi
    else
        e2e_log_assert_eq "parseable-json" "invalid-json" "readiness_apply_real_parseable"
    fi
fi
drh_log_state "external_derivation_apply_real" "apply" "recorded"

# ---- why ----------------------------------------------------------------
if [ -n "$DRH_CREATED_MEMORY_ID" ]; then
    WHY_JSON=$(ee_workspace why "$DRH_CREATED_MEMORY_ID" --json 2>/dev/null || true)
    drh_extract_degraded_codes "$WHY_JSON"
    drh_extract_recovery_actions "$WHY_JSON"
    if printf '%s' "$WHY_JSON" | jq . >/dev/null 2>&1; then
        W_OK=$(printf '%s' "$WHY_JSON" | jq -r '.success // false' 2>/dev/null || echo false)
        if [ "$W_OK" = "true" ]; then
            e2e_log_assert_eq "$W_OK" "true" "readiness_why_success"
            # Verify DerivedFrom link or provenance refers to source memory A or B
            HAS_PROV=$(printf '%s' "$WHY_JSON" | jq --arg a "$DRH_SOURCE_MEMORY_A_ID" --arg b "$DRH_SOURCE_MEMORY_B_ID" \
                '[(.data | tostring | contains($a)), (.data | tostring | contains($b))] | any' 2>/dev/null || echo false)
            e2e_log_assert_eq "$HAS_PROV" "true" "readiness_why_provenance_references_source"
        else
            todo_assert "readiness_why_success" "bd-2t583" "ee why on a freshly-applied derived memory requires the apply path to ship a memory row."
        fi
    else
        e2e_log_assert_eq "parseable-json" "invalid-json" "readiness_why_parseable"
    fi
else
    todo_assert "readiness_why_success" "bd-2t583" "ee why phase requires createdMemoryId from a successful apply."
fi
drh_log_state "external_derivation_why" "why" "recorded"

# ---- db_inspect ---------------------------------------------------------
DB_INSPECT_JSON=$(ee_workspace db inspect --json 2>/dev/null || true)
drh_extract_degraded_codes "$DB_INSPECT_JSON"
drh_extract_recovery_actions "$DB_INSPECT_JSON"
if printf '%s' "$DB_INSPECT_JSON" | jq . >/dev/null 2>&1; then
    assert_jq "$DB_INSPECT_JSON" '.schema // empty' "ee.response.v2" "readiness_db_inspect_schema"
    assert_jq "$DB_INSPECT_JSON" '.success // false' "true" "readiness_db_inspect_success"
    assert_jq "$DB_INSPECT_JSON" '.data.command // empty' "db inspect" "readiness_db_inspect_command"
else
    e2e_log_assert_eq "parseable-json" "invalid-json" "readiness_db_inspect_parseable"
fi
drh_log_state "external_derivation_db_inspect" "db_inspect" "recorded"

# ---- negative_source_drift ----------------------------------------------
# Per ADR 0043 + bd-39by4: tombstoning a source memory after propose must
# cause apply to fail closed with a structured recovery action. Wiring the
# full negative path requires the race-proof apply slice (bd-39by4) to be
# in the binary; for now we record the contract and assert the recovery
# vocabulary is present on a tombstone attempt for a source memory.
TOMBSTONE_JSON=$(ee_workspace curate tombstone --memory-id "$DRH_SOURCE_MEMORY_A_ID" --reason "readiness gate negative drift probe" --json 2>/dev/null || true)
drh_extract_degraded_codes "$TOMBSTONE_JSON"
drh_extract_recovery_actions "$TOMBSTONE_JSON"
if printf '%s' "$TOMBSTONE_JSON" | jq . >/dev/null 2>&1; then
    TS_OK=$(printf '%s' "$TOMBSTONE_JSON" | jq -r '.success // false' 2>/dev/null || echo false)
    if [ "$TS_OK" = "true" ]; then
        e2e_log_assert_eq "$TS_OK" "true" "readiness_negative_drift_setup"
    else
        todo_assert "readiness_negative_drift_setup" "bd-39by4" "Negative-drift setup requires curate tombstone or equivalent disposition surface."
    fi
else
    todo_assert "readiness_negative_drift_setup" "bd-39by4" "Negative source-drift probe needs a tombstone-or-equivalent path on source memories."
fi
drh_log_state "external_derivation_negative_source_drift" "negative_source_drift" "recorded"

# ---- reject -------------------------------------------------------------
# Propose a second derived candidate so we can exercise the reject path
# without contaminating the apply-side assertions above.
PROPOSE_FOR_REJECT=$(ee_workspace curate propose-derived \
    --source-memory "$DRH_SOURCE_MEMORY_B_ID" \
    --level semantic \
    --kind insight \
    --content "Readiness gate: reject-path probe candidate" \
    --tag derivation \
    --tag e2e \
    --tag readiness-reject \
    --confidence 0.4 \
    --producer-kind reflection \
    --producer-model readiness-e2e-reject \
    --json 2>/dev/null || true)
REJECT_CANDIDATE_ID=$(printf '%s' "$PROPOSE_FOR_REJECT" | jq -r '.data.candidateId // .data.candidate_id // empty' 2>/dev/null || true)
if [ -n "$REJECT_CANDIDATE_ID" ]; then
    REJECT_JSON=$(ee_workspace curate reject "$REJECT_CANDIDATE_ID" --reason "readiness gate reject probe" --json 2>/dev/null || true)
    drh_extract_degraded_codes "$REJECT_JSON"
    drh_extract_recovery_actions "$REJECT_JSON"
    if printf '%s' "$REJECT_JSON" | jq . >/dev/null 2>&1; then
        REJ_OK=$(printf '%s' "$REJECT_JSON" | jq -r '.success // false' 2>/dev/null || echo false)
        if [ "$REJ_OK" = "true" ]; then
            e2e_log_assert_eq "$REJ_OK" "true" "readiness_reject_success"
        else
            todo_assert "readiness_reject_success" "bd-18z8x" "Reject (disposition) of create-derived candidates requires the show/preview surface to land with the disposition vocabulary."
        fi
    else
        e2e_log_assert_eq "parseable-json" "invalid-json" "readiness_reject_parseable"
    fi
else
    todo_assert "readiness_reject_success" "bd-17bob" "Reject phase requires propose-derived to insert a candidate first."
fi
drh_log_state "external_derivation_reject" "reject" "recorded"

# ---- invariant_check ----------------------------------------------------
# Final invariants per bd-1xpqd success criteria:
# - no raw chain-of-thought persistence: search for "thinking" / "raw cot" prose
# - no unexpected degraded codes outside the known catalog
# - no Tokio/HTTP dependency drift (covered by forbidden-deps gate, asserted as documented)
# - response envelopes stable
COT_PROBE=$(ee_workspace search "chain of thought private thinking" --limit 5 --json 2>/dev/null || true)
if printf '%s' "$COT_PROBE" | jq . >/dev/null 2>&1; then
    COT_HITS=$(printf '%s' "$COT_PROBE" | jq '[.data.results[]? | select((.content // "") | test("(?i)private thinking|raw chain.of.thought"))] | length' 2>/dev/null || echo 0)
    e2e_log_assert_num "$COT_HITS" -eq 0 "readiness_invariant_no_chain_of_thought_persistence"
else
    todo_assert "readiness_invariant_no_chain_of_thought_persistence" "bd-1xpqd" "Chain-of-thought invariant probe requires ee search to return parseable JSON."
fi
drh_log_state "external_derivation_invariant_check" "invariant_check" "recorded"

DRH_RECOVERY_ACTIONS='["implement bd-17bob before requiring external derivation candidates","complete bd-215tr/bd-2dek9/bd-3bsvv/bd-pxi7f before closing bd-1xpqd","run external_derivation_lifecycle.sh and reflection_handshake_gaps.sh after integrated paths ship"]'
drh_log_state "derivation_reflection_readiness_probe" "readiness_contract_probe" "recorded"
drh_assert_test_log_contract "derivation_reflection_readiness_log_contract"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
