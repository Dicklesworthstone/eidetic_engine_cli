#!/usr/bin/env bash
# No-mock hotset prewarm e2e for bd-ty3pl.4.
# Runs real ee commands against a temporary workspace, writes an explicit
# ee.cache.hotset.v1 manifest, and logs ee.test_event.v1 evidence.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$REPO_ROOT/scripts/e2e_overhaul/lib/shared.sh"
require_jq
require_ee_binary

RUN_TMP_ROOT="${EE_HOTSET_PREWARM_TMPDIR:-${TMPDIR:-/tmp}}"
mkdir -p "$RUN_TMP_ROOT"
RUN_TMP_ROOT="$(cd "$RUN_TMP_ROOT" && pwd -P)"
RUN_ROOT="$(mktemp -d "$RUN_TMP_ROOT/ee-hotset-prewarm.XXXXXX")"
WORKSPACE="$RUN_ROOT/workspace"
ARTIFACT_DIR="$RUN_ROOT/artifacts"
EVENT_LOG="$RUN_ROOT/hotset-prewarm-events.jsonl"
LATENCY_SAMPLES="$RUN_ROOT/latencies.txt"
QUERY="hotset prewarm release workflow deterministic reuse"
CURRENT_GENERATION=7
SOURCE_SNAPSHOT_HASH="sha256:unavailable"
MANIFEST_HASH="sha256:unavailable"
WARMED_COUNTS='{"requested":0,"admitted":0,"staleRejected":0}'

mkdir -p "$WORKSPACE" "$ARTIFACT_DIR"
: > "$EVENT_LOG"
: > "$LATENCY_SAMPLES"

now_ns() {
    date +%s%N
}

sha256_file() {
    shasum -a 256 "$1" | awk '{print "sha256:" $1}'
}

sample_summary_json() {
    jq -s -c '
      sort as $v
      | ($v | length) as $n
      | if $n == 0 then
          {sampleCount: 0, p50Ms: null, p99Ms: null}
        else
          def idx($p):
            (((($n * $p) + 99) / 100 | floor) - 1)
            | if . < 0 then 0 elif . >= $n then ($n - 1) else . end;
          {sampleCount: $n, p50Ms: $v[idx(50)], p99Ms: $v[idx(99)]}
        end
    ' "$LATENCY_SAMPLES"
}

emit_event() {
    local kind="${1:?kind required}"
    local command="${2:?command required}"
    local elapsed_ms="${3:?elapsed required}"
    local stdout_path="${4:?stdout path required}"
    local stderr_path="${5:?stderr path required}"
    local first_failure="${6:-}"
    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg kind "$kind" \
        --arg command "$command" \
        --arg workspace "[WORKSPACE]" \
        --arg source_snapshot_hash "$SOURCE_SNAPSHOT_HASH" \
        --arg manifest_hash "$MANIFEST_HASH" \
        --arg stdout_path "$stdout_path" \
        --arg stderr_path "$stderr_path" \
        --arg redaction_status "paths_hashes_counts_only_no_raw_memory_content" \
        --arg first_failure "$first_failure" \
        --argjson warmed_item_counts "$WARMED_COUNTS" \
        --argjson elapsed_ms "$elapsed_ms" \
        --argjson sample_summary "$(sample_summary_json)" \
        '{
          schema: $schema,
          kind: $kind,
          command: $command,
          workspace: $workspace,
          sourceSnapshotHash: $source_snapshot_hash,
          manifestHash: $manifest_hash,
          warmedItemCounts: $warmed_item_counts,
          elapsedMs: $elapsed_ms,
          sampleSummary: $sample_summary,
          stdoutArtifactPath: $stdout_path,
          stderrArtifactPath: $stderr_path,
          redactionStatus: $redaction_status,
          firstFailureDiagnosis: (if $first_failure == "" then null else $first_failure end)
        }' >> "$EVENT_LOG"
}

run_step() {
    local label="${1:?label required}"
    shift
    local stdout_path="$ARTIFACT_DIR/${label}.stdout.json"
    local stderr_path="$ARTIFACT_DIR/${label}.stderr"
    local start_ns end_ns elapsed_ms status
    start_ns="$(now_ns)"
    set +e
    "$EE_BINARY" "$@" >"$stdout_path" 2>"$stderr_path"
    status=$?
    set -e
    end_ns="$(now_ns)"
    elapsed_ms=$(( (end_ns - start_ns) / 1000000 ))
    printf '%s\n' "$elapsed_ms" >> "$LATENCY_SAMPLES"
    if [ "$status" -eq 0 ]; then
        emit_event "$label" "ee $label" "$elapsed_ms" "$stdout_path" "$stderr_path" ""
    else
        emit_event "$label" "ee $label" "$elapsed_ms" "$stdout_path" "$stderr_path" "command_exit_${status}"
        printf 'hotset prewarm e2e failed at %s; events=%s\n' "$label" "$EVENT_LOG" >&2
        printf 'stdout (%s):\n' "$stdout_path" >&2
        sed -n '1,80p' "$stdout_path" >&2 || true
        printf 'stderr (%s):\n' "$stderr_path" >&2
        sed -n '1,80p' "$stderr_path" >&2 || true
        return "$status"
    fi
    LAST_STDOUT="$stdout_path"
    LAST_STDERR="$stderr_path"
}

scrub_signature() {
    jq -S -c '
      {
        schema,
        success,
        command: (.data.command // null),
        dataSchema: (.data.schema // null),
        resultCount: (if (.data.results | type) == "array" then (.data.results | length) else (.data.resultCount // null) end),
        packHash: (.data.pack.hash // .data.pack.packHash // .data.packHash // null),
        found: (.data.found // null),
        memoryId: (.data.memoryId // .data.memory_id // null),
        degradedCodes: ([.degraded[]?.code, .data.degraded[]?.code, .data.pack.slo.degradations[]?.code] | sort)
      }
    ' "$1" | shasum -a 256 | awk '{print $1}'
}

combined_signature() {
    printf '%s\n%s\n%s\n' "$(scrub_signature "$1")" "$(scrub_signature "$2")" "$(scrub_signature "$3")" |
        shasum -a 256 |
        awk '{print $1}'
}

write_hotset_manifest() {
    local manifest_path="${1:?manifest path required}"
    local memory_id="${2:?memory id required}"
    jq -n \
        --arg workspace_id "ws_hotset_prewarm_e2e" \
        --arg memory_id "$memory_id" \
        --argjson generation "$CURRENT_GENERATION" \
        '{
          schema: "ee.cache.hotset.v1",
          workspaceId: $workspace_id,
          workspaceGeneration: $generation,
          indexGeneration: $generation,
          admissionThreshold: $generation,
          profileTier: "standard",
          redactionStatus: "content_not_stored",
          candidateCount: 4,
          admittedCount: 4,
          rejectedStaleCount: 0,
          memoryBudget: {
            maxEntries: 128,
            maxBytes: 8388608,
            currentEntries: 4,
            currentBytes: 1536
          },
          searchEntries: [
            {
              key: $memory_id,
              kind: "memory",
              generation: $generation,
              estimatedBytes: 384,
              hitCount: 5,
              redactionStatus: "content_not_stored"
            },
            {
              key: "query_shape:hotset-prewarm-release-workflow",
              kind: "query_shape",
              generation: $generation,
              estimatedBytes: 512,
              hitCount: 4,
              redactionStatus: "content_not_stored"
            }
          ],
          packEntries: [
            {
              key: "pack:section:procedural_rules:hotset-prewarm",
              kind: "pack_section",
              section: "procedural_rules",
              generation: $generation,
              estimatedBytes: 512,
              hitCount: 4,
              redactionStatus: "content_not_stored"
            },
            {
              key: "pack:audit:hotset-prewarm",
              kind: "selection_audit",
              section: null,
              generation: $generation,
              estimatedBytes: 256,
              hitCount: 2,
              redactionStatus: "content_not_stored"
            }
          ],
          rejectedStaleSearchEntries: [],
          rejectedStalePackEntries: [],
          degraded: []
        }' > "$manifest_path"
}

run_step init --workspace "$WORKSPACE" --json init
run_step remember_release_rule \
    --workspace "$WORKSPACE" \
    --json \
    remember \
    --level procedural \
    --kind rule \
    --tags hotset,prewarm \
    "Hotset prewarm release workflow rule: repeated search pack and why queries must keep output semantics stable."

MEMORY_ID="$(jq -r '.data.memory_id // .data.memoryId // .data.id // empty' "$LAST_STDOUT")"
if [ -z "$MEMORY_ID" ]; then
    emit_event "remember_memory_id_missing" "jq .data.memory_id" 0 "$LAST_STDOUT" "$LAST_STDERR" "remember_memory_id_missing"
    printf 'hotset prewarm e2e failed: remember memory id missing; events=%s\n' "$EVENT_LOG" >&2
    exit 1
fi

run_step source_status --workspace "$WORKSPACE" --json status
SOURCE_SNAPSHOT_HASH="$(sha256_file "$LAST_STDOUT")"
run_step index_rebuild --workspace "$WORKSPACE" --json index rebuild

run_step search_before --workspace "$WORKSPACE" --json search "$QUERY" --relevance-floor 0.0
SEARCH_BEFORE="$LAST_STDOUT"
run_step pack_before --workspace "$WORKSPACE" --json pack "$QUERY" --max-tokens 2000
PACK_BEFORE="$LAST_STDOUT"
run_step why_before --workspace "$WORKSPACE" --json why "$MEMORY_ID"
WHY_BEFORE="$LAST_STDOUT"
BEFORE_SIGNATURE="$(combined_signature "$SEARCH_BEFORE" "$PACK_BEFORE" "$WHY_BEFORE")"

HOTSET_JSON="$ARTIFACT_DIR/hotset.json"
write_hotset_manifest "$HOTSET_JSON" "$MEMORY_ID"
MANIFEST_HASH="$(sha256_file "$HOTSET_JSON")"

run_step cache_prewarm \
    --workspace "$WORKSPACE" \
    --json \
    cache \
    prewarm \
    --from-hotset "$HOTSET_JSON" \
    --profile standard \
    --current-generation "$CURRENT_GENERATION"

WARMED_COUNTS="$(jq -c '{
    requested: (.data.requested.totalEntries // 0),
    admitted: (.data.admitted.totalEntries // 0),
    staleRejected: (.data.rejected.staleEntries // 0)
}' "$LAST_STDOUT")"

run_step search_after --workspace "$WORKSPACE" --json search "$QUERY" --relevance-floor 0.0
SEARCH_AFTER="$LAST_STDOUT"
run_step pack_after --workspace "$WORKSPACE" --json pack "$QUERY" --max-tokens 2000
PACK_AFTER="$LAST_STDOUT"
run_step why_after --workspace "$WORKSPACE" --json why "$MEMORY_ID"
WHY_AFTER="$LAST_STDOUT"
AFTER_SIGNATURE="$(combined_signature "$SEARCH_AFTER" "$PACK_AFTER" "$WHY_AFTER")"

if [ "$BEFORE_SIGNATURE" != "$AFTER_SIGNATURE" ]; then
    emit_event "semantic_signature_mismatch" "compare scrubbed search pack why signatures" 0 \
        "$ARTIFACT_DIR/pack_after.stdout.json" "$ARTIFACT_DIR/pack_after.stderr" \
        "prewarm_changed_scrubbed_output_semantics"
    printf 'hotset prewarm e2e failed: semantic signature mismatch; events=%s\n' "$EVENT_LOG" >&2
    exit 1
fi

emit_event "hotset_prewarm_summary" "compare scrubbed search pack why signatures" 0 \
    "$ARTIFACT_DIR/pack_after.stdout.json" "$ARTIFACT_DIR/pack_after.stderr" ""

jq -e -s '
  length >= 9
  and all(.schema == "ee.test_event.v1")
  and all(.command and .workspace and .sourceSnapshotHash and .manifestHash)
  and all(.warmedItemCounts and .elapsedMs != null and .sampleSummary)
  and all(.stdoutArtifactPath and .stderrArtifactPath and .redactionStatus)
' "$EVENT_LOG" >/dev/null

if grep -qE 'hunter2|DATABASE_URL|sk-[A-Za-z0-9]' "$EVENT_LOG"; then
    printf 'hotset prewarm e2e failed: event log leaked secret-like marker; events=%s\n' "$EVENT_LOG" >&2
    exit 1
fi

printf 'hotset prewarm e2e passed; events=%s\n' "$EVENT_LOG" >&2
