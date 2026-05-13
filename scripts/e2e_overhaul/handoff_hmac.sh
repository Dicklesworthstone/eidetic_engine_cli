#!/usr/bin/env bash
# M5 — handoff HMAC integrity e2e coverage.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
epic_setup "m5_handoff_hmac"

remember_handoff_memory() {
    local content="${1:?content required}"
    ee_workspace remember "$content" \
        --level procedural \
        --kind rule \
        --no-propose-candidates \
        --json >/dev/null
}

capsule_path() {
    local name="${1:?name required}"
    printf '%s\n' "$EPIC_WORKSPACE/$name-capsule.json"
}

hmac_prefix() {
    jq -r '.integrity.hmacPrefix // empty' "$1"
}

tamper_capsule_section() {
    local path="${1:?capsule path required}"
    local tmp="${path}.tmp"
    jq '.sections[0].content = "M5 tampered handoff capsule body"' "$path" >"$tmp" \
        && mv "$tmp" "$path"
}

remember_handoff_memory "M5 handoff HMAC e2e baseline memory."

DEFAULT_CAPSULE=$(capsule_path "default")
CREATE_JSON=$(ee_workspace handoff create --out "$DEFAULT_CAPSULE" --profile resume --json)
assert_jq "$CREATE_JSON" '.capsule_id[0:5]' "hcap_" "m5_default_create_capsule_id"
DEFAULT_PREFIX=$(hmac_prefix "$DEFAULT_CAPSULE")
e2e_log_assert_eq "${#DEFAULT_PREFIX}" "8" "m5_default_hmac_prefix_len"

DEFAULT_RESUME=$(ee_workspace handoff resume "$DEFAULT_CAPSULE" --json)
assert_jq "$DEFAULT_RESUME" '.capsule_id[0:5]' "hcap_" "m5_default_resume_success"

ROTATE_JSON=$(ee_workspace handoff rotate-key --capsule "$DEFAULT_CAPSULE" --json)
assert_jq "$ROTATE_JSON" '.body_preserved' "true" "m5_rotate_preserves_body"
assert_jq "$ROTATE_JSON" '.canonical_content_hash_before == .canonical_content_hash_after' \
    "true" "m5_rotate_preserves_canonical_hash"
ROTATED_PREFIX=$(hmac_prefix "$DEFAULT_CAPSULE")
if [ "$DEFAULT_PREFIX" != "$ROTATED_PREFIX" ]; then
    e2e_log_assert_eq "changed" "changed" "m5_rotate_changes_hmac_prefix"
else
    e2e_log_assert_eq "$DEFAULT_PREFIX" "not-$ROTATED_PREFIX" "m5_rotate_changes_hmac_prefix"
fi
ROTATED_RESUME=$(ee_workspace handoff resume "$DEFAULT_CAPSULE" --json)
assert_jq "$ROTATED_RESUME" '.capsule_id[0:5]' "hcap_" "m5_rotated_resume_success"

TAMPERED_CAPSULE=$(capsule_path "tampered")
ee_workspace handoff create --out "$TAMPERED_CAPSULE" --profile resume --json >/dev/null
tamper_capsule_section "$TAMPERED_CAPSULE"
TAMPERED_JSON=$(e2e_log_command "$EE_BINARY" handoff resume "$TAMPERED_CAPSULE" \
    --workspace "$EPIC_WORKSPACE" --json)
TAMPERED_RC=$?
e2e_log_assert_eq "$TAMPERED_RC" "6" "m5_tampered_resume_exit"
assert_jq "$TAMPERED_JSON" '.error.code // empty' \
    "handoff_capsule_tampered" "m5_tampered_resume_code"

SKIP_JSON=$(ee_workspace handoff resume "$TAMPERED_CAPSULE" --insecure-skip-hmac --json)
assert_jq "$SKIP_JSON" \
    '[.degradations[]? | select(.code == "handoff_hmac_skipped" and .severity == "high")] | length' \
    "1" "m5_insecure_skip_degradation"

STRICT_CAPSULE=$(capsule_path "strict")
STRICT_HOME_A="$EPIC_WORKSPACE/strict-home-a"
STRICT_HOME_B="$EPIC_WORKSPACE/strict-home-b"
mkdir -p "$STRICT_HOME_A" "$STRICT_HOME_B"
HOME="$STRICT_HOME_A" ee_workspace handoff create \
    --out "$STRICT_CAPSULE" \
    --profile resume \
    --bind-to-machine \
    --json >/dev/null
STRICT_OK_JSON=$(HOME="$STRICT_HOME_A" ee_workspace handoff resume "$STRICT_CAPSULE" --json)
assert_jq "$STRICT_OK_JSON" '.capsule_id[0:5]' "hcap_" "m5_strict_same_home_resume"
STRICT_MISSING_JSON=$(HOME="$STRICT_HOME_B" e2e_log_command "$EE_BINARY" handoff resume \
    "$STRICT_CAPSULE" --workspace "$EPIC_WORKSPACE" --json)
STRICT_MISSING_RC=$?
e2e_log_assert_eq "$STRICT_MISSING_RC" "6" "m5_strict_missing_salt_exit"
assert_jq "$STRICT_MISSING_JSON" '.error.code // empty' \
    "strict_mode_no_salt_file" "m5_strict_missing_salt_code"

if printf '%s\n%s\n' "$ROTATE_JSON" "$SKIP_JSON" | grep -Eq 'base64url:[A-Za-z0-9_-]{32,}'; then
    e2e_log_assert_eq "full-hmac-leaked" "redacted" "m5_hmac_outputs_redact_full_hmac"
else
    e2e_log_assert_eq "redacted" "redacted" "m5_hmac_outputs_redact_full_hmac"
fi
