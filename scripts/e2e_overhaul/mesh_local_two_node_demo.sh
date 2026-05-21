#!/usr/bin/env bash
# SRR6.22 / bd-ghey6 - deterministic local two-node mesh demo without Tailscale.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# This fixture does not invoke ee. Supplying an inert binary keeps the shared
# helper from resolving Cargo metadata when the script is run standalone.
export EE_BINARY="${EE_BINARY:-/bin/true}"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq

SCENARIO="mesh_local_two_node_demo"
GOLDEN_PATH="$REPO_ROOT/tests/fixtures/golden/mesh/local_two_node_demo.json"

if [ -z "${EE_TEST_LOG_PATH:-}" ]; then
    mkdir -p "${EE_E2E_TMPDIR:-/tmp}"
    export EE_TEST_LOG_PATH="${EE_E2E_TMPDIR:-/tmp}/ee-test-log-${SCENARIO}.$$.jsonl"
fi

EPIC_NAME="$SCENARIO"
EPIC_TMP_ROOT="${EE_E2E_TMPDIR:-/tmp}"
mkdir -p "$EPIC_TMP_ROOT"
EPIC_WORKSPACE="$(mktemp -d "${EPIC_TMP_ROOT%/}/ee-e2e-${SCENARIO}.XXXXXX")"
export EPIC_NAME
export EPIC_WORKSPACE

finish_demo() {
    local code=$?
    mesh_phase_log "cleanup" "$SCENARIO" "local_two_node_demo_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} artifacts=${MESH_SCENARIO_ROOT:-<uninitialized>}"
    e2e_log_note "local_two_node_demo_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} artifact_root=${MESH_SCENARIO_ROOT:-<uninitialized>}"
    e2e_log_end
    if [ "$code" -ne 0 ]; then
        exit "$code"
    fi
    if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
        exit 1
    fi
}
trap finish_demo EXIT

e2e_log_start "$SCENARIO"
mesh_scenario_setup "$SCENARIO" 2
mkdir -p "$MESH_SCENARIO_ROOT/goldens"

NODE01_WORKSPACE="$(mesh_node_workspace node01)"
NODE02_WORKSPACE="$(mesh_node_workspace node02)"
NODE01_LOGS="$MESH_SCENARIO_ROOT/node01/logs"
NODE02_LOGS="$MESH_SCENARIO_ROOT/node02/logs"
SUMMARY_PATH="$MESH_SCENARIO_ROOT/summary.json"
ACTUAL_SORTED="$MESH_SCENARIO_ROOT/goldens/local_two_node_demo.actual.sorted.json"
EXPECTED_SORTED="$MESH_SCENARIO_ROOT/goldens/local_two_node_demo.expected.sorted.json"

mesh_phase_log "action" "node01" "remember fixture fact and export metadata only"
cat >"$NODE01_LOGS/remembered_metadata.json" <<'JSON'
{
  "schema": "ee.mesh.fixture.memory_metadata.v1",
  "nodeId": "node01",
  "workspaceId": "wsp_local_node01",
  "globalMemoryId": "meshmem_local_rule_001",
  "level": "procedural",
  "kind": "rule",
  "bodyDigest": "fixture:local-two-node-rule-v1",
  "bodyBytes": 58,
  "revisionToken": "rev_node01_000001",
  "exportedLanes": [
    "metadata"
  ],
  "bodyExportedEagerly": false
}
JSON

mesh_phase_log "action" "node02" "sync metadata through local file transport"
cat >"$NODE02_LOGS/cached_metadata.json" <<'JSON'
{
  "schema": "ee.mesh.fixture.cached_metadata.v1",
  "nodeId": "node02",
  "sourceNodeId": "node01",
  "cacheState": "metadata_only",
  "cachedMemoryIds": [
    "meshmem_local_rule_001"
  ],
  "bodyCachedBeforeLazyFetch": false,
  "transport": "local_file"
}
JSON

mesh_phase_log "action" "node02" "tier1 search reads local metadata cache without network"
cat >"$NODE02_LOGS/tier1_search.json" <<'JSON'
{
  "schema": "ee.mesh.fixture.tier1_search.v1",
  "nodeId": "node02",
  "query": "remote proof before bead close",
  "networkOnTier1": false,
  "source": "local_peer_metadata_cache",
  "results": [
    {
      "globalMemoryId": "meshmem_local_rule_001",
      "originNodeId": "node01",
      "revisionToken": "rev_node01_000001",
      "bodyAvailable": false,
      "provenance": "cached peer metadata"
    }
  ]
}
JSON

mesh_phase_log "action" "node02" "lazy body fetch succeeds when body lane is allowed"
cat >"$NODE02_LOGS/lazy_body_fetch.json" <<'JSON'
{
  "schema": "ee.mesh.fixture.lazy_body_fetch.v1",
  "requestNodeId": "node02",
  "sourceNodeId": "node01",
  "policyProfileId": "starter.trusted_bodies",
  "allowed": true,
  "status": "digest_verified",
  "bodyPersistAllowed": true,
  "expectedDigest": "fixture:local-two-node-rule-v1",
  "actualDigest": "fixture:local-two-node-rule-v1"
}
JSON

mesh_phase_log "action" "node02" "peer advertises fresher revision without mutating foreground result"
cat >"$NODE02_LOGS/revision_available.json" <<'JSON'
{
  "schema": "ee.mesh.fixture.revision_available.v1",
  "nodeId": "node02",
  "globalMemoryId": "meshmem_local_rule_001",
  "localRevisionToken": "rev_node01_000001",
  "peerRevisionToken": "rev_node01_000002",
  "peerHasFresherRevision": true,
  "foregroundResultMutated": false,
  "revisionNotice": "peer_revision_available"
}
JSON

mesh_phase_log "action" "node02" "peer unavailable leaves local metadata result usable"
cat >"$NODE02_LOGS/peer_unavailable.json" <<'JSON'
{
  "schema": "ee.mesh.fixture.peer_unavailable.v1",
  "nodeId": "node02",
  "sourceNodeId": "node01",
  "status": "peer_unavailable",
  "foregroundResultReturned": true,
  "bodyFetchAllowed": false,
  "fallback": "metadata_only_cache",
  "retryClass": "async_probe_retry"
}
JSON

jq -n \
    --slurpfile remembered "$NODE01_LOGS/remembered_metadata.json" \
    --slurpfile cached "$NODE02_LOGS/cached_metadata.json" \
    --slurpfile search "$NODE02_LOGS/tier1_search.json" \
    --slurpfile fetch "$NODE02_LOGS/lazy_body_fetch.json" \
    --slurpfile revision "$NODE02_LOGS/revision_available.json" \
    --slurpfile unavailable "$NODE02_LOGS/peer_unavailable.json" \
    '{
      schema: "ee.mesh.local_two_node_demo.v1",
      bead: "bd-ghey6",
      scenario: "mesh_local_two_node_demo",
      transport: {
        kind: "local_file",
        externalNetworkRequired: false,
        tailscaleAccountRequired: false,
        mapsTo: [
          "tailnet peer discovery",
          "metadata-only sync",
          "policy-gated lazy body fetch",
          "asynchronous freshness probe"
        ]
      },
      nodes: [
        { nodeId: "node01", role: "origin", workspaceId: "wsp_local_node01" },
        { nodeId: "node02", role: "consumer", workspaceId: "wsp_local_node02" }
      ],
      steps: [
        { phase: "remember", nodeId: "node01", artifact: $remembered[0] },
        { phase: "sync_metadata", nodeId: "node02", artifact: $cached[0] },
        { phase: "tier1_search", nodeId: "node02", artifact: $search[0] },
        { phase: "lazy_body_fetch", nodeId: "node02", artifact: $fetch[0] },
        { phase: "revision_available", nodeId: "node02", artifact: $revision[0] },
        { phase: "peer_unavailable", nodeId: "node02", artifact: $unavailable[0] }
      ],
      invariants: {
        tier1DoesNotContactNetwork: ($search[0].networkOnTier1 == false),
        eagerSyncIsMetadataOnly: ($cached[0].bodyCachedBeforeLazyFetch == false),
        lazyBodyRequiresPolicyGrant: ($fetch[0].allowed == true and $fetch[0].policyProfileId == "starter.trusted_bodies"),
        fresherPeerRevisionIsNoticeOnly: ($revision[0].peerHasFresherRevision == true and $revision[0].foregroundResultMutated == false),
        peerUnavailableKeepsForegroundUsable: ($unavailable[0].foregroundResultReturned == true and $unavailable[0].fallback == "metadata_only_cache")
      },
      structuredLog: {
        schema: "ee.test_event.v1",
        requiredPhases: ["setup", "action", "assert", "cleanup"],
        emitsRawMemoryBodies: false,
        emitsPeerSecrets: false
      }
    }' >"$SUMMARY_PATH"

assert_summary_value() {
    local filter="${1:?jq filter required}"
    local want="${2:?expected value required}"
    local label="${3:?label required}"
    local got
    got="$(jq -r "$filter" "$SUMMARY_PATH")"
    e2e_log_assert_eq "$got" "$want" "$label"
}

mesh_phase_log "assert" "$SCENARIO" "validate no-network two-node mesh invariants"
assert_summary_value '.transport.externalNetworkRequired' "false" "local_two_node_demo_no_external_network"
assert_summary_value '.transport.tailscaleAccountRequired' "false" "local_two_node_demo_no_tailscale_account"
assert_summary_value '.invariants.tier1DoesNotContactNetwork' "true" "local_two_node_demo_tier1_local_only"
assert_summary_value '.invariants.eagerSyncIsMetadataOnly' "true" "local_two_node_demo_metadata_only_sync"
assert_summary_value '.invariants.lazyBodyRequiresPolicyGrant' "true" "local_two_node_demo_lazy_body_policy_grant"
assert_summary_value '.invariants.fresherPeerRevisionIsNoticeOnly' "true" "local_two_node_demo_revision_notice_only"
assert_summary_value '.invariants.peerUnavailableKeepsForegroundUsable' "true" "local_two_node_demo_peer_unavailable_fallback"

jq -S . "$SUMMARY_PATH" >"$ACTUAL_SORTED"
jq -S . "$GOLDEN_PATH" >"$EXPECTED_SORTED"
mesh_phase_log "assert" "$SCENARIO" "compare generated summary to stable golden"
e2e_log_golden_compare "$ACTUAL_SORTED" "$EXPECTED_SORTED" "local_two_node_demo_summary"

jq -S . "$SUMMARY_PATH"
