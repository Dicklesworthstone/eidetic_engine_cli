#!/usr/bin/env bash
# bd-36bbk.1.11 — opt-in real Tailscale auto-enrollment smoke for SRR6.46.
#
# This script is intentionally outside normal CI. Without
# EE_E2E_REAL_TAILSCALE=1 it exits 78 after writing an ee.test_event.v1 skip
# event. With opt-in enabled it verifies a real authenticated tailscaled, then
# exercises the auto-enrollment chain through `ee mesh auto-enroll` against
# the real tailnet in dry-run mode (no durable writes by default), and ensures
# the trap-driven cleanup runs `ee mesh disable` regardless of intermediate
# failure. Parallel to bd-1crtj's mesh_tailscale_smoke.sh (transport surface).
#
# Structure mirrors mesh_tailscale_smoke.sh so operators reading one already
# understand the other; the only divergences are the scenario name, bead id,
# and the auto-enrollment-specific assertions on the `ee mesh auto-enroll`
# output schema.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
EXIT_SKIP=78
EXIT_CLEANUP_FAILURE=79
SCENARIO="auto_enroll_real_tailscale"
BEAD_ID="bd-36bbk.1.11"

EVENT_DIR="${EE_TEST_EVENT_DIR:-${TMPDIR:-/tmp}/ee-${SCENARIO}.$$}"
mkdir -p "$EVENT_DIR"
EVENT_FILE="$EVENT_DIR/events.jsonl"
ARTIFACT_DIR="${EE_E2E_ARTIFACT_DIR:-$EVENT_DIR/artifacts}"
mkdir -p "$ARTIFACT_DIR"

# Track workspace + cleanup state so the bash EXIT trap can disable mesh
# even when an intermediate scenario fails partway through. WORKSPACE stays
# empty until setup runs so an early precondition skip does not try to call
# `ee mesh disable` on a non-existent workspace.
WORKSPACE=""
CLEANUP_RAN=0
EE_BINARY=""

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
          kind: "auto_enroll_real_tailscale",
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
    printf '%s skipped: %s\n' "$SCENARIO" "$reason" >&2
    printf '%s\n' "$EVENT_FILE"
    exit "$EXIT_SKIP"
}

fail() {
    local phase="${1:?phase required}"
    local reason="${2:?failure reason required}"
    emit_event "$phase" "failed" "$reason" '{}'
    printf '%s failed: %s\n' "$SCENARIO" "$reason" >&2
    printf 'event log: %s\n' "$EVENT_FILE" >&2
    exit 1
}

require_tool() {
    local tool="${1:?tool required}"
    if ! command -v "$tool" >/dev/null 2>&1; then
        skip "$tool is required for the opt-in real Tailscale auto-enroll smoke"
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

# Always-run cleanup. Per the bead's "Always runs cleanup (R9 disable) even
# if intermediate scenarios fail (uses bash `trap`)" requirement: this is the
# trap target. Runs `ee mesh disable --dry-run --reason=...` on the test
# workspace so a half-finished auto-enroll never leaves a real workspace in a
# weird mesh state. Dry-run is used by default because the auto-enroll
# scenarios above also dry-run; a future positive-write extension can set
# EE_E2E_AUTO_ENROLL_PERSIST=1 and pair it with non-dry cleanup.
cleanup() {
    local exit_code=$?
    if [ "$CLEANUP_RAN" -eq 1 ]; then
        return
    fi
    CLEANUP_RAN=1
    if [ -n "$WORKSPACE" ] && [ -n "$EE_BINARY" ] && [ -x "$EE_BINARY" ]; then
        local disable_out="$ARTIFACT_DIR/mesh_disable_cleanup.json"
        if "$EE_BINARY" mesh disable \
                --workspace "$WORKSPACE" \
                --dry-run \
                --reason "auto_enroll_real_tailscale cleanup trap" \
                --json >"$disable_out" 2>/dev/null; then
            emit_event "cleanup" "passed" "mesh disable cleanup completed" \
                "$(jq -cn --arg outHash "$(json_hash "$(cat "$disable_out")")" '{disableOutputHash: $outHash}')"
        else
            emit_event "cleanup" "failed" "mesh disable cleanup failed" '{}'
            # Only surface the cleanup-failure exit code if the main path
            # already succeeded (exit 0). A primary failure takes priority.
            if [ "$exit_code" -eq 0 ]; then
                exit_code="$EXIT_CLEANUP_FAILURE"
            fi
        fi
    fi
    exit "$exit_code"
}
trap cleanup EXIT

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
TAILNET_ID="$(jq -r '.CurrentTailnet.Name? // .CurrentTailnet.MagicDNSSuffix? // "unknown"' "$TAILSCALE_STATUS")"
TAILNET_HASH="$(json_hash "$TAILNET_ID")"
emit_event "precondition" "passed" "real tailnet peer is visible for auto-enroll" \
    "$(jq -cn \
        --arg peerHash "$PEER_HASH" \
        --arg routeHash "$(json_hash "$ROUTE_HINT")" \
        --arg tailnetHash "$TAILNET_HASH" \
        '{peerNodeHash: $peerHash, routeHash: $routeHash, tailnetHash: $tailnetHash}')"

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
  "cleanup_policy": "retained_by_auto_enroll_real_tailscale_smoke"
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

# Scenario A — `ee mesh status --json` against the live tailnet (no
# enrollment yet). Establishes the baseline mesh posture so a follow-up
# auto-enroll has a before/after pair.
STATUS_PRE_JSON="$(run_ee_json mesh_status_pre mesh status)"
assert_jq "$STATUS_PRE_JSON" '.success == true or (.schema // "" | startswith("ee.mesh.status"))' "mesh_status_pre_success"

# Scenario B — `ee mesh auto-enroll --dry-run --explain --json` runs the full
# discovery + plan + audit pipeline against the real tailnet without writing
# durable peer rows. The dry-run is the default opt-in shape; a future
# positive-write run is gated separately so the smoke is safe to run on a
# personal tailnet without accidental durable enrollment.
AUTO_ENROLL_JSON="$(run_ee_json mesh_auto_enroll_dry_run mesh auto-enroll --dry-run --explain)"
assert_jq "$AUTO_ENROLL_JSON" '.success == true or (.schema // "" | startswith("ee.mesh.auto_enroll"))' "mesh_auto_enroll_success"

# Scenario C — `ee mesh status --json` after the dry-run. Asserts the
# post-status surface is still reachable; nothing durable should have changed
# because Scenario B was a dry-run, but the schema must still parse.
STATUS_POST_JSON="$(run_ee_json mesh_status_post mesh status)"
assert_jq "$STATUS_POST_JSON" '.success == true or (.schema // "" | startswith("ee.mesh.status"))' "mesh_status_post_success"

AUTO_ENROLL_MS="$(jq -r 'select(.fields.commandLabel? == "mesh_auto_enroll_dry_run") | .fields.elapsedMs // empty' "$EVENT_FILE" | tail -1)"
emit_event "assert" "passed" "real Tailscale auto-enroll dry-run smoke surfaces completed" \
    "$(jq -cn \
        --arg peerNodeHash "$PEER_HASH" \
        --arg routeHash "$(json_hash "$ROUTE_HINT")" \
        --arg tailnetHash "$TAILNET_HASH" \
        --arg autoEnrollMs "${AUTO_ENROLL_MS:-0}" \
        '{
          peerNodeHash: $peerNodeHash,
          routeHash: $routeHash,
          tailnetHash: $tailnetHash,
          autoEnrollMs: ($autoEnrollMs | tonumber),
          deniedPolicyCode: "not_exercised_by_dry_run",
          revisionTokenPresent: false
        }')"

emit_event "cleanup" "retained" "real Tailscale auto-enroll smoke artifacts retained" \
    "$(jq -cn --arg manifestHash "$(json_hash "$(cat "$MANIFEST")")" '{manifestHash: $manifestHash}')"
printf '%s artifacts retained: %s\n' "$SCENARIO" "$ARTIFACT_DIR" >&2
printf '%s\n' "$EVENT_FILE"
