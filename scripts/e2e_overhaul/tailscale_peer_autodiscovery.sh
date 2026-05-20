#!/usr/bin/env bash
# bd-36bbk.1.2 - deterministic Tailscale peer autodiscovery e2e driver.
#
# This is a no-build harness. It requires an existing ee binary, uses the
# deterministic fake Tailscale CLI, and retains all workspace artifacts.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/fake_tailscale.sh
source "$SCRIPT_DIR/lib/fake_tailscale.sh"

require_jq() {
    if ! command -v jq >/dev/null 2>&1; then
        echo "tailscale_peer_autodiscovery: jq is required" >&2
        exit 2
    fi
}

resolve_ee_binary() {
    if [ -n "${EE_BINARY:-}" ]; then
        printf '%s\n' "$EE_BINARY"
        return 0
    fi
    if [ -n "${CARGO_TARGET_DIR:-}" ] && [ -x "${CARGO_TARGET_DIR%/}/debug/ee" ]; then
        printf '%s\n' "${CARGO_TARGET_DIR%/}/debug/ee"
        return 0
    fi
    if [ -n "${CARGO_TARGET_DIR:-}" ] && [ -x "${CARGO_TARGET_DIR%/}/release/ee" ]; then
        printf '%s\n' "${CARGO_TARGET_DIR%/}/release/ee"
        return 0
    fi
    if [ -x "$REPO_ROOT/target/debug/ee" ]; then
        printf '%s\n' "$REPO_ROOT/target/debug/ee"
        return 0
    fi
    echo "tailscale_peer_autodiscovery: set EE_BINARY or CARGO_TARGET_DIR to an existing ee binary" >&2
    echo "    this no-build harness will not run cargo" >&2
    exit 2
}

fail() {
    local phase="${1:?phase required}"
    local detail="${2:?detail required}"
    ft_emit_event "$phase" "false" "$detail" "" || true
    echo "tailscale_peer_autodiscovery: $detail" >&2
    exit 1
}

run_mesh_status() {
    local label="${1:?label required}"
    shift
    local output_path="$WORK_DIR/${label}.json"
    if ! env \
        EE_MESH_ENABLED=1 \
        EE_TAILSCALE_BINARY_OVERRIDE="$TAILSCALE_SHIM" \
        EE_TAILSCALE_PROBE_SOCKET_OVERRIDE="$WORK_DIR/no-such-tailscale.sock" \
        EE_TAILSCALE_PROBE_TIMEOUT_MS=1500 \
        "$@" \
        "$EE_BINARY" mesh status --workspace "$WORKSPACE" --json > "$output_path"
    then
        fail "$label" "ee mesh status failed for $label"
    fi
    if ! jq -e . "$output_path" >/dev/null; then
        fail "$label" "ee mesh status produced malformed JSON for $label"
    fi
    printf '%s\n' "$output_path"
}

assert_jq() {
    local path="${1:?path required}"
    local expr="${2:?jq expression required}"
    local label="${3:?label required}"
    if ! jq -e "$expr" "$path" >/dev/null; then
        fail "$label" "JSON assertion failed for $label: $expr"
    fi
}

assert_jq_arg() {
    local path="${1:?path required}"
    local arg_name="${2:?arg name required}"
    local arg_value="${3:?arg value required}"
    local expr="${4:?jq expression required}"
    local label="${5:?label required}"
    if ! jq -e --arg "$arg_name" "$arg_value" "$expr" "$path" >/dev/null; then
        fail "$label" "JSON assertion failed for $label: $expr"
    fi
}

assert_autodiscovery_code() {
    local path="${1:?path required}"
    local code="${2:?code required}"
    local label="${3:?label required}"
    if ! jq -e --arg code "$code" '
        .data.autoEnrollment.discovery.degraded
        | map(.code)
        | index($code) != null
    ' "$path" >/dev/null; then
        fail "$label" "missing autodiscovery degraded code $code"
    fi
}

require_jq
EE_BINARY="$(resolve_ee_binary)"
if [ ! -x "$EE_BINARY" ]; then
    echo "tailscale_peer_autodiscovery: resolved EE_BINARY is not executable: $EE_BINARY" >&2
    exit 2
fi

TMP_ROOT="${TMPDIR:-/tmp}"
WORK_DIR="$(mktemp -d "${TMP_ROOT%/}/ee-tailscale-autodiscovery.XXXXXX")"
WORKSPACE="$WORK_DIR/workspace"
mkdir -p "$WORKSPACE"

export EE_TEST_EVENT_DIR="$WORK_DIR/events"
export FT_EVENT_KIND="tailscale_peer_autodiscovery_e2e"
export FT_WORKSPACE_ID="tailscale-peer-autodiscovery-workspace"
export FT_REQUEST_ID="tailscale-peer-autodiscovery"
export FT_BEAD_ID="bd-36bbk.1.2"
export FT_SURFACE="tailscale_peer_autodiscovery"

trap 'ft_teardown' EXIT

ft_init "$WORK_DIR/scenario" "tailscale_peer_autodiscovery"
ft_set_self "nodekey:1111111111111111111111111111111111111111111111111111111111111111" \
    "100.64.0.10" "tailnet-alpha" "ee-local" --platform=linux --authenticated=true
shim_dir="$(ft_shim_path)"
TAILSCALE_SHIM="$shim_dir/tailscale"

if ! "$EE_BINARY" init --workspace "$WORKSPACE" --json >/dev/null; then
    fail "setup" "ee init failed for autodiscovery workspace"
fi

empty_json="$(run_mesh_status empty)"
WORKSPACE_ID="$(jq -r '.data.workspaceId' "$empty_json")"
if [ -z "$WORKSPACE_ID" ] || [ "$WORKSPACE_ID" = "null" ]; then
    fail "setup" "could not resolve mesh workspaceId"
fi

peer_a="nodekey:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
peer_b="nodekey:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
peer_c="nodekey:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"

ft_add_peer "$peer_a" "100.64.0.20" "peer-a" \
    --tag=tag:ee-mesh --ee_version=0.2.0 --ee_protocol=1.0 \
    --workspace_ids="$WORKSPACE_ID" --latency_ms=17
ft_add_peer "$peer_b" "100.64.0.21" "peer-b"
ft_add_peer "$peer_c" "100.64.0.22" "peer-c" \
    --tag=tag:ee-mesh --ee_version=0.2.0 --ee_protocol=1.0 \
    --workspace_ids=workspace-other --latency_ms=5

service_tag_json="$(run_mesh_status service_tag)"
assert_jq "$service_tag_json" '.success == true' "service_tag_success"
assert_jq "$service_tag_json" '.data.autoEnrollment.discovery.schema == "ee.tailscale.autodiscovery.v1"' "service_tag_schema"
assert_jq_arg "$service_tag_json" peer "$peer_a" '.data.autoEnrollment.discovery.eeCapablePeers | map(.nodeKey) == [$peer]' "service_tag_eligible"
assert_jq_arg "$service_tag_json" peer "$peer_b" '.data.autoEnrollment.discovery.skippedPeers[] | select(.nodeKey == $peer and .reason == "no_discovery_consent")' "service_tag_peer_b"
assert_jq_arg "$service_tag_json" peer "$peer_c" '.data.autoEnrollment.discovery.skippedPeers[] | select(.nodeKey == $peer and .reason == "workspace_mismatch")' "service_tag_peer_c"
assert_autodiscovery_code "$service_tag_json" "peer_discovery_workspace_mismatch" "service_tag_workspace_mismatch"
ft_emit_event "service_tag" "true" "service-tag autodiscovery admitted only the matching ee peer" "$(_ft_hash "$(cat "$service_tag_json")")"

auto_admit_json="$(run_mesh_status auto_admit EE_TAILSCALE_DISCOVERY_MODE=auto_admit)"
assert_jq_arg "$auto_admit_json" peer "$peer_b" '.data.autoEnrollment.discovery.skippedPeers[] | select(.nodeKey == $peer and .reason == "non_ee")' "auto_admit_peer_b"
ft_emit_event "auto_admit" "true" "auto-admit probed non-ee peer and classified it as non_ee" "$(_ft_hash "$(cat "$auto_admit_json")")"

mkdir -p "$WORKSPACE/.ee"
printf 'node_keys = ["%s"]\n' "$peer_c" > "$WORKSPACE/.ee/discovery_allowlist.toml"
allowlist_json="$(run_mesh_status allowlist EE_TAILSCALE_DISCOVERY_MODE=allowlist)"
assert_jq_arg "$allowlist_json" peer "$peer_c" '.data.autoEnrollment.discovery.skippedPeers[] | select(.nodeKey == $peer and .reason == "workspace_mismatch")' "allowlist_peer_c"
ft_emit_event "allowlist" "true" "allowlist mode probed only the configured node key" "$(_ft_hash "$(cat "$allowlist_json")")"

for i in $(seq 1 50); do
    hex="$(printf '%064x' "$i")"
    ft_add_peer "nodekey:$hex" "100.64.1.$i" "budget-$i" \
        --tag=tag:ee-mesh --ee_version=0.2.0 --ee_protocol=1.0 \
        --workspace_ids="$WORKSPACE_ID" --latency_ms=750
done
budget_json="$(run_mesh_status budget EE_TAILSCALE_DISCOVERY_MODE=service_tag EE_TAILSCALE_DISCOVERY_BUDGET_MS=500)"
assert_autodiscovery_code "$budget_json" "tailscale_peer_probe_timeout" "budget_timeout"
assert_autodiscovery_code "$budget_json" "peer_discovery_budget_exhausted" "budget_exhausted"
ft_emit_event "budget" "true" "autodiscovery budget exhaustion surfaced degraded codes" "$(_ft_hash "$(cat "$budget_json")")"

ft_assert_no_invalid_events
ft_emit_event "summary" "true" "tailscale peer autodiscovery e2e passed" "$(_ft_hash "$WORK_DIR")"
printf 'tailscale_peer_autodiscovery workspace retained: %s\n' "$WORK_DIR" >&2
printf '%s\n' "$FT_EVENT_FILE"
