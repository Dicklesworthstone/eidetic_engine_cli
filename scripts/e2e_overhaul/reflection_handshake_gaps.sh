#!/usr/bin/env bash
# Logged E2E driver for the no-LLM reflection handshake gap surface.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
# shellcheck source=scripts/e2e_overhaul/lib/derivation_reflection.sh
source "$SCRIPT_DIR/lib/derivation_reflection.sh"

require_jq
epic_setup "reflection_handshake_gaps"
drh_seed_derivation_sources "reflection_handshake_gaps"

REFLECT_HELP=$(ee_global reflect --help 2>/dev/null || true)
if printf '%s' "$REFLECT_HELP" | grep -q "reflect"; then
    e2e_log_assert_eq "true" "true" "reflection_reflect_help_available"
    REFLECT_KEY_PATH="$EPIC_WORKSPACE/reflection_hmac.key"
    printf '%s\n' "reflection-handshake-gaps-hmac-key" >"$REFLECT_KEY_PATH"
    PROPOSE_JSON=$(EE_REFLECTION_HMAC_KEY_ID="reflect-e2e-key" \
        EE_REFLECTION_HMAC_KEY_PATH="$REFLECT_KEY_PATH" \
        ee_workspace reflect propose \
        --kind gaps \
        --source "$DRH_SOURCE_MEMORY_A_ID" \
        --source "$DRH_SOURCE_MEMORY_B_ID" \
        --json 2>/dev/null || true)
    drh_capture_request_identity "$PROPOSE_JSON"
    drh_extract_degraded_codes "$PROPOSE_JSON"
    drh_extract_recovery_actions "$PROPOSE_JSON"
    assert_jq_nonempty "$PROPOSE_JSON" '.data.requestId // .requestId // empty' "reflection_request_id_present"
    assert_jq_nonempty "$PROPOSE_JSON" '.data.requestHash // .requestHash // empty' "reflection_request_hash_present"
    assert_jq "$PROPOSE_JSON" '.data.request.challenge.keyId // empty' "reflect-e2e-key" "reflection_request_challenge_key_id"
    assert_jq "$PROPOSE_JSON" '.data.ledgerOutcome.status // empty' "inserted" "reflection_request_ledger_inserted"
    if printf '%s' "$PROPOSE_JSON" | grep -Fq "$REFLECT_KEY_PATH"; then
        e2e_log_assert_eq "hmac-key-path-leaked" "redacted" "reflection_request_redacts_hmac_key_path"
    else
        e2e_log_assert_eq "redacted" "redacted" "reflection_request_redacts_hmac_key_path"
    fi
else
    DRH_RECOVERY_ACTIONS='["implement bd-ogqf6 request ledger before accepting reflection results","run ee reflect propose after the CLI surface exists"]'
    todo_assert "reflection_reflect_help_available" "bd-ogqf6" "ee reflect propose/ingest is intentionally unshipped until the request ledger and HMAC protocol land."
fi

drh_log_state "reflection_handshake_gap_probe" "reflect_surface_probe" "recorded"
drh_assert_test_log_contract "reflection_handshake_gaps_log_contract"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
