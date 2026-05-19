#!/usr/bin/env bash
# SRR6.38 emergency mesh disable and incident containment smoke.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/e2e_overhaul/lib/shared.sh"

require_jq
epic_setup "mesh_emergency_disable"
mesh_scenario_setup "mesh_emergency_disable" 1

log_event() {
    local phase="${1:?phase required}"
    local ok="${2:?ok required}"
    local detail="${3:-}"
    jq -c -n \
        --arg phase "$phase" \
        --arg ok "$ok" \
        --arg detail "$detail" \
        '{
          schema: "ee.test_event.v1",
          test: "mesh_emergency_disable",
          bead_id: "bd-2t243",
          surface: "ee mesh disable",
          phase: $phase,
          ok: ($ok == "true"),
          detail: $detail
        }'
}

assert_json_bool() {
    local json="${1:?json required}"
    local pointer="${2:?jq pointer required}"
    local expected="${3:?expected required}"
    local phase="${4:?phase required}"
    local actual
    actual="$(printf '%s' "$json" | jq -r "$pointer")"
    if [ "$actual" = "$expected" ]; then
        log_event "$phase" "true" "$actual"
    else
        log_event "$phase" "false" "expected=$expected actual=$actual"
        return 1
    fi
}

export EE_MESH_ENABLED=1
export EE_MESH_MODE=cache

STATUS_BEFORE="$(ee_workspace mesh status --json)"
MESH_ENABLED_BEFORE="$(printf '%s' "$STATUS_BEFORE" | jq -r '.data.meshEnabled // .meshEnabled // empty')"
if [ "$MESH_ENABLED_BEFORE" = "true" ]; then
    log_event "mesh_enabled_before" "true" "$MESH_ENABLED_BEFORE"
else
    log_event "mesh_enabled_before" "false" "$MESH_ENABLED_BEFORE"
    exit 1
fi

DRY_RUN="$(ee_workspace mesh disable --dry-run --reason "e2e incident preview" --json)"
assert_json_bool "$DRY_RUN" '.data.disableRequested // .disableRequested' "true" "disable_requested"
assert_json_bool "$DRY_RUN" '.data.listenerStopped // .listenerStopped' "true" "listener_stopped"
QUEUED_CANCELLED="$(printf '%s' "$DRY_RUN" | jq -r '.data.queuedExportsCancelled // .queuedExportsCancelled')"
if [ "$QUEUED_CANCELLED" = "0" ]; then
    log_event "queued_exports_cancelled" "true" "$QUEUED_CANCELLED"
else
    log_event "queued_exports_cancelled" "false" "$QUEUED_CANCELLED"
    exit 1
fi

DISABLE_JSON="$(ee_workspace mesh disable --reason "e2e incident containment" --json)"
assert_json_bool "$DISABLE_JSON" '.data.meshEnabledAfter // .meshEnabledAfter' "false" "mesh_enabled_after_disable"

MEMORY_JSON="$(ee_workspace remember --level procedural --kind rule "Mesh containment keeps local search readable." --json)"
MEMORY_ID="$(printf '%s' "$MEMORY_JSON" | jq -r '.data.memory_id // empty')"
SEARCH_JSON="$(ee_workspace search "Mesh containment keeps local search readable" --json)"
if [ -n "$MEMORY_ID" ] && printf '%s' "$SEARCH_JSON" | jq -e '.success == true' >/dev/null; then
    log_event "local_search_still_works" "true" "$MEMORY_ID"
else
    log_event "local_search_still_works" "false" "memory_id=$MEMORY_ID"
    exit 1
fi

if ee_workspace mesh reenable --json >/tmp/ee_mesh_reenable_without_confirm.json 2>/tmp/ee_mesh_reenable_without_confirm.err; then
    log_event "reenable_requires_explicit_command" "false" "reenable without confirmation succeeded"
    exit 1
else
    log_event "reenable_requires_explicit_command" "true" "missing confirmation rejected"
fi

REENABLE_JSON="$(ee_workspace mesh reenable --confirm-reenable --json)"
assert_json_bool "$REENABLE_JSON" '.data.meshEnabledAfter // .meshEnabledAfter' "true" "reenable_explicit_command"

mesh_phase_log "cleanup" "node01" "mesh_emergency_disable complete"
