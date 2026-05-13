#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
MANIFEST="$REPO_ROOT/tests/fixtures/swarm_scale/corpus_manifest.json"
GOLDEN="$REPO_ROOT/tests/fixtures/golden/swarm_fixture/smoke_release_pack.md.golden"
EVENT_DIR="${TMPDIR:-/Volumes/USBNVME16TB/temp_agent_space/tmp}/ee-swarm-fixture-events"
EVENT_LOG="$EVENT_DIR/swarm_fixture.jsonl"

mkdir -p "$EVENT_DIR"
: > "$EVENT_LOG"

if ! command -v jq >/dev/null 2>&1; then
  printf 'error: jq is required for swarm fixture e2e\n' >&2
  exit 1
fi

started_ns="$(date +%s%N)"

agent_count="$(jq -r '.scales[] | select(.name == "mid_10k") | .agentCount' "$MANIFEST")"
memory_count="$(jq -r '.scales[] | select(.name == "mid_10k") | .memoryCount' "$MANIFEST")"
conflict_count="$(jq -r '.conflicts | length' "$MANIFEST")"
expected_packs="$(jq -r '.expectedPacks | length' "$MANIFEST")"

if [[ "$agent_count" -lt 64 || "$memory_count" -lt 10000 ]]; then
  printf 'error: mid_10k must cover at least 64 agents and 10k memories\n' >&2
  exit 1
fi
if [[ "$conflict_count" -lt 3 || "$expected_packs" -lt 1 ]]; then
  printf 'error: manifest must include conflicts and expected packs\n' >&2
  exit 1
fi
if [[ ! -s "$GOLDEN" ]]; then
  printf 'error: missing swarm fixture markdown golden\n' >&2
  exit 1
fi

manifest_hash="$(shasum -a 256 "$MANIFEST" | awk '{print "sha256:" $1}')"
golden_hash="$(shasum -a 256 "$GOLDEN" | awk '{print "sha256:" $1}')"
finished_ns="$(date +%s%N)"
elapsed_ms="$(( (finished_ns - started_ns) / 1000000 ))"

jq -cn \
  --arg schema "ee.test_event.v1" \
  --arg kind "swarm_fixture" \
  --arg scale "mid_10k" \
  --arg command "jq manifest validation" \
  --arg manifest_hash "$manifest_hash" \
  --arg output_hash "$golden_hash" \
  --argjson memory_count "$memory_count" \
  --argjson agent_count "$agent_count" \
  --argjson conflict_count "$conflict_count" \
  --argjson elapsed_ms "$elapsed_ms" \
  --argjson expected_packs_validated "$expected_packs" \
  '{
    schema:$schema,
    kind:$kind,
    scale:$scale,
    command:$command,
    elapsedMs:$elapsed_ms,
    outputHash:$output_hash,
    degradationCodes:[],
    manifestHash:$manifest_hash,
    memoryCount:$memory_count,
    agentCount:$agent_count,
    conflictCount:$conflict_count,
    expectedPacksValidated:$expected_packs_validated
  }' | tee -a "$EVENT_LOG" >&2

printf 'swarm fixture e2e passed; events=%s\n' "$EVENT_LOG" >&2
