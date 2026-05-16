#!/usr/bin/env bash
# bd-36bbk.1.1 - local Tailscale probe status e2e driver.
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
        echo "tailscale_local_probe: jq is required" >&2
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
    echo "tailscale_local_probe: set EE_BINARY or CARGO_TARGET_DIR to an existing ee binary" >&2
    echo "    this no-build harness will not run cargo" >&2
    exit 2
}

fail() {
    local phase="${1:?phase required}"
    local detail="${2:?detail required}"
    ft_emit_event "$phase" "false" "$detail" "" || true
    echo "tailscale_local_probe: $detail" >&2
    exit 1
}

run_status() {
    local label="${1:?label required}"
    local output_path="$WORK_DIR/${label}.json"
    if ! env \
        EE_MESH_ENABLED=1 \
        EE_TAILSCALE_BINARY_OVERRIDE="$TAILSCALE_SHIM" \
        EE_TAILSCALE_PROBE_TIMEOUT_MS=1500 \
        FAKE_TAILSCALE_VERSION_MODE="${FAKE_TAILSCALE_VERSION_MODE:-valid}" \
        "$EE_BINARY" status --workspace "$WORKSPACE" --json > "$output_path"
    then
        fail "$label" "ee status failed for $label"
    fi
    if [ ! -s "$output_path" ]; then
        fail "$label" "ee status produced empty JSON output for $label"
    fi
    if ! jq -e . "$output_path" >/dev/null; then
        fail "$label" "ee status produced malformed JSON output for $label"
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

assert_code() {
    local path="${1:?path required}"
    local code="${2:?code required}"
    local label="${3:?label required}"
    if ! jq -e --arg code "$code" '
        [
          .degraded[]?.code,
          .data.degraded[]?.code,
          .data.mesh.tailscale.degraded[]?.code
        ]
        | index($code) != null
    ' "$path" >/dev/null; then
        fail "$label" "JSON assertion failed for $label: missing degradation code $code"
    fi
}

require_jq
EE_BINARY="$(resolve_ee_binary)"
if [ ! -x "$EE_BINARY" ]; then
    echo "tailscale_local_probe: resolved EE_BINARY is not executable: $EE_BINARY" >&2
    exit 2
fi
version_output="$("$EE_BINARY" --version 2>&1)" || {
    echo "tailscale_local_probe: resolved EE_BINARY is not runnable: $EE_BINARY" >&2
    exit 2
}
if [ -z "$version_output" ]; then
    echo "tailscale_local_probe: resolved EE_BINARY returned empty --version output: $EE_BINARY" >&2
    exit 2
fi

TMP_ROOT="${TMPDIR:-/tmp}"
WORK_DIR="$(mktemp -d "${TMP_ROOT%/}/ee-tailscale-local-probe.XXXXXX")"
WORKSPACE="$WORK_DIR/workspace"
mkdir -p "$WORKSPACE"

export EE_TEST_EVENT_DIR="$WORK_DIR/events"
export FT_EVENT_KIND="tailscale_local_probe_e2e"
export FT_WORKSPACE_ID="tailscale-local-probe-workspace"
export FT_REQUEST_ID="tailscale-local-probe"
export FT_BEAD_ID="bd-36bbk.1.1"
export FT_SURFACE="tailscale_local_probe"

trap 'ft_teardown' EXIT

ft_init "$WORK_DIR/scenario" "tailscale_local_probe"
ft_set_self "nodekey:localprobe" "100.64.0.10" "tailnet-alpha" "ee-local" --platform=linux --authenticated=true
shim_dir="$(ft_shim_path)"
TAILSCALE_SHIM="$shim_dir/tailscale"

if ! "$EE_BINARY" init --workspace "$WORKSPACE" --json >/dev/null; then
    fail "setup" "ee init failed for local-probe workspace"
fi

healthy_json="$(run_status healthy)"
assert_jq "$healthy_json" '.success == true' "healthy_success"
assert_jq "$healthy_json" '.data.mesh.tailscale.schema == "ee.tailscale.local.v1"' "healthy_schema"
assert_jq "$healthy_json" '.data.mesh.tailscale.installed == true' "healthy_installed"
assert_jq "$healthy_json" '.data.mesh.tailscale.daemonReachable == true' "healthy_daemon"
assert_jq "$healthy_json" '.data.mesh.tailscale.authenticated == true' "healthy_authenticated"
assert_jq "$healthy_json" '.data.mesh.tailscale.binaryAuthentic == true' "healthy_binary"
assert_jq "$healthy_json" '.data.mesh.tailscale.shieldsUp == false' "healthy_shields"
assert_jq "$healthy_json" '.data.mesh.tailscale.tailnetId == "tailnet-alpha"' "healthy_tailnet"
assert_jq "$healthy_json" '.data.mesh.tailscale.selfTailscaleIp == "100.64.0.10"' "healthy_ip"
ft_emit_event "healthy" "true" "local probe reported healthy fake tailscale state" "$(_ft_hash "$(cat "$healthy_json")")"

ft_set_daemon_state not_authenticated
not_auth_json="$(run_status not_authenticated)"
assert_jq "$not_auth_json" '.success == true' "not_authenticated_success"
assert_jq "$not_auth_json" '.data.mesh.tailscale.daemonReachable == true' "not_authenticated_daemon"
assert_jq "$not_auth_json" '.data.mesh.tailscale.authenticated == false' "not_authenticated_flag"
assert_code "$not_auth_json" "tailscale_not_authenticated" "not_authenticated_code"
ft_emit_event "not_authenticated" "true" "local probe surfaced not-authenticated degradation" "$(_ft_hash "$(cat "$not_auth_json")")"

ft_set_daemon_state unreachable
daemon_json="$(run_status daemon_unreachable)"
assert_jq "$daemon_json" '.success == true' "daemon_unreachable_success"
assert_jq "$daemon_json" '.data.mesh.tailscale.daemonReachable == false' "daemon_unreachable_flag"
assert_code "$daemon_json" "tailscale_daemon_unreachable" "daemon_unreachable_code"
ft_emit_event "daemon_unreachable" "true" "local probe surfaced daemon-unreachable degradation" "$(_ft_hash "$(cat "$daemon_json")")"

ft_set_daemon_state running
ft_set_shields_up true
shields_json="$(run_status shields_up)"
assert_jq "$shields_json" '.success == true' "shields_up_success"
assert_jq "$shields_json" '.data.mesh.tailscale.shieldsUp == true' "shields_up_flag"
assert_code "$shields_json" "tailscale_shields_up" "shields_up_code"
ft_emit_event "shields_up" "true" "local probe surfaced shields-up degradation" "$(_ft_hash "$(cat "$shields_json")")"

ft_set_shields_up false
FAKE_TAILSCALE_VERSION_MODE=malformed
export FAKE_TAILSCALE_VERSION_MODE
binary_json="$(run_status binary_inauthentic)"
assert_jq "$binary_json" '.success == true' "binary_inauthentic_success"
assert_jq "$binary_json" '.data.mesh.tailscale.binaryAuthentic == false' "binary_inauthentic_flag"
assert_code "$binary_json" "tailscale_binary_inauthentic" "binary_inauthentic_code"
ft_emit_event "binary_inauthentic" "true" "local probe surfaced binary-authenticity degradation" "$(_ft_hash "$(cat "$binary_json")")"

ft_assert_no_invalid_events
ft_emit_event "summary" "true" "tailscale local probe e2e passed" "$(_ft_hash "$WORK_DIR")"
printf 'tailscale_local_probe workspace retained: %s\n' "$WORK_DIR" >&2
printf '%s\n' "$FT_EVENT_FILE"
