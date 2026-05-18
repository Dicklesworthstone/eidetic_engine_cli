#!/usr/bin/env bash
# bd-36bbk.1.7.1 - peer discovery-policy CLI e2e driver.
#
# This harness is intentionally safe to land before the CLI surface is wired:
# when `ee mesh discovery-policy` is unavailable it records each required
# scenario as a structured todo_assert instead of failing the whole script.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
START_SECONDS="$(python3 -c 'import time; print(time.monotonic())')"
epic_setup "peer_discovery_policy"
mesh_scenario_setup "peer_discovery_policy" 2

NODE_ALPHA="nodekey:00000000000000000000000000000000000000000000000000000000000000aa"
NODE_BRAVO="nodekey:00000000000000000000000000000000000000000000000000000000000000bb"
NODE_CHARLIE="nodekey:00000000000000000000000000000000000000000000000000000000000000cc"

policy_todo() {
    local label="${1:?label required}"
    local description="${2:?description required}"
    todo_assert "peer_discovery_policy_${label}" "bd-36bbk.1.7.1" "$description"
}

policy_command_available() {
    ee_global mesh discovery-policy --help >/dev/null 2>&1
}

run_policy_json() {
    local label="${1:?label required}"
    shift
    e2e_log_note "peer_discovery_policy_scenario=$label"
    e2e_log_command "$EE_BINARY" mesh discovery-policy "$@" --workspace "$EPIC_WORKSPACE" --json
}

run_policy_json_with_env() {
    local label="${1:?label required}"
    local env_pair="${2:?env pair required}"
    shift 2
    e2e_log_note "peer_discovery_policy_scenario=$label env=$env_pair"
    e2e_log_command env "$env_pair" "$EE_BINARY" mesh discovery-policy "$@" --workspace "$EPIC_WORKSPACE" --json
}

write_node_list() {
    local path="${1:?path required}"
    shift
    mkdir -p "$(dirname "$path")"
    python3 - "$path" "$@" <<'PY'
import sys

path = sys.argv[1]
keys = sorted(dict.fromkeys(sys.argv[2:]))
with open(path, "w", encoding="utf-8") as handle:
    handle.write("node_keys = [\n")
    for key in keys:
        handle.write(f'  "{key}",\n')
    handle.write("]\n")
PY
}

assert_json_success() {
    local json="${1:-}"
    local label="${2:?label required}"
    assert_jq "$json" '.success // false' "true" "$label"
}

assert_degraded_code() {
    local json="${1:-}"
    local code="${2:?code required}"
    local label="${3:?label required}"
    local got
    got="$(printf '%s' "$json" | jq -r --arg code "$code" '
        [
          .degraded[]?.code,
          .data.degraded[]?.code,
          .data.policy.degraded[]?.code,
          .data.degraded[]?.code
        ]
        | map(select(. == $code))
        | length
    ' 2>/dev/null || echo 0)"
    e2e_log_assert_num "$got" -ge 1 "$label"
}

assert_node_list_contains() {
    local json="${1:-}"
    local filter="${2:?filter required}"
    local node="${3:?node required}"
    local label="${4:?label required}"
    local got
    got="$(printf '%s' "$json" | jq -r --arg node "$node" "$filter" 2>/dev/null || true)"
    e2e_log_assert_eq "$got" "true" "$label"
}

if ! policy_command_available; then
    policy_todo "service_tag_default" "ee mesh discovery-policy no-arg JSON surface is not available yet."
    policy_todo "responder_decline" "respondMode=service_tag decline/degradation scenario awaits CLI wiring."
    policy_todo "auto_admit" "discoveryMode=auto_admit env/config scenario awaits CLI wiring."
    policy_todo "allowlist" "allowlist mode and discovery_allowlist.toml scenario awaits CLI wiring."
    policy_todo "denylist_override" "denylist precedence scenario awaits CLI wiring."
    policy_todo "set_allow_deny_mutations" "set/allow/deny mutation subcommands await CLI wiring."
    policy_todo "explain_decision_tree" "--explain effectiveDecisionPreview scenario awaits CLI wiring."
else
    DEFAULT_JSON="$(run_policy_json "service_tag_default")"
    assert_json_success "$DEFAULT_JSON" "peer_discovery_policy_default_success"
    assert_jq "$DEFAULT_JSON" '.data.schema // empty' "ee.mesh.discovery_policy.v1" "peer_discovery_policy_default_schema"
    assert_jq "$DEFAULT_JSON" '.data.discoveryMode // empty' "service_tag" "peer_discovery_policy_default_discovery_mode"
    assert_jq "$DEFAULT_JSON" '.data.respondMode // empty' "service_tag" "peer_discovery_policy_default_respond_mode"

    RESPONDER_JSON="$(run_policy_json_with_env "responder_decline" "EE_TAILSCALE_RESPOND_MODE=service_tag")"
    assert_json_success "$RESPONDER_JSON" "peer_discovery_policy_responder_decline_success"
    assert_degraded_code "$RESPONDER_JSON" "discovery_policy_no_ee_mesh_tag" "peer_discovery_policy_responder_decline_degraded"

    AUTO_ADMIT_JSON="$(run_policy_json_with_env "auto_admit" "EE_TAILSCALE_DISCOVERY_MODE=auto_admit")"
    assert_json_success "$AUTO_ADMIT_JSON" "peer_discovery_policy_auto_admit_success"
    assert_jq "$AUTO_ADMIT_JSON" '.data.discoveryMode // empty' "auto_admit" "peer_discovery_policy_auto_admit_mode"

    write_node_list "$EPIC_WORKSPACE/.ee/discovery_allowlist.toml" "$NODE_ALPHA" "$NODE_BRAVO"
    ALLOWLIST_JSON="$(run_policy_json_with_env "allowlist" "EE_TAILSCALE_DISCOVERY_MODE=allowlist")"
    assert_json_success "$ALLOWLIST_JSON" "peer_discovery_policy_allowlist_success"
    assert_jq "$ALLOWLIST_JSON" '.data.discoveryMode // empty' "allowlist" "peer_discovery_policy_allowlist_mode"
    # shellcheck disable=SC2016 # $node is a jq variable, not a shell variable.
    assert_node_list_contains "$ALLOWLIST_JSON" '(.data.allowlistedNodeKeys // []) | index($node) != null' "$NODE_ALPHA" "peer_discovery_policy_allowlist_contains_alpha"

    write_node_list "$EPIC_WORKSPACE/.ee/discovery_denylist.toml" "$NODE_ALPHA"
    DENYLIST_JSON="$(run_policy_json_with_env "denylist_override" "EE_TAILSCALE_DISCOVERY_MODE=auto_admit" --explain)"
    assert_json_success "$DENYLIST_JSON" "peer_discovery_policy_denylist_success"
    # shellcheck disable=SC2016 # $node is a jq variable, not a shell variable.
    assert_node_list_contains "$DENYLIST_JSON" '(.data.deniedNodeKeys // []) | index($node) != null' "$NODE_ALPHA" "peer_discovery_policy_denylist_contains_alpha"

    SET_JSON="$(run_policy_json "set_modes" set --discovery-mode allowlist --respond-mode allowlist)"
    assert_json_success "$SET_JSON" "peer_discovery_policy_set_success"
    ALLOW_JSON="$(run_policy_json "allow_mutation" allow "$NODE_CHARLIE")"
    assert_json_success "$ALLOW_JSON" "peer_discovery_policy_allow_mutation_success"
    DENY_JSON="$(run_policy_json "deny_mutation" deny "$NODE_BRAVO")"
    assert_json_success "$DENY_JSON" "peer_discovery_policy_deny_mutation_success"

    EXPLAIN_JSON="$(run_policy_json "explain_decision_tree" --explain)"
    assert_json_success "$EXPLAIN_JSON" "peer_discovery_policy_explain_success"
    assert_jq "$EXPLAIN_JSON" '(.data.effectiveDecisionPreview // []) | type' "array" "peer_discovery_policy_explain_preview_array"
fi

ELAPSED_MS="$(python3 -c "import time; print(int((time.monotonic() - float('$START_SECONDS')) * 1000))")"
e2e_log_note "peer_discovery_policy_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} elapsed_ms=${ELAPSED_MS}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
