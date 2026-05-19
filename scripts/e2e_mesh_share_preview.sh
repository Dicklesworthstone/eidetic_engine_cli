#!/usr/bin/env bash
# bd-1ps4c - share-preview dry run and consent-audit UX e2e driver.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/e2e_overhaul/lib/shared.sh"

require_jq
export EE_MESH_ENABLED=1
export EE_MESH_MODE=cache

epic_setup "mesh_share_preview"
mesh_scenario_setup "mesh_share_preview" 1

emit_preview_events() {
    local json="${1:?json required}"
    printf '%s' "$json" | jq -r '.data.events[]?.event' 2>/dev/null | while read -r event; do
        [ -z "$event" ] && continue
        _e2e_emit_event "$event" \
            "phase" "assert" \
            "meshScenario" "$MESH_SCENARIO_NAME" \
            "meshNode" "node01"
    done
}

mesh_phase_log "action" "node01" "seed memories for share preview"
ee_workspace remember --level procedural --kind rule \
    "Share preview safe public fixture." --json >/dev/null
ee_workspace remember --level episodic --kind decision --allow-secret-mention \
    "Share preview secret fixture API_KEY=sk-proj-local-only-token-000000000000000000000000000000000000000000." \
    --json >/dev/null

mesh_phase_log "action" "node01" "metadata-only share preview"
PREVIEW_JSON="$(ee_workspace share preview --peer peer_alpha --json --max-examples 8 2>/dev/null || true)"
assert_jq "$PREVIEW_JSON" '.success // false' "true" "share_preview_metadata_success"
assert_jq "$PREVIEW_JSON" '.data.dryRun' "true" "share_preview_is_dry_run"
assert_jq "$PREVIEW_JSON" '.data.exportPerformed' "false" "share_preview_export_not_performed"
assert_jq "$PREVIEW_JSON" '.data.preview.estimatedBodyBytes' "0" "share_preview_metadata_body_bytes_zero"
assert_jq "$PREVIEW_JSON" '.data.preview.estimatedEmbeddingBytes' "0" "share_preview_metadata_embedding_bytes_zero"
assert_jq_nonempty "$PREVIEW_JSON" '.data.preview.deniedClasses[]? | select(. == "redaction_class:body_denied")' \
    "share_preview_body_denied_class"
assert_jq_nonempty "$PREVIEW_JSON" '.data.preview.deniedClasses[]? | select(. == "redaction_class:embedding_denied")' \
    "share_preview_embedding_denied_class"
assert_jq "$PREVIEW_JSON" '[.. | strings | contains("sk-proj-local-only-token")] | any' "false" \
    "share_preview_does_not_leak_secret"
emit_preview_events "$PREVIEW_JSON"

mesh_phase_log "action" "node01" "record consent audit without export"
CONSENT_JSON="$(ee_workspace share preview --peer peer_alpha --json --include-body --record-consent --max-examples 8 2>/dev/null || true)"
assert_jq "$CONSENT_JSON" '.success // false' "true" "share_preview_consent_success"
assert_jq "$CONSENT_JSON" '.data.exportPerformed' "false" "share_preview_consent_still_no_export"
assert_jq_nonempty "$CONSENT_JSON" '.data.consentAudit.auditId // empty' "share_preview_consent_audit_id"
assert_jq "$CONSENT_JSON" '.data.consentAudit.consentRecorded' "true" "share_preview_consent_recorded"
assert_jq "$CONSENT_JSON" '.data.consentAudit.exportAfterConsent' "false" "share_preview_no_export_after_consent"
emit_preview_events "$CONSENT_JSON"

mesh_phase_log "assert" "node01" "share preview structured events captured"
assert_jq_nonempty "$PREVIEW_JSON" '.data.events[]? | select(.event == "preview_generated") | .event' \
    "share_preview_event_preview_generated"
assert_jq_nonempty "$PREVIEW_JSON" '.data.events[]? | select(.event == "export_not_performed") | .event' \
    "share_preview_event_export_not_performed"
assert_jq_nonempty "$CONSENT_JSON" '.data.events[]? | select(.event == "consent_recorded") | .event' \
    "share_preview_event_consent_recorded"
assert_jq_nonempty "$CONSENT_JSON" '.data.events[]? | select(.event == "export_after_consent") | .event' \
    "share_preview_event_export_after_consent"
