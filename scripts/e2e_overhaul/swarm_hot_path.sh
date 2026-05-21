#!/usr/bin/env bash
# O5 - No-mock swarm hot-path e2e with cache prewarm, burst admission,
# and latency summary logging.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
if ! command -v python3 >/dev/null 2>&1; then
    echo "o5: python3 is required for swarm hot-path aggregation" >&2
    exit 2
fi

epic_setup "swarm_hot_path"
ee_global profile config apply \
    --workspace "$EPIC_WORKSPACE" \
    --profile swarm \
    --json >/dev/null 2>&1 || true

ARTIFACT_DIR="$EPIC_WORKSPACE/swarm-hot-path-artifacts"
mkdir -p "$ARTIFACT_DIR"
READERS_TSV="$ARTIFACT_DIR/readers.tsv"
METRICS_JSONL="$ARTIFACT_DIR/reader-metrics.jsonl"
SUMMARY_JSON="$ARTIFACT_DIR/summary.json"
HOTSET_JSON="$ARTIFACT_DIR/hotset.json"
IMPORT_DIR="$ARTIFACT_DIR/import"
IMPORT_JSONL="$IMPORT_DIR/memories.jsonl"
QUERY="O5 swarm hot path cache prewarm burst admission deterministic context"
CURRENT_GENERATION=5
TIER="small"
MEMORY_COUNT="${SWARM_HOT_PATH_MEMORY_COUNT:-24}"
SEARCH_FANOUT="${SWARM_HOT_PATH_SEARCH_FANOUT:-2}"
LEAN_CONTEXT_FANOUT="${SWARM_HOT_PATH_LEAN_CONTEXT_FANOUT:-2}"
STANDARD_CONTEXT_FANOUT="${SWARM_HOT_PATH_STANDARD_CONTEXT_FANOUT:-1}"
DETERMINISM_RUNS="${SWARM_HOT_PATH_DETERMINISM_RUNS:-3}"

if [ "${SWARM_HOT_PATH_LARGE:-0}" = "1" ]; then
    TIER="large"
    MEMORY_COUNT="${SWARM_HOT_PATH_MEMORY_COUNT:-10000}"
    SEARCH_FANOUT="${SWARM_HOT_PATH_SEARCH_FANOUT:-8}"
    LEAN_CONTEXT_FANOUT="${SWARM_HOT_PATH_LEAN_CONTEXT_FANOUT:-4}"
    STANDARD_CONTEXT_FANOUT="${SWARM_HOT_PATH_STANDARD_CONTEXT_FANOUT:-4}"
fi

: > "$READERS_TSV"
: > "$METRICS_JSONL"

now_ns() {
    python3 -c 'from time import time_ns; print(time_ns())'
}

sha256_file() {
    shasum -a 256 "$1" | awk '{print $1}'
}

seed_swarm_hot_path_corpus() {
    mkdir -p "$IMPORT_DIR"
    printf '{"schema":"ee.export.header.v1","format_version":1,"created_at":"2026-05-21T00:00:00Z","workspace_id":"ws_o5_swarm_hot_path","workspace_path":"/o5/swarm-hot-path","export_scope":"memories","redaction_level":"standard","record_count":%s,"ee_version":"o5-fixture","hostname":null,"export_id":"exp_o5_swarm_hot_path","import_source":"native","trust_level":"validated","checksum":null,"signature":null,"source_schema_version":null}\n' \
        "$MEMORY_COUNT" > "$IMPORT_JSONL"
    local index memory_id
    for index in $(seq 1 "$MEMORY_COUNT"); do
        memory_id="$(printf 'mem_o5_%022d' "$index")"
        printf '{"schema":"ee.export.memory.v1","memory_id":"%s","workspace_id":"ws_o5_swarm_hot_path","level":"procedural","kind":"fact","content":"O5 swarm hot path memory %s: cache prewarm, burst admission, search, context, provenance, and redaction-safe summaries stay deterministic under reader fanout.","importance":0.8,"confidence":0.8,"utility":0.8,"created_at":"2026-05-21T00:00:01Z","updated_at":null,"tombstoned_at":null,"tombstoned_reason":null,"valid_from":null,"valid_to":null,"expires_at":null,"source_agent":"o5-swarm-hot-path","provenance_uri":"ee-export://o5/swarm-hot-path/%s","superseded_by":null,"supersedes":null,"redacted":false,"redaction_reason":null}\n' \
            "$memory_id" "$index" "$index" >> "$IMPORT_JSONL"
    done
    ee_workspace import jsonl --source "$IMPORT_JSONL" --json >/dev/null 2>&1 || true
    _e2e_emit_event "swarm_hot_path_seeded" \
        "memory_count" "$MEMORY_COUNT" \
        "import_jsonl" "$IMPORT_JSONL"
}

write_hotset_manifest() {
    python3 - "$HOTSET_JSON" "$CURRENT_GENERATION" <<'PY'
import json
import sys

path = sys.argv[1]
generation = int(sys.argv[2])
manifest = {
    "schema": "ee.cache.hotset.v1",
    "workspaceId": "ws_o5_swarm_hot_path",
    "workspaceGeneration": generation,
    "indexGeneration": generation,
    "admissionThreshold": generation,
    "profileTier": "standard",
    "redactionStatus": "content_not_stored",
    "candidateCount": 5,
    "admittedCount": 5,
    "rejectedStaleCount": 0,
    "memoryBudget": {
        "maxEntries": 128,
        "maxBytes": 8388608,
        "currentEntries": 5,
        "currentBytes": 1536,
    },
    "searchEntries": [
        {
            "key": "mem_o5_0000000000000000000001",
            "kind": "memory",
            "generation": generation,
            "estimatedBytes": 384,
            "hitCount": 5,
            "redactionStatus": "content_not_stored",
        },
        {
            "key": "query_shape:o5-swarm-hot-path",
            "kind": "query_shape",
            "generation": generation,
            "estimatedBytes": 512,
            "hitCount": 4,
            "redactionStatus": "content_not_stored",
        },
        {
            "key": "search_document:o5-cache-prewarm",
            "kind": "search_document",
            "generation": generation,
            "estimatedBytes": 384,
            "hitCount": 3,
            "redactionStatus": "content_not_stored",
        },
    ],
    "packEntries": [
        {
            "key": "pack:section:procedural_rules:o5",
            "kind": "pack_section",
            "section": "procedural_rules",
            "generation": generation,
            "estimatedBytes": 512,
            "hitCount": 4,
            "redactionStatus": "content_not_stored",
        },
        {
            "key": "pack:audit:o5",
            "kind": "selection_audit",
            "section": None,
            "generation": generation,
            "estimatedBytes": 256,
            "hitCount": 2,
            "redactionStatus": "content_not_stored",
        },
    ],
    "rejectedStaleSearchEntries": [],
    "rejectedStalePackEntries": [],
    "degraded": [],
}
with open(path, "w", encoding="utf-8") as handle:
    json.dump(manifest, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
    _e2e_emit_event "swarm_hot_path_hotset_manifest" \
        "path" "$HOTSET_JSON" \
        "generation" "$CURRENT_GENERATION"
}

hold_lean_pack_slot() {
    local slot_dir slot_path ready_path holder_pid
    slot_dir="$EPIC_WORKSPACE/.ee/pack-slots"
    slot_path="$slot_dir/lean-00.lock"
    ready_path="$slot_dir/lean-holder-ready-$$"
    mkdir -p "$slot_dir"

    python3 - "$slot_path" "$ready_path" >/dev/null 2>&1 <<'PY' &
import fcntl
import pathlib
import sys
import time

slot_path = sys.argv[1]
ready_path = pathlib.Path(sys.argv[2])
with open(slot_path, "a+", encoding="utf-8") as handle:
    fcntl.flock(handle, fcntl.LOCK_EX)
    ready_path.write_text("ready\n", encoding="utf-8")
    time.sleep(30)
PY
    holder_pid=$!
    for _attempt in $(seq 1 100); do
        if [ -f "$ready_path" ]; then
            printf '%s\n' "$holder_pid"
            return 0
        fi
        sleep 0.05
    done
    kill "$holder_pid" 2>/dev/null || true
    wait "$holder_pid" 2>/dev/null || true
    printf '\n'
    return 1
}

release_pack_slot_holder() {
    local holder_pid="${1:-}"
    if [ -n "$holder_pid" ]; then
        kill "$holder_pid" 2>/dev/null || true
        wait "$holder_pid" 2>/dev/null || true
    fi
}

spawn_reader() {
    local label="${1:?label required}"
    local kind="${2:?kind required}"
    local profile="${3:-}"
    local out_file="$ARTIFACT_DIR/$label.out.json"
    local err_file="$ARTIFACT_DIR/$label.err"
    local start_ns pid
    start_ns="$(now_ns)"
    case "$kind" in
        search)
            "$EE_BINARY" search "$QUERY" \
                --workspace "$EPIC_WORKSPACE" \
                --relevance-floor 0.0 \
                --json >"$out_file" 2>"$err_file" &
            ;;
        context)
            "$EE_BINARY" context "$QUERY" \
                --workspace "$EPIC_WORKSPACE" \
                --resource-profile "$profile" \
                --candidate-pool 24 \
                --json >"$out_file" 2>"$err_file" &
            ;;
        *)
            echo "o5: unknown reader kind: $kind" >&2
            exit 2
            ;;
    esac
    pid=$!
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$label" "$kind" "$profile" "$out_file" "$err_file" "$start_ns:$pid" >> "$READERS_TSV"
    _e2e_emit_event "swarm_hot_path_reader_spawned" \
        "label" "$label" \
        "kind" "$kind" \
        "profile" "$profile" \
        "pid" "$pid"
}

record_reader_result() {
    local label="${1:?label required}"
    local kind="${2:?kind required}"
    local profile="${3:-}"
    local out_file="${4:?stdout file required}"
    local err_file="${5:?stderr file required}"
    local start_pid="${6:?start/pid required}"
    local start_ns="${start_pid%%:*}"
    local pid="${start_pid#*:}"
    local status end_ns elapsed_ms
    if wait "$pid"; then
        status=0
    else
        status=$?
    fi
    end_ns="$(now_ns)"
    elapsed_ms=$(( (end_ns - start_ns) / 1000000 ))
    python3 - "$label" "$kind" "$profile" "$status" "$elapsed_ms" "$out_file" "$err_file" "$METRICS_JSONL" <<'PY'
import hashlib
import json
import sys

label, kind, profile, status, elapsed_ms, out_file, err_file, metrics_path = sys.argv[1:]
stdout = open(out_file, "rb").read() if out_file else b""
stderr = open(err_file, "rb").read() if err_file else b""
try:
    payload = json.loads(stdout.decode("utf-8")) if stdout else {}
except Exception:
    payload = {}
codes = []
if isinstance(payload, dict):
    error = payload.get("error") or {}
    if isinstance(error, dict) and error.get("code"):
        codes.append(str(error["code"]))
    data = payload.get("data") or {}
    if isinstance(data, dict):
        for entry in data.get("degraded") or []:
            if isinstance(entry, dict) and entry.get("code"):
                codes.append(str(entry["code"]))
        pack = data.get("pack") or {}
        slo = pack.get("slo") if isinstance(pack, dict) else {}
        if isinstance(slo, dict):
            for entry in slo.get("degradations") or []:
                if isinstance(entry, dict) and entry.get("code"):
                    codes.append(str(entry["code"]))
success = bool(payload.get("success")) if isinstance(payload, dict) else False
record = {
    "label": label,
    "kind": kind,
    "profile": profile or None,
    "exitCode": int(status),
    "success": success,
    "elapsedMs": int(elapsed_ms),
    "stdoutPath": out_file,
    "stderrPath": err_file,
    "stdoutHash": "sha256:" + hashlib.sha256(stdout).hexdigest(),
    "stderrBytes": len(stderr),
    "codes": sorted(set(codes)),
}
with open(metrics_path, "a", encoding="utf-8") as handle:
    handle.write(json.dumps(record, sort_keys=True) + "\n")
PY
    _e2e_emit_event "swarm_hot_path_reader_finished" \
        "label" "$label" \
        "kind" "$kind" \
        "profile" "$profile" \
        "exit_code" "$status" \
        "elapsed_ms" "$elapsed_ms"
}

scrubbed_signature() {
    local json="${1:?json required}"
    jq -S -c '{
        success: (.success // false),
        searchCount: (.data.results | length // 0),
        contextStatus: (.data.pack.slo.status // null),
        degradedCodes: ([.data.degraded[]?.code, .data.pack.slo.degradations[]?.code] | sort),
        admittedTotal: (.data.admitted.totalEntries // null)
    }' <<< "$json" | shasum -a 256 | awk '{print $1}'
}

run_determinism_probe() {
    local previous="" current run search_json context_json combined
    for run in $(seq 1 "$DETERMINISM_RUNS"); do
        search_json=$("$EE_BINARY" search "$QUERY" \
            --workspace "$EPIC_WORKSPACE" \
            --relevance-floor 0.0 \
            --json 2>/dev/null || true)
        context_json=$("$EE_BINARY" context "$QUERY" \
            --workspace "$EPIC_WORKSPACE" \
            --resource-profile standard \
            --candidate-pool 24 \
            --json 2>/dev/null || true)
        combined="$(printf '%s\n%s\n' "$(scrubbed_signature "$search_json")" "$(scrubbed_signature "$context_json")" | shasum -a 256 | awk '{print $1}')"
        if [ -n "$previous" ]; then
            e2e_log_assert_eq "$combined" "$previous" "o5_determinism_run_${run}"
        fi
        previous="$combined"
        current="$combined"
    done
    printf '%s\n' "$current"
}

write_summary() {
    python3 - "$SUMMARY_JSON" "$METRICS_JSONL" "$PREWARM_JSON_PATH" "$DETERMINISM_HASH" \
        "$DETERMINISM_RUNS" "$TIER" "$MEMORY_COUNT" "$SEARCH_FANOUT" "$LEAN_CONTEXT_FANOUT" \
        "$STANDARD_CONTEXT_FANOUT" <<'PY'
import json
import math
import sys

(
    summary_path,
    metrics_path,
    prewarm_path,
    determinism_hash,
    determinism_runs,
    tier,
    memory_count,
    search_fanout,
    lean_context_fanout,
    standard_context_fanout,
) = sys.argv[1:]

metrics = []
with open(metrics_path, encoding="utf-8") as handle:
    for line in handle:
        line = line.strip()
        if line:
            metrics.append(json.loads(line))
latencies = sorted(record["elapsedMs"] for record in metrics)

def percentile(p):
    if not latencies:
        return 0
    index = max(0, min(len(latencies) - 1, math.ceil((p / 100) * len(latencies)) - 1))
    return latencies[index]

degraded_counts = {}
queue_depth_max = 0
for record in metrics:
    for code in record["codes"]:
        degraded_counts[code] = degraded_counts.get(code, 0) + 1
    try:
        payload = json.load(open(record["stdoutPath"], encoding="utf-8"))
    except Exception:
        payload = {}
    slo = (((payload.get("data") or {}).get("pack") or {}).get("slo") or {})
    admission = slo.get("admission") or {}
    queue_depth_max = max(queue_depth_max, int(admission.get("queueDepth") or 0))

with open(prewarm_path, encoding="utf-8") as handle:
    prewarm = json.load(handle)
data = prewarm.get("data") or {}
summary = {
    "schema": "ee.e2e.swarm_hot_path.v1",
    "tier": tier,
    "memoryCount": int(memory_count),
    "fanout": {
        "search": int(search_fanout),
        "leanContext": int(lean_context_fanout),
        "standardContext": int(standard_context_fanout),
        "total": len(metrics),
    },
    "latencyMs": {
        "p50": percentile(50),
        "p95": percentile(95),
        "p99": percentile(99),
    },
    "cacheAdmission": {
        "totalEntries": int(((data.get("admitted") or {}).get("totalEntries")) or 0),
        "degradedCodes": [entry.get("code") for entry in data.get("degraded", []) if entry.get("code")],
    },
    "queueDepthMax": queue_depth_max,
    "degradedCodeCounts": dict(sorted(degraded_counts.items())),
    "responseHashes": [record["stdoutHash"] for record in metrics],
    "determinism": {
        "runs": int(determinism_runs),
        "scrubbedHash": "sha256:" + determinism_hash,
    },
}
with open(summary_path, "w", encoding="utf-8") as handle:
    json.dump(summary, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
}

seed_swarm_hot_path_corpus
write_hotset_manifest

PREWARM_JSON_PATH="$ARTIFACT_DIR/prewarm.json"
PREWARM_JSON=$(ee_workspace cache prewarm \
    --from-hotset "$HOTSET_JSON" \
    --profile standard \
    --current-generation "$CURRENT_GENERATION" \
    --json 2>"$ARTIFACT_DIR/prewarm.err" || true)
printf '%s\n' "$PREWARM_JSON" > "$PREWARM_JSON_PATH"
assert_jq "$PREWARM_JSON" '.success // false' "true" "o5_cache_prewarm_success"
assert_jq "$PREWARM_JSON" '.data.schema // empty' "ee.cache.prewarm.v1" "o5_cache_prewarm_schema"
assert_jq "$PREWARM_JSON" '(.data.admitted.totalEntries // 0) > 0' "true" "o5_cache_prewarm_admitted"
assert_jq "$PREWARM_JSON" '([.data.degraded[]?.code] | index("hotset_prewarm_no_signals") == null)' \
    "true" "o5_cache_prewarm_has_signal"
case "$PREWARM_JSON" in
    *"swarm hot path memory"*)
        e2e_log_assert_eq "prewarm leaked raw content" "redaction safe prewarm" \
            "o5_cache_prewarm_redaction_safe"
        ;;
    *)
        e2e_log_assert_eq "redaction safe prewarm" "redaction safe prewarm" \
            "o5_cache_prewarm_redaction_safe"
        ;;
esac

LEAN_SLOT_HOLDER_PID="$(hold_lean_pack_slot)"
for index in $(seq 1 "$SEARCH_FANOUT"); do
    spawn_reader "search_$index" "search" ""
done
for index in $(seq 1 "$LEAN_CONTEXT_FANOUT"); do
    spawn_reader "context_lean_$index" "context" "lean"
done
for index in $(seq 1 "$STANDARD_CONTEXT_FANOUT"); do
    spawn_reader "context_standard_$index" "context" "standard"
done

while IFS="$(printf '\t')" read -r label kind profile out_file err_file start_pid; do
    [ -n "$label" ] || continue
    record_reader_result "$label" "$kind" "$profile" "$out_file" "$err_file" "$start_pid"
done < "$READERS_TSV"
release_pack_slot_holder "$LEAN_SLOT_HOLDER_PID"

assert_jq "$(jq -s '{records: .}' "$METRICS_JSONL")" \
    '(.records | all(.exitCode == 0 or (.codes | index("pack_concurrent_limit_reached") != null)))' \
    "true" "o5_readers_success_or_structured_backoff"
assert_jq "$(jq -s '{records: .}' "$METRICS_JSONL")" \
    '(.records | any(.kind == "context" and (.codes | index("pack_concurrent_limit_reached") != null)))' \
    "true" "o5_context_structured_backoff_seen"

DETERMINISM_HASH="$(run_determinism_probe)"
write_summary

assert_jq "$(cat "$SUMMARY_JSON")" '.schema' "ee.e2e.swarm_hot_path.v1" "o5_summary_schema"
assert_jq "$(cat "$SUMMARY_JSON")" '(.latencyMs.p95 >= .latencyMs.p50)' "true" "o5_latency_p95_ge_p50"
assert_jq "$(cat "$SUMMARY_JSON")" '(.latencyMs.p99 >= .latencyMs.p95)' "true" "o5_latency_p99_ge_p95"
assert_jq "$(cat "$SUMMARY_JSON")" '(.cacheAdmission.totalEntries > 0)' "true" "o5_summary_cache_admitted"
assert_jq "$(cat "$SUMMARY_JSON")" '(.queueDepthMax >= 1)' "true" "o5_summary_queue_depth"
_e2e_emit_event "swarm_hot_path_summary" \
    "summary_json" "$SUMMARY_JSON" \
    "tier" "$TIER" \
    "memory_count" "$MEMORY_COUNT" \
    "reader_metrics_jsonl" "$METRICS_JSONL" \
    "determinism_hash" "$DETERMINISM_HASH"
