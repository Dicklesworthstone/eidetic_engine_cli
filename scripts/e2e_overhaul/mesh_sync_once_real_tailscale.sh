#!/usr/bin/env bash
# bd-36bbk.2 — opt-in real Tailscale mesh-sync-once smoke for SRR6.7/SRR6.9.
#
# This script is intentionally outside normal CI. Without
# EE_E2E_REAL_TAILSCALE=1 it exits 78 after writing an ee.test_event.v1 skip
# event. With opt-in enabled it verifies a real authenticated tailscaled,
# then exercises `ee mesh sync --once --json` against the real tailnet so
# operators can confirm whether the foreground sync supervisor still emits
# `mesh_sync_once_network_deferred` for every tick — the regression the
# bd-36bbk.2 bug bead tracks — or whether the SRR6.7 anti-entropy + SRR6.9
# tailscale transport primitives now actually contact a peer.
#
# Parallel to bd-1crtj's mesh_tailscale_smoke.sh (transport surface) and
# bd-36bbk.1.11's auto_enroll_real_tailscale.sh (auto-enroll surface).
# Structure mirrors both so operators reading one already understand the
# other; the only divergences are the scenario name, bead id, and the
# sync-once-specific assertions on the `ee mesh sync --once` output.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
EXIT_SKIP=78
EXIT_CLEANUP_FAILURE=79
SCENARIO="mesh_sync_once_real_tailscale"
BEAD_ID="bd-36bbk.2"

# Predictable PID-suffixed /tmp paths are a symlink-attack surface and,
# with the default umask, leave tailnet-topology artifacts world-readable
# on a shared host. Mint the event dir with `mktemp -d` (unguessable
# name, mode 0700) when the caller did not pin one, and `chmod 0700`
# defensively in both branches. bd-25lyv.
if [ -n "${EE_TEST_EVENT_DIR:-}" ]; then
    EVENT_DIR="$EE_TEST_EVENT_DIR"
    mkdir -p "$EVENT_DIR"
else
    EVENT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/ee-${SCENARIO}.XXXXXX")"
fi
chmod 0700 "$EVENT_DIR"
EVENT_FILE="$EVENT_DIR/events.jsonl"
if [ -n "${EE_E2E_ARTIFACT_DIR:-}" ]; then
    ARTIFACT_DIR="$EE_E2E_ARTIFACT_DIR"
    ARTIFACT_DIR_SOURCE="explicit"
else
    ARTIFACT_DIR="$EVENT_DIR/artifacts"
    ARTIFACT_DIR_SOURCE="fallback"
fi
mkdir -p "$ARTIFACT_DIR"
chmod 0700 "$ARTIFACT_DIR"

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
          kind: "mesh_sync_once_real_tailscale",
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
        skip "$tool is required for the opt-in real Tailscale sync-once smoke"
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

assert_json_text() {
    local text="${1:-}"
    local label="${2:?label required}"
    if [ -z "$text" ]; then
        fail "$label" "JSON payload is empty"
    fi
    if ! printf '%s\n' "$text" | jq -e . >/dev/null; then
        fail "$label" "JSON payload is not valid JSON"
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

mesh_contacted_peer_count() {
    local path="${1:?path required}"
    jq -r '
        def contacted_peer_count:
          if type == "boolean" then
            if . then 1 else 0 end
          elif type == "number" then
            .
          elif type == "string" then
            if test("^(true|yes)$"; "i") then 1
            elif test("^(false|no)$"; "i") then 0
            else (tonumber? // 0)
            end
          else
            0
          end;
        [
          .data.contactedPeers? // empty,
          .data.contacted_peers? // empty,
          .data.peers?.contacted? // empty,
          .data.summary?.contactedPeers? // empty,
          .data.summary?.contacted_peers? // empty
        ]
        | map(select(. != null))
        | first // 0
        | contacted_peer_count
      ' "$path"
}

require_tailnet_artifact_dir_opt_in() {
    if [ "$ARTIFACT_DIR_SOURCE" = "fallback" ] && [ "${EE_TAILNET_TMP_OK:-0}" != "1" ]; then
        skip "set EE_E2E_ARTIFACT_DIR to a private directory for retained tailnet artifacts, or set EE_TAILNET_TMP_OK=1 to allow the temporary fallback"
    fi
}

select_peer_entry() {
    local status_json="${1:-}"
    local peer_selector="${2:?peer selector required}"
    printf '%s\n' "$status_json" | jq -c --arg peer "$peer_selector" '
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
      '
}

write_redacted_tailnet_artifacts() {
    local status_json="${1:-}"
    local selected_peer="${2:?selected peer JSON required}"
    local peer_hash="${3:?peer hash required}"
    local route_hash="${4:?route hash required}"
    local tailnet_hash="${5:?tailnet hash required}"
    local selected_peer_hash="${6:?selected peer hash required}"
    local status_path="${7:?status output path required}"
    local peer_path="${8:?peer output path required}"

    # Persist replay-friendly facts without retaining raw node keys, hostnames,
    # Tailscale IPs, or the full peer table from `tailscale status --json`.
    if ! printf '%s\n' "$status_json" | jq \
            --argjson selectedPeer "$selected_peer" \
            --arg peerHash "$peer_hash" \
            --arg routeHash "$route_hash" \
            --arg tailnetHash "$tailnet_hash" \
            --arg selectedPeerHash "$selected_peer_hash" \
            '{
              schema: "ee.tailscale.retained_status.v1",
              redacted: true,
              tailnetHash: $tailnetHash,
              self: {
                backendState: (.BackendState? // null),
                online: (.Self.Online? // null),
                authenticated: (.Self.Authenticated? // null)
              },
              selectedPeer: {
                recordHash: $selectedPeerHash,
                peerSelectorHash: $peerHash,
                routeHash: $routeHash,
                online: ($selectedPeer.value.Online? // null),
                relayPresent: (($selectedPeer.value.Relay? // null) != null),
                currentAddressPresent: (($selectedPeer.value.CurAddr? // null) != null),
                tailscaleIpCount: (($selectedPeer.value.TailscaleIPs? // []) | length),
                tagCount: (($selectedPeer.value.Tags? // []) | length)
              }
            }' >"$status_path"; then
        skip "failed to write redacted tailscale status artifact"
    fi

    if ! jq -cn \
            --argjson selectedPeer "$selected_peer" \
            --arg peerHash "$peer_hash" \
            --arg routeHash "$route_hash" \
            --arg tailnetHash "$tailnet_hash" \
            --arg selectedPeerHash "$selected_peer_hash" \
            '{
              schema: "ee.tailscale.retained_peer.v1",
              redacted: true,
              tailnetHash: $tailnetHash,
              selectedPeer: {
                recordHash: $selectedPeerHash,
                peerSelectorHash: $peerHash,
                routeHash: $routeHash,
                online: ($selectedPeer.value.Online? // null),
                relayPresent: (($selectedPeer.value.Relay? // null) != null),
                currentAddressPresent: (($selectedPeer.value.CurAddr? // null) != null),
                tailscaleIpCount: (($selectedPeer.value.TailscaleIPs? // []) | length),
                tagCount: (($selectedPeer.value.Tags? // []) | length)
              }
            }' >"$peer_path"; then
        skip "failed to write redacted selected-peer artifact"
    fi
}

# Always-run cleanup. Per the bead's foreground sync supervisor wiring,
# `ee mesh disable --dry-run` is the safe lever to flip a half-finished
# sync back off without mutating durable peer state.
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
                --reason "mesh_sync_once_real_tailscale cleanup trap" \
                --json >"$disable_out" 2>/dev/null; then
            emit_event "cleanup" "passed" "mesh disable cleanup completed" \
                "$(jq -cn --arg outHash "$(json_hash "$(cat "$disable_out")")" '{disableOutputHash: $outHash}')"
        else
            emit_event "cleanup" "failed" "mesh disable cleanup failed" '{}'
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
require_tailnet_artifact_dir_opt_in

PEER_SELECTOR="${EE_REAL_TAILSCALE_PEER:-}"
if [ -z "$PEER_SELECTOR" ]; then
    skip "set EE_REAL_TAILSCALE_PEER to a peer node key, MagicDNS name, host name, or Tailscale IP"
fi

EE_BINARY="$(resolve_ee_binary)"
if [ ! -x "$EE_BINARY" ]; then
    skip "set EE_BINARY to an executable ee binary; this harness never runs cargo"
fi

TAILSCALE_STATUS="$ARTIFACT_DIR/tailscale_status.json"
if ! TAILSCALE_STATUS_RAW="$(tailscale status --json)"; then
    skip "tailscale status --json failed; authenticate tailscaled before running this smoke"
fi
assert_json_text "$TAILSCALE_STATUS_RAW" "tailscale_status_json"

if ! printf '%s\n' "$TAILSCALE_STATUS_RAW" | jq -e '(.Self? // null) != null and (((.BackendState? // "") == "Running") or (.Self.Online? == true))' >/dev/null; then
    skip "tailscale status did not include an authenticated local Self node"
fi

PEER_JSON="$ARTIFACT_DIR/peer.json"
if ! PEER_ENTRY_RAW="$(select_peer_entry "$TAILSCALE_STATUS_RAW" "$PEER_SELECTOR")"; then
    skip "failed to inspect peer list from tailscale status"
fi
if [ -z "$PEER_ENTRY_RAW" ] || ! printf '%s\n' "$PEER_ENTRY_RAW" | jq -e '.value' >/dev/null; then
    skip "requested peer was not visible in tailscale status --json"
fi

PEER_HASH="$(json_hash "$PEER_SELECTOR")"
ROUTE_HINT="$(printf '%s\n' "$PEER_ENTRY_RAW" | jq -r '.value.Relay? // .value.CurAddr? // "unknown"')"
ROUTE_HASH="$(json_hash "$ROUTE_HINT")"
TAILNET_ID="$(printf '%s\n' "$TAILSCALE_STATUS_RAW" | jq -r '.CurrentTailnet.Name? // .CurrentTailnet.MagicDNSSuffix? // .Self.Tailnet? // .Self.TailnetName? // "unknown"')"
TAILNET_HASH="$(json_hash "$TAILNET_ID")"
SELECTED_PEER_HASH="$(json_hash "$PEER_ENTRY_RAW")"
write_redacted_tailnet_artifacts "$TAILSCALE_STATUS_RAW" "$PEER_ENTRY_RAW" "$PEER_HASH" "$ROUTE_HASH" "$TAILNET_HASH" "$SELECTED_PEER_HASH" "$TAILSCALE_STATUS" "$PEER_JSON"
assert_json_file "$TAILSCALE_STATUS" "redacted_tailscale_status_json"
assert_json_file "$PEER_JSON" "redacted_peer_json"
assert_jq "$TAILSCALE_STATUS" '.redacted == true and (.Peer? | not) and (.tailnetHash | type == "string") and (.selectedPeer.recordHash | type == "string")' "redacted_tailscale_status"
assert_jq "$PEER_JSON" '.redacted == true and (.value? | not) and (.selectedPeer.recordHash | type == "string")' "redacted_peer_json"
emit_event "precondition" "passed" "real tailnet peer is visible for sync-once" \
    "$(jq -cn \
        --arg peerHash "$PEER_HASH" \
        --arg routeHash "$ROUTE_HASH" \
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
  "cleanup_policy": "retained_by_mesh_sync_once_real_tailscale_smoke"
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

# Scenario A — `ee mesh status --json` against the live tailnet. Establishes
# the baseline mesh posture so we can compare degraded codes before/after a
# foreground sync tick.
STATUS_PRE_JSON="$(run_ee_json mesh_status_pre mesh status)"
assert_jq "$STATUS_PRE_JSON" '.success == true or (.schema // "" | startswith("ee.mesh.status"))' "mesh_status_pre_success"

# Scenario B — `ee mesh sync --once --json` is the bead's primary surface.
# bd-36bbk.2 evidence says this currently always emits
# mesh_sync_once_network_deferred with contactedPeers=false. The smoke
# captures the live output so an operator with a real tailnet can confirm
# whether the SRR6.7 + SRR6.9 primitives now actually contact a peer
# (degraded array empty / contactedPeers > 0) or still defer.
SYNC_ONCE_JSON="$(run_ee_json mesh_sync_once mesh sync --once)"
assert_jq "$SYNC_ONCE_JSON" '.success == true or (.schema // "" | startswith("ee.mesh.sync"))' "mesh_sync_once_success"

# Capture deferred-vs-contacted posture as audit-friendly fields. The bead's
# acceptance specifies degraded code `mesh_sync_once_network_deferred` for
# the offline path; the contacted path should NOT include that code and
# SHOULD show `contactedPeers >= 1`. Both signals are emitted so a future
# regression in either direction is readable from the event log.
#
# Default opt-in still records both signals (local Tailscale observation
# without a second `ee` is not a failed transport). Set
# EE_E2E_REAL_TAILSCALE_REQUIRE_CONTACT=1 to assert a live EE-to-EE round
# (T2.6 / bd-tc-epic-qzk7o.3.8).
DEFERRED_PRESENT="$(jq -r '
    [
      (.data.degraded // [])[]?.code,
      (.degraded // [])[]?.code
    ]
    | any(. == "mesh_sync_once_network_deferred")
' "$SYNC_ONCE_JSON")"
CONTACTED_PEERS="$(mesh_contacted_peer_count "$SYNC_ONCE_JSON")"
if [ "${EE_E2E_REAL_TAILSCALE_REQUIRE_CONTACT:-}" = "1" ]; then
    if [ "$DEFERRED_PRESENT" = "true" ] || [ "${CONTACTED_PEERS:-0}" = "0" ]; then
        fail "assert" "real EE-to-EE contact required: deferred=${DEFERRED_PRESENT} contactedPeers=${CONTACTED_PEERS}"
    fi
fi

# Scenario C — `ee mesh status --json` after the sync tick. Baseline
# diff-friendly snapshot.
STATUS_POST_JSON="$(run_ee_json mesh_status_post mesh status)"
assert_jq "$STATUS_POST_JSON" '.success == true or (.schema // "" | startswith("ee.mesh.status"))' "mesh_status_post_success"

SYNC_MS="$(jq -r 'select(.fields.commandLabel? == "mesh_sync_once") | .fields.elapsedMs // empty' "$EVENT_FILE" | tail -1)"
emit_event "assert" "passed" "real Tailscale mesh sync --once smoke surfaces completed" \
    "$(jq -cn \
        --arg peerNodeHash "$PEER_HASH" \
        --arg routeHash "$(json_hash "$ROUTE_HINT")" \
        --arg tailnetHash "$TAILNET_HASH" \
        --arg syncMs "${SYNC_MS:-0}" \
        --arg deferredPresent "$DEFERRED_PRESENT" \
        --arg contactedPeers "$CONTACTED_PEERS" \
        '{
          peerNodeHash: $peerNodeHash,
          routeHash: $routeHash,
          tailnetHash: $tailnetHash,
          syncOnceMs: ($syncMs | tonumber),
          meshSyncOnceNetworkDeferred: ($deferredPresent == "true"),
          contactedPeers: ($contactedPeers | tonumber)
        }')"

emit_event "cleanup" "retained" "real Tailscale mesh sync --once smoke artifacts retained" \
    "$(jq -cn --arg manifestHash "$(json_hash "$(cat "$MANIFEST")")" '{manifestHash: $manifestHash}')"
printf '%s artifacts retained: %s\n' "$SCENARIO" "$ARTIFACT_DIR" >&2
printf '%s\n' "$EVENT_FILE"
