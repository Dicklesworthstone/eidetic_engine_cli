#!/usr/bin/env bash
# S3 - deterministic coordination snapshot context-pack e2e.
#
# This driver avoids live Agent Mail entirely. It writes a redacted
# ee.coordination_snapshot.v1 fixture into an isolated temp workspace and
# verifies that `ee context` embeds compact coordination posture in JSON and
# rendered Markdown.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DEFAULT_AGENT_BUILD_ROOT="/Volumes/USBNVME16TB/temp_agent_space"

if [ -d "$DEFAULT_AGENT_BUILD_ROOT" ]; then
    mkdir -p "$DEFAULT_AGENT_BUILD_ROOT/cargo-target" "$DEFAULT_AGENT_BUILD_ROOT/tmp" 2>/dev/null || true
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$DEFAULT_AGENT_BUILD_ROOT/cargo-target}"
    export TMPDIR="${EE_AGENT_TMPDIR:-$DEFAULT_AGENT_BUILD_ROOT/tmp}"
fi

if ! command -v jq >/dev/null 2>&1; then
    echo "coordination_snapshot: jq is required" >&2
    exit 2
fi

if [ -z "${EE_BINARY:-}" ]; then
    if [ -n "${CARGO_TARGET_DIR:-}" ] && [ -x "${CARGO_TARGET_DIR%/}/debug/ee" ]; then
        EE_BINARY="${CARGO_TARGET_DIR%/}/debug/ee"
    elif [ -n "${CARGO_TARGET_DIR:-}" ] && [ -x "${CARGO_TARGET_DIR%/}/release/ee" ]; then
        EE_BINARY="${CARGO_TARGET_DIR%/}/release/ee"
    else
        EE_BINARY="$REPO_ROOT/target/debug/ee"
    fi
fi

if [ ! -x "$EE_BINARY" ]; then
    echo "coordination_snapshot: ee binary not executable at $EE_BINARY" >&2
    exit 2
fi

WORKSPACE="$(mktemp -d "${TMPDIR:-/tmp}/ee-e2e-coordination.XXXXXX")"
echo "coordination_snapshot: workspace retained at $WORKSPACE" >&2

"$EE_BINARY" init --workspace "$WORKSPACE" --json >/dev/null
"$EE_BINARY" remember --workspace "$WORKSPACE" \
    --level procedural \
    --kind rule \
    "Coordinate before editing reserved files." \
    --json >/dev/null
"$EE_BINARY" index rebuild --workspace "$WORKSPACE" --json >/dev/null

SNAPSHOT="$WORKSPACE/coordination_snapshot.json"
cat >"$SNAPSHOT" <<'JSON'
{
  "schema": "ee.coordination_snapshot.v1",
  "captured_at": "2026-05-13T10:00:00Z",
  "scope": "workspace",
  "sources": [
    {
      "kind": "beads_ready",
      "source_id": "br ready --json",
      "freshness_ms": 1000,
      "entries": [
        {
          "kind": "bead",
          "id": "bd-1zb7k.4",
          "status": "in_progress",
          "summary": "S3 coordination-aware context packs"
        }
      ]
    },
    {
      "kind": "file_reservation",
      "source_id": "agent-mail snapshot",
      "freshness_ms": 1000,
      "entries": [
        {
          "path_pattern": "src/pack/**",
          "holder": "BlueLake",
          "exclusive": true,
          "conflict": true
        }
      ]
    }
  ]
}
JSON

CONTEXT_JSON="$("$EE_BINARY" context "coordinate shared pack" \
    --workspace "$WORKSPACE" \
    --coordination-snapshot "$SNAPSHOT" \
    --max-tokens 1500 \
    --format json)"

assert_jq() {
    local filter="$1"
    local want="$2"
    local label="$3"
    local got
    got="$(printf '%s' "$CONTEXT_JSON" | jq -r "$filter")"
    if [ "$got" != "$want" ]; then
        echo "coordination_snapshot: assertion failed $label: got '$got', wanted '$want'" >&2
        exit 1
    fi
}

printf '%s' "$CONTEXT_JSON" | jq . >/dev/null
assert_jq '.data.pack.coordination.schema' 'ee.coordination_snapshot.v1' 'schema'
assert_jq '.data.pack.coordination.summary.activeConflictCount' '1' 'active conflict count'
assert_jq '.data.pack.coordination.summary.inProgressBeadCount' '1' 'in-progress bead count'
assert_jq '.data.pack.text | contains("## Coordination")' 'true' 'markdown section'

UNAVAILABLE_JSON="$("$EE_BINARY" context "coordinate shared pack" \
    --workspace "$WORKSPACE" \
    --coordination-snapshot "$REPO_ROOT/tests/fixtures/coordination_snapshots/coordination_source_unavailable.json" \
    --max-tokens 1500 \
    --format json)"

got="$(printf '%s' "$UNAVAILABLE_JSON" | jq -r '.data.pack.coordination.summary.unavailableSourceCount')"
if [ "$got" != "1" ]; then
    echo "coordination_snapshot: unavailable fixture count was '$got', wanted '1'" >&2
    exit 1
fi
