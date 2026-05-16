#!/usr/bin/env bash
# SRR6.2 - mesh-off no-network and ordinary-output regression gate.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
export EE_MESH_ENABLED=0
epic_setup "mesh_off_no_network"
mesh_scenario_setup "mesh_off_no_network" 1

json_codes() {
    jq -r '
        [
          .degraded[]?.code,
          .data.degraded[]?.code,
          .. | objects | .code? // empty
        ]
        | map(select(type == "string"))
        | unique
        | join(",")
    ' 2>/dev/null
}

assert_no_mesh_degradation() {
    local json="${1:?json required}"
    local label="${2:?label required}"
    local codes
    codes="$(printf '%s' "$json" | json_codes || true)"
    if printf '%s\n' "$codes" | grep -Eiq 'mesh|tailscale|peer'; then
        e2e_log_assert_eq "$codes" "no mesh/tailscale/peer degradation" "$label"
    else
        e2e_log_assert_eq "true" "true" "$label"
    fi
}

assert_no_mesh_data_key() {
    local json="${1:?json required}"
    local label="${2:?label required}"
    local count
    count="$(printf '%s' "$json" | jq '[.data? | objects | keys[] | select(test("mesh|tailscale|peer"; "i"))] | length' 2>/dev/null || echo 0)"
    e2e_log_assert_eq "$count" "0" "$label"
}

listener_snapshot() {
    if command -v lsof >/dev/null 2>&1; then
        lsof -nP -iTCP -sTCP:LISTEN 2>/dev/null | awk 'NR > 1 {print $1 " " $2 " " $9}' | sort -u
    elif [ -x /usr/sbin/lsof ]; then
        /usr/sbin/lsof -nP -iTCP -sTCP:LISTEN 2>/dev/null | awk 'NR > 1 {print $1 " " $2 " " $9}' | sort -u
    else
        return 127
    fi
}

mesh_phase_log "action" "node01" "status --json mesh capability probe"
STATUS_JSON="$(ee_workspace status --json 2>/dev/null || true)"
if printf '%s' "$STATUS_JSON" | jq . >/dev/null 2>&1; then
    mesh_phase_log "assert" "node01" "status JSON parses and reports mesh capability posture"
    assert_jq_nonempty "$STATUS_JSON" '.data.capabilities.mesh // empty' "mesh_off_status_reports_mesh_capability"
    MESH_POSTURE="$(printf '%s' "$STATUS_JSON" | jq -r '.data.capabilities.mesh // empty')"
    case "$MESH_POSTURE" in
        disabled|pending|unavailable|degraded|ok)
            e2e_log_assert_eq "true" "true" "mesh_off_status_mesh_capability_enum"
            ;;
        *)
            e2e_log_assert_eq "$MESH_POSTURE" "known mesh capability posture" "mesh_off_status_mesh_capability_enum"
            ;;
    esac
else
    e2e_log_note "mesh_off_status_json_unparseable bytes=${#STATUS_JSON}"
    e2e_log_assert_eq "false" "true" "mesh_off_status_json_parses"
fi

mesh_phase_log "action" "node01" "ordinary remember/search/context commands with mesh disabled"
MEMORY_JSON="$(ee_workspace remember --level procedural --kind rule "Mesh-off e2e ordinary command fixture." --json 2>/dev/null || true)"
SEARCH_JSON="$(ee_workspace search "mesh-off ordinary command fixture" --json 2>/dev/null || true)"
CONTEXT_JSON="$(ee_workspace context "mesh-off ordinary command fixture" --max-tokens 500 --json 2>/dev/null || true)"
MEMORY_ID="$(printf '%s' "$MEMORY_JSON" | jq -r '.data.memory_id // empty' 2>/dev/null || true)"
PACK_JSON="$(ee_workspace pack "mesh-off ordinary command fixture" --max-tokens 500 --json 2>/dev/null || true)"
WHY_JSON=""
if [ -n "$MEMORY_ID" ]; then
    WHY_JSON="$(ee_workspace why "$MEMORY_ID" --json 2>/dev/null || true)"
else
    e2e_log_assert_eq "<empty>" "memory id" "mesh_off_why_memory_id_present"
fi

for pair in \
    "remember:$MEMORY_JSON" \
    "search:$SEARCH_JSON" \
    "context:$CONTEXT_JSON" \
    "pack:$PACK_JSON" \
    "why:$WHY_JSON"
do
    label="${pair%%:*}"
    json="${pair#*:}"
    if printf '%s' "$json" | jq . >/dev/null 2>&1; then
        mesh_phase_log "assert" "node01" "${label} JSON has no mesh degraded code or data key"
        assert_no_mesh_degradation "$json" "mesh_off_${label}_has_no_mesh_degradation"
        assert_no_mesh_data_key "$json" "mesh_off_${label}_has_no_mesh_data_key"
    else
        e2e_log_note "mesh_off_${label}_json_unparseable bytes=${#json}"
        e2e_log_assert_eq "false" "true" "mesh_off_${label}_json_parses"
    fi
done

if BEFORE_LISTENERS="$(listener_snapshot)"; then
    mesh_phase_log "action" "node01" "listener snapshot around mesh-off status"
    ee_workspace status --json >/dev/null 2>&1 || true
    AFTER_LISTENERS="$(listener_snapshot || true)"
    NEW_MESH_LISTENERS="$(comm -13 <(printf '%s\n' "$BEFORE_LISTENERS") <(printf '%s\n' "$AFTER_LISTENERS") | grep -E 'ee|eidetic|mesh|tailscale' || true)"
    if [ -z "$NEW_MESH_LISTENERS" ]; then
        e2e_log_assert_eq "true" "true" "mesh_off_status_opens_no_mesh_listener"
    else
        e2e_log_note "mesh_off_new_listeners=$NEW_MESH_LISTENERS"
        e2e_log_assert_eq "$NEW_MESH_LISTENERS" "no new ee/mesh/tailscale listeners" "mesh_off_status_opens_no_mesh_listener"
    fi
else
    e2e_log_note "mesh_off_listener_snapshot_skipped reason=lsof_unavailable"
fi

mesh_phase_log "cleanup" "node01" "mesh_off_no_network_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} node_count=${MESH_NODE_COUNT}"
e2e_log_note "mesh_off_no_network_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL}"
if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
