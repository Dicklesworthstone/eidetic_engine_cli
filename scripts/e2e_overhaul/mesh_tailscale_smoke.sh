#!/usr/bin/env bash
# bd-1crtj - opt-in real Tailscale transport smoke for SRR6.
#
# This script is intentionally outside normal CI. Without
# EE_E2E_REAL_TAILSCALE=1 it exits 78 after writing an ee.test_event.v1 skip
# event. With opt-in enabled it verifies a real tailscaled, checks that the
# requested peer is visible in `tailscale status --json`, then exercises the
# local ee mesh status/sync/export/import surfaces against a retained workspace.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
EXIT_SKIP=78
SCENARIO="mesh_tailscale_smoke"
BEAD_ID="bd-1crtj"

EVENT_DIR="${EE_TEST_EVENT_DIR:-${TMPDIR:-/tmp}/ee-${SCENARIO}.$$}"
mkdir -p "$EVENT_DIR"
EVENT_FILE="$EVENT_DIR/events.jsonl"
ARTIFACT_DIR="${EE_E2E_ARTIFACT_DIR:-$EVENT_DIR/artifacts}"
mkdir -p "$ARTIFACT_DIR"

json_hash() {
    printf '%s' "${1:-}" | shasum -a 256 | awk '{print substr($1, 1, 16)}'
}

emit_event() {
    local phase="${1:?phase required}"
    local status="${2:?status required}"
    local message="${3:?message required}"
    local detail_json="${4:-}"
    if [ -z "$detail_json" ]; then
        detail_json="{}"
    fi
    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg scenario "$SCENARIO" \
        --arg bead "$BEAD_ID" \
        --arg phase "$phase" \
        --arg status "$status" \
        --arg message "$message" \
        --argjson details "$detail_json" \
        '{
          schema: $schema,
          kind: "mesh_tailscale_smoke",
          bead: $bead,
          phase: $phase,
          status: $status,
          message: $message,
          fields: ({scenario: $scenario} + $details)
        }' >>"$EVENT_FILE"
}

skip() {
    local reason="${1:?skip reason required}"
    emit_event "precondition" "skipped" "$reason" '{}'
    printf 'mesh_tailscale_smoke skipped: %s\n' "$reason" >&2
    printf '%s\n' "$EVENT_FILE"
    exit "$EXIT_SKIP"
}

fail() {
    local phase="${1:?phase required}"
    local reason="${2:?failure reason required}"
    emit_event "$phase" "failed" "$reason" '{}'
    printf 'mesh_tailscale_smoke failed: %s\n' "$reason" >&2
    printf 'event log: %s\n' "$EVENT_FILE" >&2
    exit 1
}

require_tool() {
    local tool="${1:?tool required}"
    if ! command -v "$tool" >/dev/null 2>&1; then
        skip "$tool is required for the opt-in real Tailscale smoke"
    fi
}

resolve_ee_binary() {
    if [ -n "${EE_BINARY:-}" ]; then
        printf '%s\n' "$EE_BINARY"
        return 0
    fi
    # shellcheck source=scripts/lib/ee_binary_resolution.sh
    source "$REPO_ROOT/scripts/lib/ee_binary_resolution.sh"
    ee_resolve_binary release
}

assert_json_file() {
    local path="${1:?path required}"
    local label="${2:?label required}"
    if [ ! -s "$path" ]; then
        fail "$label" "$path is empty"
    fi
    if ! jq -e . "$path" >/dev/null; then
        fail "$label" "$path is not valid JSON"
    fi
}

assert_jq() {
    local path="${1:?path required}"
    local filter="${2:?jq filter required}"
    local label="${3:?label required}"
    if ! jq -e "$filter" "$path" >/dev/null; then
        fail "$label" "JSON assertion failed for $label: $filter"
    fi
}

if [ "${EE_E2E_REAL_TAILSCALE:-0}" != "1" ]; then
    skip "set EE_E2E_REAL_TAILSCALE=1 to run against a real tailnet"
fi

require_tool jq
require_tool shasum
require_tool tailscale

PEER_SELECTOR="${EE_REAL_TAILSCALE_PEER:-}"
if [ -z "$PEER_SELECTOR" ]; then
    skip "set EE_REAL_TAILSCALE_PEER to a peer node key, MagicDNS name, host name, or Tailscale IP"
fi

EE_BINARY="$(resolve_ee_binary)"
if [ ! -x "$EE_BINARY" ]; then
    skip "set EE_BINARY to an executable ee binary; this harness never runs cargo"
fi

TAILSCALE_STATUS="$ARTIFACT_DIR/tailscale_status.json"
if ! tailscale status --json >"$TAILSCALE_STATUS"; then
    skip "tailscale status --json failed; authenticate tailscaled before running this smoke"
fi
assert_json_file "$TAILSCALE_STATUS" "tailscale_status_json"

if ! jq -e '(.Self? // null) != null and (((.BackendState? // "") == "Running") or (.Self.Online? == true))' "$TAILSCALE_STATUS" >/dev/null; then
    skip "tailscale status did not include an authenticated local Self node"
fi

PEER_JSON="$ARTIFACT_DIR/peer.json"
if ! jq --arg peer "$PEER_SELECTOR" '
    (.Peer // {})
    | to_entries
    | map(select(
        .key == $peer
        or (.value.ID? == $peer)
        or (.value.HostName? == $peer)
        or ((.value.DNSName? // "" | rtrimstr(".")) == ($peer | rtrimstr(".")))
        or ((.value.TailscaleIPs? // []) | index($peer))
      ))
    | first // empty
  ' "$TAILSCALE_STATUS" >"$PEER_JSON"; then
    skip "failed to inspect peer list from tailscale status"
fi
if [ ! -s "$PEER_JSON" ] || ! jq -e '.value' "$PEER_JSON" >/dev/null; then
    skip "requested peer was not visible in tailscale status --json"
fi

PEER_HASH="$(json_hash "$PEER_SELECTOR")"
ROUTE_HINT="$(jq -r '.value.Relay? // .value.CurAddr? // "unknown"' "$PEER_JSON")"
emit_event "precondition" "passed" "real tailnet peer is visible" \
    "$(jq -cn --arg peerHash "$PEER_HASH" --arg routeHash "$(json_hash "$ROUTE_HINT")" '{peerNodeHash: $peerHash, routeHash: $routeHash}')"

WORK_ROOT="${EE_E2E_TMPDIR:-${TMPDIR:-/tmp}}"
WORKSPACE="$(mktemp -d "${WORK_ROOT%/}/ee-${SCENARIO}.XXXXXX")"
MANIFEST="$WORKSPACE/e2e_retention_manifest.json"
cat >"$MANIFEST" <<JSON
{
  "schema": "ee.e2e.retention_manifest.v1",
  "epic_name": "$SCENARIO",
  "workspace": "$WORKSPACE",
  "event_log": "$EVENT_FILE",
  "artifact_dir": "$ARTIFACT_DIR",
  "cleanup_policy": "retained_by_real_tailscale_smoke"
}
JSON

if ! "$EE_BINARY" init --workspace "$WORKSPACE" --json >"$ARTIFACT_DIR/init.json"; then
    fail "setup" "ee init failed"
fi
assert_json_file "$ARTIFACT_DIR/init.json" "init_json"
emit_event "setup" "passed" "workspace initialized" \
    "$(jq -cn --arg workspaceHash "$(json_hash "$WORKSPACE")" '{workspaceHash: $workspaceHash}')"

run_ee_json() {
    local label="${1:?label required}"
    shift
    local output="$ARTIFACT_DIR/${label}.json"
    local start_ms end_ms elapsed_ms
    start_ms="$(($(date +%s) * 1000))"
    if ! env EE_MESH_ENABLED=1 "$EE_BINARY" "$@" --workspace "$WORKSPACE" --json >"$output"; then
        fail "$label" "ee command failed for $label"
    fi
    end_ms="$(($(date +%s) * 1000))"
    elapsed_ms=$((end_ms - start_ms))
    assert_json_file "$output" "$label"
    emit_event "action" "passed" "$label completed" \
        "$(jq -cn --arg label "$label" --argjson elapsedMs "$elapsed_ms" '{commandLabel: $label, elapsedMs: $elapsedMs}')"
    printf '%s\n' "$output"
}

STATUS_JSON="$(run_ee_json mesh_status mesh status)"
assert_jq "$STATUS_JSON" '.success == true or .schema == "ee.mesh.status.v1"' "mesh_status_success"

SYNC_JSON="$(run_ee_json mesh_sync mesh sync --once --peer-concurrency 1 --body-fetch-budget-bytes 65536 --time-budget-ms "${EE_REAL_TAILSCALE_SYNC_BUDGET_MS:-5000}")"
assert_jq "$SYNC_JSON" '.success == true or .schema == "ee.mesh.sync.v1"' "mesh_sync_success"

EXPORT_PATH="$ARTIFACT_DIR/mesh-export.json"
EXPORT_JSON="$(run_ee_json mesh_export mesh export --out "$EXPORT_PATH")"
assert_jq "$EXPORT_JSON" '.success == true or .schema == "ee.mesh.export.v1"' "mesh_export_success"
assert_json_file "$EXPORT_PATH" "mesh_export_artifact"

IMPORT_JSON="$(run_ee_json mesh_import_dry_run mesh import --file "$EXPORT_PATH" --dry-run)"
assert_jq "$IMPORT_JSON" '.success == true or .schema == "ee.mesh.import.v1"' "mesh_import_success"

SYNC_MS="$(jq -r 'select(.fields.commandLabel? == "mesh_sync") | .fields.elapsedMs // empty' "$EVENT_FILE" | tail -1)"
emit_event "assert" "passed" "real Tailscale smoke surfaces completed" \
    "$(jq -cn \
        --arg peerNodeHash "$PEER_HASH" \
        --arg routeHash "$(json_hash "$ROUTE_HINT")" \
        --arg syncMs "${SYNC_MS:-0}" \
        --arg bodyFetchMs "0" \
        --arg deniedPolicyCode "not_exercised_by_local_sync_surface" \
        --arg revisionTokenPresent "false" \
        '{
          peerNodeHash: $peerNodeHash,
          routeHash: $routeHash,
          syncMs: ($syncMs | tonumber),
          bodyFetchMs: ($bodyFetchMs | tonumber),
          deniedPolicyCode: $deniedPolicyCode,
          revisionTokenPresent: ($revisionTokenPresent == "true")
        }')"

emit_event "cleanup" "retained" "real Tailscale smoke artifacts retained" \
    "$(jq -cn --arg manifestHash "$(json_hash "$(cat "$MANIFEST")")" '{manifestHash: $manifestHash}')"
printf 'mesh_tailscale_smoke artifacts retained: %s\n' "$ARTIFACT_DIR" >&2
printf '%s\n' "$EVENT_FILE"
