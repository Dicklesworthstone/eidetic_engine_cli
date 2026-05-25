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
            assert_jq "$PROPOSE_JSON" '.success // false' "true" "readiness_reflect_propose_dry_run_success"
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

DRH_RECOVERY_ACTIONS='["implement bd-17bob before requiring external derivation candidates","complete bd-215tr/bd-2dek9/bd-3bsvv/bd-pxi7f before closing bd-1xpqd","run external_derivation_lifecycle.sh and reflection_handshake_gaps.sh after integrated paths ship"]'
drh_log_state "derivation_reflection_readiness_probe" "readiness_contract_probe" "recorded"
drh_assert_test_log_contract "derivation_reflection_readiness_log_contract"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
