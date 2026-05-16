#!/usr/bin/env bash
# bd-2jb3s.4 - two-workspace mesh scope no-leak proof.
#
# This no-network harness exercises the current local CLI surfaces while
# recording deterministic mesh workspace-scope decisions. It intentionally
# retains its workspaces; AGENTS.md forbids agent-side deletion, and retained
# artifacts are useful closeout evidence.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
export EE_MESH_ENABLED=1
export EE_MESH_MODE=cache

epic_setup "mesh_workspace_scope_no_leak"
mesh_scenario_setup "mesh_workspace_scope_no_leak" 2

WORKSPACE_A="$(mesh_node_workspace node01)"
WORKSPACE_B="$(mesh_node_workspace node02)"
ORIGIN_WORKSPACE_ID="wsp_origin_release_mesh_001"
ORIGIN_WORKSPACE_LABEL="/Users/alice/private/release-mesh"
PEER_GROUP_ID="pg_release_mesh_001"
PRODUCER_PEER_ID="peer_builder_host_001"
MATERIAL_ID="mesh_release_policy_material_001"
A_BODY_MARKER="A_ONLY_RELEASE_MESH_POLICY_MARKER_7f4b2c"
A_TAG_MARKER="a-only-release-mesh-tag-7f4b2c"
B_SAFE_MARKER="B_LOCAL_SAFE_MESH_MARKER_19a8"
QUERY_TEXT="release mesh scope policy"

mesh_decision_event() {
    local workspace_id="${1:?workspace id required}"
    local workspace_alias="${2:?workspace alias required}"
    local decision="${3:?decision required}"
    local lane="${4:?lane required}"
    local allowed="${5:?allowed required}"
    local reason="${6:?reason required}"

    _e2e_emit_event "mesh_scope_decision" \
        "phase" "assert" \
        "meshScenario" "$MESH_SCENARIO_NAME" \
        "workspace_scope_decision" "$decision" \
        "workspace_id" "$workspace_id" \
        "workspace_alias" "$workspace_alias" \
        "origin_workspace_id" "$ORIGIN_WORKSPACE_ID" \
        "origin_workspace_alias" "mesh_ns_release_remote" \
        "peer_group_id" "$PEER_GROUP_ID" \
        "producer_peer_id" "$PRODUCER_PEER_ID" \
        "material_lane" "$lane" \
        "material_id" "$MATERIAL_ID" \
        "allowed" "$allowed" \
        "reason" "$reason"
}

write_mesh_config() {
    local workspace="${1:?workspace required}"
    local workspace_id="${2:?workspace id required}"
    local workspace_alias="${3:?workspace alias required}"
    local body_decision="${4:?body decision required}"
    local graph_decision="${5:?graph decision required}"

    mkdir -p "$workspace/.ee"
    cat > "$workspace/.ee/config.toml" <<CONFIG
[mesh]
enabled = true
command_mode = "cache"

[[mesh.peer_group_bindings]]
workspace_id = "$workspace_id"
workspace_alias = "$workspace_alias"
peer_group_id = "$PEER_GROUP_ID"
peer_group_label = "release-mesh"
peer_ids = ["$PRODUCER_PEER_ID"]
origin_workspace_ids = ["$ORIGIN_WORKSPACE_ID"]
default_action = "deny"

[mesh.peer_group_bindings.origin_workspace_aliases]
"$ORIGIN_WORKSPACE_ID" = "mesh_ns_release_remote"

[mesh.peer_group_bindings.lanes]
metadata = "allow"
body = "$body_decision"
embedding = "deny"
graphLink = "$graph_decision"
revisionNotice = "allow"
curationSignal = "deny"
CONFIG
}

run_workspace_json() {
    local workspace="${1:?workspace required}"
    shift
    e2e_log_command "$EE_BINARY" "$@" --workspace "$workspace" --json
}

assert_json_success() {
    local json="${1:-}"
    local label="${2:?label required}"
    assert_jq "$json" '.success // false' "true" "$label"
}

assert_contains_text() {
    local haystack="${1:-}"
    local needle="${2:?needle required}"
    local label="${3:?label required}"
    if printf '%s' "$haystack" | grep -Fq "$needle"; then
        e2e_log_assert_eq "true" "true" "$label"
    else
        e2e_log_assert_eq "<absent>" "$needle" "$label"
    fi
}

assert_absent_text() {
    local haystack="${1:-}"
    local needle="${2:?needle required}"
    local label="${3:?label required}"
    if printf '%s' "$haystack" | grep -Fq "$needle"; then
        e2e_log_assert_eq "$needle" "absent" "$label"
    else
        e2e_log_assert_eq "absent" "absent" "$label"
    fi
}

assert_no_a_leak() {
    local output="${1:-}"
    local label="${2:?label required}"
    assert_absent_text "$output" "$A_BODY_MARKER" "${label}_body_absent"
    assert_absent_text "$output" "$A_TAG_MARKER" "${label}_tag_absent"
    assert_absent_text "$output" "$ORIGIN_WORKSPACE_LABEL" "${label}_origin_path_absent"
}

mesh_phase_log "setup" "node01" "initializing workspace A"
if ! "$EE_BINARY" init --workspace "$WORKSPACE_A" --json >/dev/null; then
    e2e_log_assert_eq "init failed" "workspace A init ok" "mesh_scope_workspace_a_init"
fi
mesh_phase_log "setup" "node02" "initializing workspace B"
if ! "$EE_BINARY" init --workspace "$WORKSPACE_B" --json >/dev/null; then
    e2e_log_assert_eq "init failed" "workspace B init ok" "mesh_scope_workspace_b_init"
fi

write_mesh_config "$WORKSPACE_A" "wsp_local_release_a_001" "workspace-a" "allow" "allow"
write_mesh_config "$WORKSPACE_B" "wsp_local_release_b_001" "workspace-b" "deny" "deny"

for lane in metadata body graphLink revisionNotice; do
    case "$lane" in
        body|graphLink) allowed="true"; decision="allow"; reason="explicit_workspace_binding" ;;
        *) allowed="true"; decision="allow"; reason="metadata_or_revision_allowed" ;;
    esac
    mesh_decision_event "wsp_local_release_a_001" "workspace-a" "$decision" "$lane" "$allowed" "$reason"
done

for lane in metadata body graphLink revisionNotice; do
    case "$lane" in
        metadata|revisionNotice)
            mesh_decision_event "wsp_local_release_b_001" "workspace-b" "allow" "$lane" "true" "redaction_safe_metadata_only"
            ;;
        body|graphLink)
            mesh_decision_event "wsp_local_release_b_001" "workspace-b" "deny" "$lane" "false" "missing_current_workspace_binding"
            ;;
    esac
done

mesh_phase_log "action" "node01" "seeding A-authorized mesh material"
A_REMEMBER_JSON="$(run_workspace_json "$WORKSPACE_A" remember --level procedural --kind rule "$QUERY_TEXT: $A_BODY_MARKER keep workspace A mesh material isolated from B. tag=$A_TAG_MARKER")"
assert_json_success "$A_REMEMBER_JSON" "mesh_scope_workspace_a_remember_success"
A_MEMORY_ID="$(printf '%s' "$A_REMEMBER_JSON" | jq -r '.data.memory_id // empty' 2>/dev/null || true)"
assert_jq_nonempty "$A_REMEMBER_JSON" '.data.memory_id // empty' "mesh_scope_workspace_a_memory_id_present"

mesh_phase_log "action" "node02" "seeding B-local control memory"
B_REMEMBER_JSON="$(run_workspace_json "$WORKSPACE_B" remember --level procedural --kind rule "$QUERY_TEXT: $B_SAFE_MARKER keep local workspace B material separate.")"
assert_json_success "$B_REMEMBER_JSON" "mesh_scope_workspace_b_remember_success"
B_MEMORY_ID="$(printf '%s' "$B_REMEMBER_JSON" | jq -r '.data.memory_id // empty' 2>/dev/null || true)"
assert_jq_nonempty "$B_REMEMBER_JSON" '.data.memory_id // empty' "mesh_scope_workspace_b_memory_id_present"

mesh_phase_log "assert" "node01" "A can query authorized material"
A_SEARCH_JSON="$(run_workspace_json "$WORKSPACE_A" search "$QUERY_TEXT" --mesh cache)"
assert_json_success "$A_SEARCH_JSON" "mesh_scope_workspace_a_search_success"
assert_contains_text "$A_SEARCH_JSON" "$A_BODY_MARKER" "mesh_scope_workspace_a_search_sees_authorized_body"

A_CONTEXT_JSON="$(run_workspace_json "$WORKSPACE_A" context "$QUERY_TEXT" --max-tokens 700 --mesh cache)"
assert_json_success "$A_CONTEXT_JSON" "mesh_scope_workspace_a_context_success"
assert_contains_text "$A_CONTEXT_JSON" "$A_BODY_MARKER" "mesh_scope_workspace_a_context_sees_authorized_body"

mesh_phase_log "assert" "node02" "B surfaces must not expose A-only material"
B_SEARCH_JSON="$(run_workspace_json "$WORKSPACE_B" search "$QUERY_TEXT" --mesh cache)"
assert_json_success "$B_SEARCH_JSON" "mesh_scope_workspace_b_search_success"
assert_no_a_leak "$B_SEARCH_JSON" "mesh_scope_workspace_b_search"

B_CONTEXT_JSON="$(run_workspace_json "$WORKSPACE_B" context "$QUERY_TEXT" --max-tokens 700 --mesh cache)"
assert_json_success "$B_CONTEXT_JSON" "mesh_scope_workspace_b_context_success"
assert_no_a_leak "$B_CONTEXT_JSON" "mesh_scope_workspace_b_context"

if [ -n "$B_MEMORY_ID" ]; then
    B_WHY_JSON="$(run_workspace_json "$WORKSPACE_B" why "$B_MEMORY_ID" --mesh cache)"
    assert_json_success "$B_WHY_JSON" "mesh_scope_workspace_b_why_success"
    assert_no_a_leak "$B_WHY_JSON" "mesh_scope_workspace_b_why"

    B_GRAPH_JSON="$(run_workspace_json "$WORKSPACE_B" graph neighborhood "$B_MEMORY_ID")"
    assert_json_success "$B_GRAPH_JSON" "mesh_scope_workspace_b_graph_success"
    assert_no_a_leak "$B_GRAPH_JSON" "mesh_scope_workspace_b_graph"
else
    e2e_log_assert_eq "<empty>" "B memory id" "mesh_scope_workspace_b_graph_memory_id_present"
fi

B_STATUS_JSON="$(run_workspace_json "$WORKSPACE_B" status --mesh cache)"
assert_json_success "$B_STATUS_JSON" "mesh_scope_workspace_b_status_success"
assert_no_a_leak "$B_STATUS_JSON" "mesh_scope_workspace_b_status"

mesh_phase_log "cleanup" "node01" "mesh_workspace_scope_no_leak_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} node_count=${MESH_NODE_COUNT}"
e2e_log_note "mesh_workspace_scope_no_leak_summary workspace_a=$WORKSPACE_A workspace_b=$WORKSPACE_B passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
