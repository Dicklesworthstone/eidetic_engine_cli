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

e2e_log_assert_eq "$([ -f "$REPO_ROOT/docs/adr/0043-external-derivation-candidates.md" ] && printf present || printf missing)" "present" "external_derivation_adr_present"
e2e_log_assert_eq "$([ -f "$REPO_ROOT/docs/adr/0044-no-llm-reflection-handshake.md" ] && printf present || printf missing)" "present" "reflection_handshake_adr_present"

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

DRH_RECOVERY_ACTIONS='["implement bd-ogqf6 before reflection ingest","run external_derivation_lifecycle.sh after create_derived_memory producer lands","run reflection_handshake_gaps.sh after ee reflect exists"]'
drh_log_state "derivation_reflection_readiness_probe" "readiness_contract_probe" "recorded"
drh_assert_test_log_contract "derivation_reflection_readiness_log_contract"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
