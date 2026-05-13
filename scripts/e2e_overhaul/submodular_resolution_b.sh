#!/usr/bin/env bash
# N5.1 - ADR 0031 Resolution B selectionAudit rename e2e driver.
#
# Exercises `ee context` across renderers and the explicit legacy transition
# path. The canonical pack output must expose selectionAudit and must not emit
# selectionCertificate unless the legacy opt-in environment variable is set.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
epic_setup "epic_N_submodular_resolution_b"

ee_workspace remember "Run cargo fmt --check before release." \
    --level procedural \
    --kind rule \
    --tags "n5-resolution-b" \
    --json >/dev/null
ee_workspace remember "Run cargo clippy --all-targets -- -D warnings before release." \
    --level procedural \
    --kind rule \
    --tags "n5-resolution-b" \
    --json >/dev/null
ee_workspace index rebuild --json >/dev/null

QUERY="release selection audit rename"
CONTEXT_JSON=$(ee_workspace context "$QUERY" --max-tokens 1500 --format json || true)
if ! printf '%s' "$CONTEXT_JSON" | jq . >/dev/null 2>&1; then
    e2e_log_assert_eq "invalid" "parseable" "n5_resolution_b_context_json_parses"
    exit 0
fi

assert_jq_nonempty "$CONTEXT_JSON" \
    '.data.pack.selectionAudit.algorithmId // empty' \
    "n5_resolution_b_algorithm_id_present"
assert_jq_nonempty "$CONTEXT_JSON" \
    '.data.pack.selectionAudit.algorithmDescription // empty' \
    "n5_resolution_b_algorithm_description_present"
assert_jq "$CONTEXT_JSON" \
    '.data.pack | has("selectionCertificate")' \
    "false" \
    "n5_resolution_b_no_selection_certificate_key"
assert_jq "$CONTEXT_JSON" \
    '.data.pack.selectionAudit | has("guaranteeStatus")' \
    "false" \
    "n5_resolution_b_no_guarantee_status"

LEGACY_JSON=$(EE_LEGACY_SELECTION_CERTIFICATE=1 ee_workspace context "$QUERY" \
    --max-tokens 1500 --format json || true)
if printf '%s' "$LEGACY_JSON" | jq . >/dev/null 2>&1; then
    assert_jq "$LEGACY_JSON" \
        '.data.pack.deprecation.deprecatedField' \
        "selectionCertificate" \
        "n5_resolution_b_legacy_deprecated_field"
    assert_jq "$LEGACY_JSON" \
        '.data.pack.deprecation.replacementField' \
        "selectionAudit" \
        "n5_resolution_b_legacy_replacement_field"
    assert_jq_nonempty "$LEGACY_JSON" \
        '.data.pack.selectionCertificate.algorithmId // empty' \
        "n5_resolution_b_legacy_payload_present"
else
    e2e_log_assert_eq "invalid" "parseable" "n5_resolution_b_legacy_json_parses"
fi

for FORMAT in markdown toon jsonl compact hook mermaid human; do
    FORMAT_OUTPUT=$(ee_workspace context "$QUERY" --max-tokens 1500 --format "$FORMAT" || true)
    if printf '%s' "$FORMAT_OUTPUT" | grep -Fq "selectionCertificate"; then
        e2e_log_assert_eq "present" "absent" "n5_resolution_b_${FORMAT}_omits_old_field"
    else
        e2e_log_assert_eq "absent" "absent" "n5_resolution_b_${FORMAT}_omits_old_field"
    fi
done
